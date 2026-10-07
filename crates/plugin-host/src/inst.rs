// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-instance data shared between the GTK thread (`host::Instance`), the
//! audio thread (`rt`), and the host callbacks a plugin may call.
//!
//! `Inner` lives in a Box owned by `Instance`; `PluginHandle` is its address.
//! Everything in it is either atomic, audio-thread-only (`rt`, touched through
//! raw pointers), or main-thread-only (`Cell`/`RefCell`, touched only by
//! callbacks that CLAP defines as `[main-thread]`).

use crate::gui::GuiState;
use crate::rt::{RtOutEvent, RtOutKind};
use crate::sources;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::posix_fd_support::*;
use clap_sys::ext::render::*;
use clap_sys::ext::state::*;
use clap_sys::ext::thread_check::*;
use clap_sys::ext::timer_support::*;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::version::CLAP_VERSION;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};

/// Input events per block (params + notes). Extra events are dropped.
pub(crate) const MAX_IN_EVENTS: usize = 2048;
/// Plugin output events per block.
pub(crate) const MAX_OUT_EVENTS: usize = 512;
const LOG_SLOTS: usize = 32;
const LOG_LEN: usize = 240;

thread_local! {
    // const init and no destructor: reading never allocates.
    static IS_MAIN: Cell<bool> = const { Cell::new(false) };
    static AUDIO_OF: Cell<*const Inner> = const { Cell::new(std::ptr::null()) };
}

/// Mark the current thread as the CLAP main thread (the GTK thread).
pub(crate) fn mark_main_thread() {
    IS_MAIN.with(|m| m.set(true));
}

/// Marks the current thread as `inner`'s audio thread until dropped.
pub(crate) struct AudioScope(*const Inner);
impl AudioScope {
    pub fn enter(inner: &Inner) -> AudioScope {
        AudioScope(AUDIO_OF.with(|a| a.replace(inner)))
    }
}
impl Drop for AudioScope {
    fn drop(&mut self) {
        AUDIO_OF.with(|a| a.set(self.0));
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union EvSlot {
    pub header: clap_event_header,
    pub note: clap_event_note,
    pub param: clap_event_param_value,
    pub gesture: clap_event_param_gesture,
}

/// Audio-thread state. Accessed only through raw pointers while a plugin call
/// is running, because the plugin calls back into `in_list`/`out_list`.
pub(crate) struct RtState {
    pub events: Box<[EvSlot]>,
    pub n_events: usize,
    pub dropped: u32,
    pub out: Box<[RtOutEvent]>,
    pub n_out: usize,
    pub in_list: clap_input_events,
    pub out_list: clap_output_events,
    pub in_buf: AudioBuf,
    pub out_buf: AudioBuf,
    pub in_ptrs: [*mut f32; 2],
    pub out_ptrs: [*mut f32; 2],
    pub has_input: bool,
}
pub(crate) type AudioBuf = clap_sys::audio_buffer::clap_audio_buffer;

/// Pointers to the plugin's extensions; null when absent.
#[derive(Clone, Copy)]
pub(crate) struct Exts {
    pub audio_ports: *const clap_plugin_audio_ports,
    #[allow(dead_code)]
    pub note_ports: *const clap_plugin_note_ports,
    pub params: *const clap_plugin_params,
    pub state: *const clap_plugin_state,
    pub gui: *const clap_plugin_gui,
    pub latency: *const clap_plugin_latency,
    pub render: *const clap_plugin_render,
    pub timer: *const clap_plugin_timer_support,
    pub fd: *const clap_plugin_posix_fd_support,
}

impl Exts {
    pub const NONE: Exts = Exts {
        audio_ports: std::ptr::null(),
        note_ports: std::ptr::null(),
        params: std::ptr::null(),
        state: std::ptr::null(),
        gui: std::ptr::null(),
        latency: std::ptr::null(),
        render: std::ptr::null(),
        timer: std::ptr::null(),
        fd: std::ptr::null(),
    };
}

/// Flags set by host callbacks from any thread, consumed by
/// `Instance::poll_main_thread`.
#[derive(Default)]
pub(crate) struct Flags {
    pub callback: AtomicBool,
    pub restart: AtomicBool,
    pub process: AtomicBool,
    pub flush: AtomicBool,
    pub latency: AtomicBool,
    pub dirty: AtomicBool,
    pub gui_show: AtomicBool,
    pub gui_hide: AtomicBool,
    pub gui_closed: AtomicBool,
    pub gui_closed_destroyed: AtomicBool,
    pub gui_hints: AtomicBool,
    pub gui_resize: AtomicBool,
    /// width << 32 | height
    pub gui_size: AtomicU64,
}

#[derive(Default)]
pub(crate) struct Sources {
    pub timers: Vec<(u32, glib::Source)>,
    pub fds: Vec<(i32, glib::Source)>,
    pub next_timer: u32,
}

impl Sources {
    pub fn destroy_all(&mut self) {
        for (_, s) in self.timers.drain(..) {
            s.destroy();
        }
        for (_, s) in self.fds.drain(..) {
            s.destroy();
        }
    }
}

/// Single-producer (audio) single-consumer (main) ring of log lines.
pub(crate) struct LogRing {
    slots: UnsafeCell<[u8; LOG_LEN * LOG_SLOTS]>,
    lens: [AtomicUsize; LOG_SLOTS],
    sev: [AtomicUsize; LOG_SLOTS],
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl LogRing {
    fn new() -> LogRing {
        LogRing {
            slots: UnsafeCell::new([0; LOG_LEN * LOG_SLOTS]),
            lens: std::array::from_fn(|_| AtomicUsize::new(0)),
            sev: std::array::from_fn(|_| AtomicUsize::new(0)),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Audio thread only. Drops the message when full.
    pub fn push(&self, severity: i32, msg: &[u8]) {
        let head = self.head.load(Relaxed);
        if head.wrapping_sub(self.tail.load(Acquire)) >= LOG_SLOTS {
            return;
        }
        let i = head % LOG_SLOTS;
        let n = msg.len().min(LOG_LEN);
        // SAFETY: slot `i` is not visible to the consumer until head is
        // published below; there is a single producer.
        unsafe {
            std::ptr::copy_nonoverlapping(
                msg.as_ptr(),
                (self.slots.get() as *mut u8).add(i * LOG_LEN),
                n,
            )
        };
        self.lens[i].store(n, Relaxed);
        self.sev[i].store(severity as usize, Relaxed);
        self.head.store(head.wrapping_add(1), Release);
    }

    /// Main thread only.
    pub fn drain(&self, mut f: impl FnMut(i32, &str)) {
        loop {
            let tail = self.tail.load(Relaxed);
            if tail == self.head.load(Acquire) {
                return;
            }
            let i = tail % LOG_SLOTS;
            let n = self.lens[i].load(Relaxed);
            // SAFETY: the producer does not rewrite slot `i` until tail moves.
            let bytes = unsafe {
                std::slice::from_raw_parts((self.slots.get() as *const u8).add(i * LOG_LEN), n)
            };
            f(
                self.sev[i].load(Relaxed) as i32,
                &String::from_utf8_lossy(bytes),
            );
            self.tail.store(tail.wrapping_add(1), Release);
        }
    }
}

pub(crate) struct Inner {
    pub plugin: Cell<*const clap_plugin>,
    pub host: clap_host,
    pub exts: Cell<Exts>,
    pub flags: Flags,
    pub active: AtomicBool,
    pub processing: AtomicBool,
    pub rt: UnsafeCell<RtState>,
    pub log: LogRing,
    pub name: String,
    pub sources: RefCell<Sources>,
    pub gui: RefCell<GuiState>,
}

static HOST_NAME: &CStr = c"LibreDAW";

impl Inner {
    /// Allocate with stable address and wire the self-pointers.
    pub fn new(name: String, has_input: bool) -> Box<Inner> {
        let zero_ev = EvSlot {
            header: clap_event_header {
                size: 0,
                time: 0,
                space_id: 0,
                type_: 0,
                flags: 0,
            },
        };
        let null_buf = AudioBuf {
            data32: std::ptr::null_mut(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let mut b = Box::new(Inner {
            plugin: Cell::new(std::ptr::null()),
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: std::ptr::null_mut(),
                name: HOST_NAME.as_ptr(),
                vendor: c"LibreDAW".as_ptr(),
                url: c"".as_ptr(),
                version: c"0.0".as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            exts: Cell::new(Exts::NONE),
            flags: Flags::default(),
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            rt: UnsafeCell::new(RtState {
                events: vec![zero_ev; MAX_IN_EVENTS].into_boxed_slice(),
                n_events: 0,
                dropped: 0,
                out: vec![RtOutEvent::EMPTY; MAX_OUT_EVENTS].into_boxed_slice(),
                n_out: 0,
                in_list: clap_input_events {
                    ctx: std::ptr::null_mut(),
                    size: Some(in_size),
                    get: Some(in_get),
                },
                out_list: clap_output_events {
                    ctx: std::ptr::null_mut(),
                    try_push: Some(out_push),
                },
                in_buf: null_buf,
                out_buf: null_buf,
                in_ptrs: [std::ptr::null_mut(); 2],
                out_ptrs: [std::ptr::null_mut(); 2],
                has_input,
            }),
            log: LogRing::new(),
            name,
            sources: RefCell::new(Sources::default()),
            gui: RefCell::new(GuiState::default()),
        });
        let p: *mut Inner = &mut *b;
        b.host.host_data = p as *mut c_void;
        let rt = b.rt.get();
        // SAFETY: `rt` points into the box we just built; nothing else
        // references it yet.
        unsafe {
            (*rt).in_list.ctx = rt as *mut c_void;
            (*rt).out_list.ctx = rt as *mut c_void;
        }
        b
    }

    pub fn on_audio_thread(&self) -> bool {
        AUDIO_OF.with(|a| std::ptr::eq(a.get(), self))
    }

    pub fn plugin(&self) -> *const clap_plugin {
        self.plugin.get()
    }
}

// ---------------------------------------------------------------------------
// Event lists (audio thread)

unsafe extern "C" fn in_size(l: *const clap_input_events) -> u32 {
    // SAFETY: ctx is the RtState set in Inner::new.
    unsafe { (*((*l).ctx as *const RtState)).n_events as u32 }
}

unsafe extern "C" fn in_get(l: *const clap_input_events, i: u32) -> *const clap_event_header {
    // SAFETY: as above; the index is bounds-checked against n_events.
    unsafe {
        let s = (*l).ctx as *const RtState;
        if i as usize >= (*s).n_events {
            return std::ptr::null();
        }
        (*s).events.as_ptr().add(i as usize) as *const clap_event_header
    }
}

/// Decode a plugin output event that we forward. Other events are ignored.
pub(crate) unsafe fn decode_out(e: *const clap_event_header) -> Option<RtOutEvent> {
    // SAFETY: the plugin passes a valid event whose size matches its type.
    unsafe {
        if (*e).space_id != CLAP_CORE_EVENT_SPACE_ID {
            return None;
        }
        match (*e).type_ {
            CLAP_EVENT_PARAM_VALUE => {
                let p = &*(e as *const clap_event_param_value);
                Some(RtOutEvent {
                    kind: RtOutKind::ParamValue,
                    param_id: p.param_id,
                    value: p.value,
                })
            }
            CLAP_EVENT_PARAM_GESTURE_BEGIN | CLAP_EVENT_PARAM_GESTURE_END => {
                let p = &*(e as *const clap_event_param_gesture);
                Some(RtOutEvent {
                    kind: if (*e).type_ == CLAP_EVENT_PARAM_GESTURE_BEGIN {
                        RtOutKind::GestureBegin
                    } else {
                        RtOutKind::GestureEnd
                    },
                    param_id: p.param_id,
                    value: 0.0,
                })
            }
            _ => None,
        }
    }
}

unsafe extern "C" fn out_push(l: *const clap_output_events, e: *const clap_event_header) -> bool {
    // SAFETY: ctx is the RtState; only the audio thread writes it.
    unsafe {
        let s = (*l).ctx as *mut RtState;
        let Some(ev) = decode_out(e) else {
            return true;
        };
        let n = (*s).n_out;
        if n >= MAX_OUT_EVENTS {
            return false;
        }
        *(*s).out.as_mut_ptr().add(n) = ev;
        (*s).n_out = n + 1;
        true
    }
}

// ---------------------------------------------------------------------------
// Host callbacks

unsafe fn inner<'a>(h: *const clap_host) -> &'a Inner {
    // SAFETY: host_data is the Inner address; Inner outlives the plugin.
    unsafe { &*((*h).host_data as *const Inner) }
}

unsafe extern "C" fn host_request_restart(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.restart.store(true, Release);
}
unsafe extern "C" fn host_request_process(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.process.store(true, Release);
}
unsafe extern "C" fn host_request_callback(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.callback.store(true, Release);
}

// audio-ports, note-ports: layouts are fixed, rescans are ignored.
unsafe extern "C" fn ap_supported(_h: *const clap_host, _flag: u32) -> bool {
    false
}
unsafe extern "C" fn ap_rescan(_h: *const clap_host, _flags: u32) {}
static HOST_AUDIO_PORTS: clap_host_audio_ports = clap_host_audio_ports {
    is_rescan_flag_supported: Some(ap_supported),
    rescan: Some(ap_rescan),
};

unsafe extern "C" fn np_dialects(
    _h: *const clap_host,
) -> clap_sys::ext::note_ports::clap_note_dialect {
    CLAP_NOTE_DIALECT_CLAP
}
unsafe extern "C" fn np_rescan(_h: *const clap_host, _flags: u32) {}
static HOST_NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
    supported_dialects: Some(np_dialects),
    rescan: Some(np_rescan),
};

// params: we always read fresh values, so rescan has nothing to invalidate.
unsafe extern "C" fn pa_rescan(_h: *const clap_host, _flags: clap_param_rescan_flags) {}
unsafe extern "C" fn pa_clear(_h: *const clap_host, _id: u32, _flags: clap_param_clear_flags) {}
unsafe extern "C" fn pa_flush(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.flush.store(true, Release);
}
static HOST_PARAMS: clap_host_params = clap_host_params {
    rescan: Some(pa_rescan),
    clear: Some(pa_clear),
    request_flush: Some(pa_flush),
};

unsafe extern "C" fn st_dirty(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.dirty.store(true, Release);
}
static HOST_STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(st_dirty),
};

unsafe extern "C" fn lat_changed(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.latency.store(true, Release);
}
static HOST_LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(lat_changed),
};

unsafe extern "C" fn tc_main(_h: *const clap_host) -> bool {
    IS_MAIN.with(Cell::get)
}
unsafe extern "C" fn tc_audio(h: *const clap_host) -> bool {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.on_audio_thread()
}
static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(tc_main),
    is_audio_thread: Some(tc_audio),
};

pub(crate) fn host_log_impl(i: &Inner, severity: i32, msg: &[u8]) {
    if i.on_audio_thread() {
        i.log.push(severity, msg);
    } else {
        eprintln!("[plugin {}] {}", i.name, String::from_utf8_lossy(msg));
    }
}
unsafe extern "C" fn log_cb(h: *const clap_host, severity: clap_log_severity, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    // SAFETY: msg is a NUL-terminated string from the plugin.
    unsafe { host_log_impl(inner(h), severity, CStr::from_ptr(msg).to_bytes()) };
}
static HOST_LOG: clap_host_log = clap_host_log { log: Some(log_cb) };

// render is a plugin extension; the host side has nothing to provide.

// timer-support
unsafe extern "C" fn timer_register(h: *const clap_host, period: u32, out_id: *mut u32) -> bool {
    if !IS_MAIN.with(Cell::get) || out_id.is_null() {
        return false;
    }
    // SAFETY: see `inner`; main thread only, so RefCell access is exclusive.
    unsafe {
        let i = inner(h);
        let mut src = i.sources.borrow_mut();
        src.next_timer += 1;
        let id = src.next_timer;
        let ptr = i as *const Inner as usize;
        let s = sources::timer_source(period, move || {
            // SAFETY: all sources are destroyed before the Inner is freed.
            let i = &*(ptr as *const Inner);
            let e = i.exts.get();
            if !e.timer.is_null()
                && let Some(f) = (*e.timer).on_timer
            {
                f(i.plugin(), id);
            }
        });
        src.timers.push((id, s));
        *out_id = id;
    }
    true
}
unsafe extern "C" fn timer_unregister(h: *const clap_host, id: u32) -> bool {
    // SAFETY: see `inner`.
    let mut src = unsafe { inner(h) }.sources.borrow_mut();
    match src.timers.iter().position(|(t, _)| *t == id) {
        Some(p) => {
            src.timers.remove(p).1.destroy();
            true
        }
        None => false,
    }
}
static HOST_TIMER: clap_host_timer_support = clap_host_timer_support {
    register_timer: Some(timer_register),
    unregister_timer: Some(timer_unregister),
};

// posix-fd-support
fn to_cond(flags: u32) -> u32 {
    let mut c = sources::COND_ERR | sources::COND_HUP;
    if flags & CLAP_POSIX_FD_READ != 0 {
        c |= sources::COND_IN;
    }
    if flags & CLAP_POSIX_FD_WRITE != 0 {
        c |= sources::COND_OUT;
    }
    c
}
fn make_fd_source(i: &Inner, fd: i32, flags: u32) -> glib::Source {
    let ptr = i as *const Inner as usize;
    sources::fd_source(
        fd,
        to_cond(flags),
        Box::new(move |fd, cond| {
            let mut f = 0;
            if cond & sources::COND_IN != 0 {
                f |= CLAP_POSIX_FD_READ;
            }
            if cond & sources::COND_OUT != 0 {
                f |= CLAP_POSIX_FD_WRITE;
            }
            if cond & (sources::COND_ERR | sources::COND_HUP) != 0 {
                f |= CLAP_POSIX_FD_ERROR;
            }
            // SAFETY: all sources are destroyed before the Inner is freed.
            unsafe {
                let i = &*(ptr as *const Inner);
                let e = i.exts.get();
                if !e.fd.is_null()
                    && let Some(cb) = (*e.fd).on_fd
                {
                    cb(i.plugin(), fd, f);
                }
            }
        }),
    )
}
unsafe extern "C" fn fd_register(h: *const clap_host, fd: i32, flags: u32) -> bool {
    if !IS_MAIN.with(Cell::get) {
        return false;
    }
    // SAFETY: see `inner`.
    let i = unsafe { inner(h) };
    let mut src = i.sources.borrow_mut();
    if src.fds.iter().any(|(f, _)| *f == fd) {
        return false;
    }
    let s = make_fd_source(i, fd, flags);
    src.fds.push((fd, s));
    true
}
unsafe extern "C" fn fd_modify(h: *const clap_host, fd: i32, flags: u32) -> bool {
    // SAFETY: see `inner`.
    let i = unsafe { inner(h) };
    let mut src = i.sources.borrow_mut();
    let Some(p) = src.fds.iter().position(|(f, _)| *f == fd) else {
        return false;
    };
    src.fds.remove(p).1.destroy();
    let s = make_fd_source(i, fd, flags);
    src.fds.push((fd, s));
    true
}
unsafe extern "C" fn fd_unregister(h: *const clap_host, fd: i32) -> bool {
    // SAFETY: see `inner`.
    let mut src = unsafe { inner(h) }.sources.borrow_mut();
    match src.fds.iter().position(|(f, _)| *f == fd) {
        Some(p) => {
            src.fds.remove(p).1.destroy();
            true
        }
        None => false,
    }
}
static HOST_FD: clap_host_posix_fd_support = clap_host_posix_fd_support {
    register_fd: Some(fd_register),
    modify_fd: Some(fd_modify),
    unregister_fd: Some(fd_unregister),
};

// gui: callable from any thread, so they only set flags.
unsafe extern "C" fn gui_hints(h: *const clap_host) {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.gui_hints.store(true, Release);
}
unsafe extern "C" fn gui_resize(h: *const clap_host, w: u32, hh: u32) -> bool {
    // SAFETY: see `inner`.
    let i = unsafe { inner(h) };
    i.flags
        .gui_size
        .store((u64::from(w) << 32) | u64::from(hh), Release);
    i.flags.gui_resize.store(true, Release);
    true
}
unsafe extern "C" fn gui_show(h: *const clap_host) -> bool {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.gui_show.store(true, Release);
    true
}
unsafe extern "C" fn gui_hide(h: *const clap_host) -> bool {
    // SAFETY: see `inner`.
    unsafe { inner(h) }.flags.gui_hide.store(true, Release);
    true
}
unsafe extern "C" fn gui_closed(h: *const clap_host, destroyed: bool) {
    // SAFETY: see `inner`.
    let i = unsafe { inner(h) };
    i.flags.gui_closed_destroyed.store(destroyed, Release);
    i.flags.gui_closed.store(true, Release);
}
static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_hints),
    request_resize: Some(gui_resize),
    request_show: Some(gui_show),
    request_hide: Some(gui_hide),
    closed: Some(gui_closed),
};

unsafe extern "C" fn host_get_extension(_h: *const clap_host, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return std::ptr::null();
    }
    // SAFETY: id is a NUL-terminated string from the plugin.
    let id = unsafe { CStr::from_ptr(id) };
    macro_rules! ext {
        ($name:expr, $table:expr) => {
            if id == $name {
                return &$table as *const _ as *const c_void;
            }
        };
    }
    ext!(CLAP_EXT_AUDIO_PORTS, HOST_AUDIO_PORTS);
    ext!(CLAP_EXT_NOTE_PORTS, HOST_NOTE_PORTS);
    ext!(CLAP_EXT_PARAMS, HOST_PARAMS);
    ext!(CLAP_EXT_STATE, HOST_STATE);
    ext!(CLAP_EXT_LATENCY, HOST_LATENCY);
    ext!(CLAP_EXT_THREAD_CHECK, HOST_THREAD_CHECK);
    ext!(CLAP_EXT_LOG, HOST_LOG);
    ext!(CLAP_EXT_TIMER_SUPPORT, HOST_TIMER);
    ext!(CLAP_EXT_POSIX_FD_SUPPORT, HOST_FD);
    ext!(CLAP_EXT_GUI, HOST_GUI);
    std::ptr::null()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_thread_log_goes_to_the_ring_not_stderr() {
        let i = Inner::new("t".into(), false);
        let mut got = Vec::new();
        {
            let _scope = AudioScope::enter(&i);
            assert!(i.on_audio_thread());
            for k in 0..LOG_SLOTS + 5 {
                host_log_impl(&i, 2, format!("m{k}").as_bytes());
            }
        }
        assert!(!i.on_audio_thread());
        i.log.drain(|s, m| got.push((s, m.to_string())));
        // full ring drops the newest messages, never blocks or allocates
        assert_eq!(got.len(), LOG_SLOTS);
        assert_eq!(got[0], (2, "m0".to_string()));
        assert_eq!(got[LOG_SLOTS - 1].1, format!("m{}", LOG_SLOTS - 1));
        let mut again = 0;
        i.log.drain(|_, _| again += 1);
        assert_eq!(again, 0);
    }

    #[test]
    fn long_log_lines_are_truncated() {
        let i = Inner::new("t".into(), false);
        let _scope = AudioScope::enter(&i);
        host_log_impl(&i, 1, &[b'x'; 1000]);
        let mut len = 0;
        i.log.drain(|_, m| len = m.len());
        assert_eq!(len, LOG_LEN);
    }
}
