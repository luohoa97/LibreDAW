// SPDX-License-Identifier: GPL-3.0-or-later
//! Test fixtures: a cdylib that exports several tiny CLAP plugins in plain
//! Rust over clap-sys. Not a product feature, never installed.
//!
//! - `test.gain`: stereo effect, parameter 0 = gain (0..2, default 1). Echoes
//!   every parameter value event back as gesture begin, value, gesture end.
//!   Mutes its output if `thread-check` reports the wrong thread.
//! - `test.sine`: instrument, one note input port, parameter 0 = volume.
//! - `test.timer`: stereo pass-through that registers a timer and a pipe fd
//!   and counts callbacks; its state is three little-endian u32 counters.
//! - `test.bad-layout`: mono output port (must be rejected by the host).
//! - `test.fail-activate`: `activate` returns false.

#![allow(non_camel_case_types)]

use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::posix_fd_support::*;
use clap_sys::ext::render::*;
use clap_sys::ext::state::*;
use clap_sys::ext::thread_check::*;
use clap_sys::ext::timer_support::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::plugin::*;
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use clap_sys::{entry::clap_plugin_entry, ext::posix_fd_support::CLAP_POSIX_FD_READ};
use std::ffi::{CStr, c_char, c_void};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Gain,
    Sine,
    Timer,
    BadLayout,
    FailActivate,
}

const IDS: [&CStr; 5] = [
    c"test.gain",
    c"test.sine",
    c"test.timer",
    c"test.bad-layout",
    c"test.fail-activate",
];
const KINDS: [Kind; 5] = [
    Kind::Gain,
    Kind::Sine,
    Kind::Timer,
    Kind::BadLayout,
    Kind::FailActivate,
];

struct Feats([*const c_char; 3]);
// SAFETY: points at immutable 'static C strings.
unsafe impl Sync for Feats {}
static FX: Feats = Feats([
    c"audio-effect".as_ptr(),
    c"stereo".as_ptr(),
    std::ptr::null(),
]);
static INST: Feats = Feats([c"instrument".as_ptr(), c"stereo".as_ptr(), std::ptr::null()]);

const fn desc(i: usize, name: &'static CStr, feats: &'static Feats) -> clap_plugin_descriptor {
    clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: IDS[i].as_ptr(),
        name: name.as_ptr(),
        vendor: c"LibreDAW tests".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"1.0".as_ptr(),
        description: c"test fixture".as_ptr(),
        features: feats.0.as_ptr(),
    }
}

static DESCS: [clap_plugin_descriptor; 5] = [
    desc(0, c"Test Gain", &FX),
    desc(1, c"Test Sine", &INST),
    desc(2, c"Test Timer", &FX),
    desc(3, c"Test Bad Layout", &FX),
    desc(4, c"Test Fail Activate", &FX),
];

struct State {
    kind: Kind,
    host: *const clap_host,
    param: AtomicU64, // f64 bits
    sr: f64,
    phase: f64,
    gate: bool,
    note_key: i16,
    note_id: i32,
    freq: f64,
    vel: f64,
    timer_hits: AtomicU32,
    fd_hits: AtomicU32,
    main_hits: AtomicU32,
    rd: Option<UnixStream>,
    wr: Option<UnixStream>,
}

unsafe fn st<'a>(p: *const clap_plugin) -> &'a mut State {
    // SAFETY: plugin_data is the Box<State> created in create_plugin.
    unsafe { &mut *((*p).plugin_data as *mut State) }
}

fn param_default(k: Kind) -> f64 {
    match k {
        Kind::Sine => 0.5,
        _ => 1.0,
    }
}

fn param_max(k: Kind) -> f64 {
    if k == Kind::Sine { 1.0 } else { 2.0 }
}

unsafe extern "C" fn p_init(p: *const clap_plugin) -> bool {
    // SAFETY: host and plugin pointers are valid for the call.
    unsafe {
        let s = st(p);
        if s.kind == Kind::Timer {
            let h = &*s.host;
            let get = h.get_extension.unwrap();
            let t = get(s.host, CLAP_EXT_TIMER_SUPPORT.as_ptr()) as *const clap_host_timer_support;
            if !t.is_null() {
                let mut id = 0;
                ((*t).register_timer.unwrap())(s.host, 5, &mut id);
            }
            let f = get(s.host, CLAP_EXT_POSIX_FD_SUPPORT.as_ptr())
                as *const clap_host_posix_fd_support;
            if !f.is_null() {
                let (a, mut b) = UnixStream::pair().unwrap();
                a.set_nonblocking(true).unwrap();
                ((*f).register_fd.unwrap())(s.host, a.as_raw_fd(), CLAP_POSIX_FD_READ);
                b.write_all(&[1]).unwrap();
                s.rd = Some(a);
                s.wr = Some(b);
            }
        }
    }
    true
}
unsafe extern "C" fn p_destroy(p: *const clap_plugin) {
    // SAFETY: undoes create_plugin's two Box::into_raw calls.
    unsafe {
        drop(Box::from_raw((*p).plugin_data as *mut State));
        drop(Box::from_raw(p as *mut clap_plugin));
    }
}
unsafe extern "C" fn p_activate(p: *const clap_plugin, sr: f64, _min: u32, _max: u32) -> bool {
    // SAFETY: valid plugin pointer.
    unsafe {
        let s = st(p);
        if s.kind == Kind::FailActivate {
            return false;
        }
        s.sr = sr;
        if s.kind == Kind::Timer {
            ((*s.host).request_callback.unwrap())(s.host);
        }
    }
    true
}
unsafe extern "C" fn p_deactivate(_p: *const clap_plugin) {}
unsafe extern "C" fn p_start(_p: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn p_stop(_p: *const clap_plugin) {}
unsafe extern "C" fn p_reset(p: *const clap_plugin) {
    // SAFETY: valid plugin pointer.
    unsafe {
        let s = st(p);
        s.gate = false;
        s.phase = 0.0;
    }
}
unsafe extern "C" fn p_on_main(p: *const clap_plugin) {
    // SAFETY: valid plugin pointer.
    unsafe { st(p).main_hits.fetch_add(1, Relaxed) };
}

unsafe fn push_param(out: *const clap_output_events, ty: u16, id: u32, value: f64) {
    let size = if ty == CLAP_EVENT_PARAM_VALUE {
        size_of::<clap_event_param_value>()
    } else {
        size_of::<clap_event_param_gesture>()
    };
    let header = clap_event_header {
        size: size as u32,
        time: 0,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_: ty,
        flags: 0,
    };
    // SAFETY: out is a valid output list; the event outlives the call.
    unsafe {
        if ty == CLAP_EVENT_PARAM_VALUE {
            let e = clap_event_param_value {
                header,
                param_id: id,
                cookie: std::ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            };
            ((*out).try_push.unwrap())(out, &e.header);
        } else {
            let e = clap_event_param_gesture {
                header,
                param_id: id,
            };
            ((*out).try_push.unwrap())(out, &e.header);
        }
    }
}

unsafe extern "C" fn p_process(
    p: *const clap_plugin,
    pr: *const clap_process,
) -> clap_process_status {
    // SAFETY: the host passes valid buffers for `frames_count` samples.
    unsafe {
        let s = st(p);
        let pr = &*pr;
        let n = pr.frames_count as usize;
        let outb = &*pr.audio_outputs;
        let o = [*outb.data32.add(0), *outb.data32.add(1)];
        let inp: Option<[&[f32]; 2]> = if pr.audio_inputs_count > 0 {
            let b = &*pr.audio_inputs;
            Some([
                std::slice::from_raw_parts(*b.data32.add(0), n),
                std::slice::from_raw_parts(*b.data32.add(1), n),
            ])
        } else {
            None
        };
        let ev = &*pr.in_events;
        let count = (ev.size.unwrap())(ev);
        let mut pos = 0usize;
        let mut gain = f64::from_bits(s.param.load(Relaxed));
        if s.kind == Kind::Gain {
            let tc = ((*s.host).get_extension.unwrap())(s.host, CLAP_EXT_THREAD_CHECK.as_ptr())
                as *const clap_host_thread_check;
            if !tc.is_null()
                && (!((*tc).is_audio_thread.unwrap())(s.host)
                    || ((*tc).is_main_thread.unwrap())(s.host))
            {
                gain = 0.0;
            }
        }
        let mut i = 0;
        loop {
            let (t, h) = if i < count {
                let h = (ev.get.unwrap())(ev, i);
                (((*h).time as usize).min(n).max(pos), h)
            } else {
                (n, std::ptr::null())
            };
            render(s, &inp, o, pos, t, gain);
            pos = t;
            if h.is_null() {
                break;
            }
            i += 1;
            if (*h).space_id != CLAP_CORE_EVENT_SPACE_ID {
                continue;
            }
            match (*h).type_ {
                CLAP_EVENT_PARAM_VALUE => {
                    let e = &*(h as *const clap_event_param_value);
                    if e.param_id == 0 {
                        s.param.store(e.value.to_bits(), Relaxed);
                        gain = e.value;
                        if s.kind == Kind::Gain {
                            let out = pr.out_events;
                            push_param(out, CLAP_EVENT_PARAM_GESTURE_BEGIN, 0, 0.0);
                            push_param(out, CLAP_EVENT_PARAM_VALUE, 0, e.value);
                            push_param(out, CLAP_EVENT_PARAM_GESTURE_END, 0, 0.0);
                        }
                    }
                }
                CLAP_EVENT_NOTE_ON => {
                    let e = &*(h as *const clap_event_note);
                    s.gate = true;
                    s.note_key = e.key;
                    s.note_id = e.note_id;
                    s.freq = 440.0 * 2f64.powf((e.key as f64 - 69.0) / 12.0);
                    s.vel = e.velocity;
                }
                CLAP_EVENT_NOTE_OFF => {
                    let e = &*(h as *const clap_event_note);
                    if e.note_id == s.note_id || e.key == s.note_key {
                        s.gate = false;
                    }
                }
                _ => {}
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

fn render(
    s: &mut State,
    inp: &Option<[&[f32]; 2]>,
    o: [*mut f32; 2],
    a: usize,
    b: usize,
    gain: f64,
) {
    for i in a..b {
        let v = match s.kind {
            Kind::Sine => {
                if s.gate {
                    let v = (s.phase * std::f64::consts::TAU).sin() * gain * s.vel;
                    s.phase = (s.phase + s.freq / s.sr).fract();
                    [v as f32, v as f32]
                } else {
                    [0.0, 0.0]
                }
            }
            Kind::Timer => inp.map_or([0.0; 2], |x| [x[0][i], x[1][i]]),
            _ => inp.map_or([0.0; 2], |x| [x[0][i] * gain as f32, x[1][i] * gain as f32]),
        };
        // SAFETY: i < frames; the output buffers are exclusively ours.
        unsafe {
            *o[0].add(i) = v[0];
            *o[1].add(i) = v[1];
        }
    }
}

fn put(dst: &mut [c_char], s: &str) {
    for (d, b) in dst.iter_mut().zip(s.bytes().chain(std::iter::once(0))) {
        *d = b as c_char;
    }
}

// audio ports
unsafe extern "C" fn ap_count(p: *const clap_plugin, is_input: bool) -> u32 {
    // SAFETY: valid plugin pointer.
    let k = unsafe { st(p).kind };
    if is_input && k == Kind::Sine { 0 } else { 1 }
}
unsafe extern "C" fn ap_get(
    p: *const clap_plugin,
    i: u32,
    is_in: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    // SAFETY: valid pointers from the host.
    unsafe {
        if i != 0 {
            return false;
        }
        let info = &mut *info;
        info.id = if is_in { 0 } else { 1 };
        put(&mut info.name, "main");
        info.flags = CLAP_AUDIO_PORT_IS_MAIN;
        info.channel_count = if st(p).kind == Kind::BadLayout { 1 } else { 2 };
        info.port_type = if info.channel_count == 2 {
            CLAP_PORT_STEREO.as_ptr()
        } else {
            CLAP_PORT_MONO.as_ptr()
        };
        info.in_place_pair = u32::MAX;
    }
    true
}
static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(ap_count),
    get: Some(ap_get),
};

// note ports
unsafe extern "C" fn np_count(p: *const clap_plugin, is_input: bool) -> u32 {
    // SAFETY: valid plugin pointer.
    let k = unsafe { st(p).kind };
    u32::from(is_input && k == Kind::Sine)
}
unsafe extern "C" fn np_get(
    _p: *const clap_plugin,
    i: u32,
    is_in: bool,
    info: *mut clap_note_port_info,
) -> bool {
    if i != 0 || !is_in {
        return false;
    }
    // SAFETY: valid out pointer.
    unsafe {
        let info = &mut *info;
        info.id = 0;
        info.supported_dialects = CLAP_NOTE_DIALECT_CLAP;
        info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
        put(&mut info.name, "notes");
    }
    true
}
static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(np_count),
    get: Some(np_get),
};

// params
unsafe extern "C" fn pa_count(_p: *const clap_plugin) -> u32 {
    1
}
unsafe extern "C" fn pa_info(p: *const clap_plugin, i: u32, info: *mut clap_param_info) -> bool {
    if i != 0 {
        return false;
    }
    // SAFETY: valid pointers.
    unsafe {
        let k = st(p).kind;
        let info = &mut *info;
        info.id = 0;
        info.flags = CLAP_PARAM_IS_AUTOMATABLE;
        info.cookie = std::ptr::null_mut();
        put(
            &mut info.name,
            if k == Kind::Sine { "Volume" } else { "Gain" },
        );
        put(&mut info.module, "");
        info.min_value = 0.0;
        info.max_value = param_max(k);
        info.default_value = param_default(k);
    }
    true
}
unsafe extern "C" fn pa_get(p: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    if id != 0 {
        return false;
    }
    // SAFETY: valid pointers.
    unsafe { *out = f64::from_bits(st(p).param.load(Relaxed)) };
    true
}
unsafe extern "C" fn pa_v2t(
    _p: *const clap_plugin,
    _id: u32,
    _v: f64,
    _b: *mut c_char,
    _c: u32,
) -> bool {
    false
}
unsafe extern "C" fn pa_t2v(
    _p: *const clap_plugin,
    _id: u32,
    _t: *const c_char,
    _o: *mut f64,
) -> bool {
    false
}
unsafe extern "C" fn pa_flush(
    p: *const clap_plugin,
    inp: *const clap_input_events,
    out: *const clap_output_events,
) {
    // SAFETY: valid lists from the host.
    unsafe {
        let s = st(p);
        let n = ((*inp).size.unwrap())(inp);
        for i in 0..n {
            let h = ((*inp).get.unwrap())(inp, i);
            if (*h).type_ == CLAP_EVENT_PARAM_VALUE {
                let e = &*(h as *const clap_event_param_value);
                s.param.store(e.value.to_bits(), Relaxed);
                if s.kind == Kind::Gain {
                    push_param(out, CLAP_EVENT_PARAM_VALUE, 0, e.value);
                }
            }
        }
    }
}
static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(pa_count),
    get_info: Some(pa_info),
    get_value: Some(pa_get),
    value_to_text: Some(pa_v2t),
    text_to_value: Some(pa_t2v),
    flush: Some(pa_flush),
};

// state
unsafe extern "C" fn s_save(p: *const clap_plugin, os: *const clap_ostream) -> bool {
    // SAFETY: valid plugin and stream.
    unsafe {
        let s = st(p);
        let bytes: Vec<u8> = if s.kind == Kind::Timer {
            [&s.timer_hits, &s.fd_hits, &s.main_hits]
                .iter()
                .flat_map(|c| c.load(Relaxed).to_le_bytes())
                .collect()
        } else {
            s.param.load(Relaxed).to_le_bytes().to_vec()
        };
        let w = (*os).write.unwrap();
        w(os, bytes.as_ptr() as *const c_void, bytes.len() as u64) == bytes.len() as i64
    }
}
unsafe extern "C" fn s_load(p: *const clap_plugin, is: *const clap_istream) -> bool {
    // SAFETY: valid plugin and stream.
    unsafe {
        let s = st(p);
        let mut buf = [0u8; 8];
        let mut got = 0;
        while got < 8 {
            let r =
                ((*is).read.unwrap())(is, buf[got..].as_mut_ptr() as *mut c_void, (8 - got) as u64);
            if r <= 0 {
                break;
            }
            got += r as usize;
        }
        if got != 8 || s.kind == Kind::Timer {
            return s.kind == Kind::Timer && got > 0;
        }
        s.param.store(u64::from_le_bytes(buf), Relaxed);
    }
    true
}
static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(s_save),
    load: Some(s_load),
};

unsafe extern "C" fn lat_get(_p: *const clap_plugin) -> u32 {
    32
}
static LATENCY: clap_plugin_latency = clap_plugin_latency { get: Some(lat_get) };

unsafe extern "C" fn r_rt(_p: *const clap_plugin) -> bool {
    false
}
unsafe extern "C" fn r_set(_p: *const clap_plugin, _m: clap_plugin_render_mode) -> bool {
    true
}
static RENDER: clap_plugin_render = clap_plugin_render {
    has_hard_realtime_requirement: Some(r_rt),
    set: Some(r_set),
};

unsafe extern "C" fn on_timer(p: *const clap_plugin, _id: u32) {
    // SAFETY: valid plugin pointer.
    unsafe { st(p).timer_hits.fetch_add(1, Relaxed) };
}
static TIMER: clap_plugin_timer_support = clap_plugin_timer_support {
    on_timer: Some(on_timer),
};

unsafe extern "C" fn on_fd(p: *const clap_plugin, _fd: i32, _flags: u32) {
    // SAFETY: valid plugin pointer.
    unsafe {
        let s = st(p);
        let mut b = [0u8; 1];
        if let Some(r) = s.rd.as_mut()
            && r.read(&mut b).is_ok()
        {
            s.fd_hits.fetch_add(1, Relaxed);
        }
    }
}
static FD: clap_plugin_posix_fd_support = clap_plugin_posix_fd_support { on_fd: Some(on_fd) };

unsafe extern "C" fn p_ext(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: id is a valid C string; plugin valid.
    unsafe {
        let id = CStr::from_ptr(id);
        let k = st(p).kind;
        if id == CLAP_EXT_AUDIO_PORTS {
            &AUDIO_PORTS as *const _ as *const c_void
        } else if id == CLAP_EXT_NOTE_PORTS {
            &NOTE_PORTS as *const _ as *const c_void
        } else if id == CLAP_EXT_PARAMS {
            &PARAMS as *const _ as *const c_void
        } else if id == CLAP_EXT_STATE {
            &STATE as *const _ as *const c_void
        } else if id == CLAP_EXT_LATENCY {
            &LATENCY as *const _ as *const c_void
        } else if id == CLAP_EXT_RENDER {
            &RENDER as *const _ as *const c_void
        } else if id == CLAP_EXT_TIMER_SUPPORT && k == Kind::Timer {
            &TIMER as *const _ as *const c_void
        } else if id == CLAP_EXT_POSIX_FD_SUPPORT && k == Kind::Timer {
            &FD as *const _ as *const c_void
        } else {
            std::ptr::null()
        }
    }
}

unsafe extern "C" fn f_count(_f: *const clap_plugin_factory) -> u32 {
    DESCS.len() as u32
}
unsafe extern "C" fn f_desc(
    _f: *const clap_plugin_factory,
    i: u32,
) -> *const clap_plugin_descriptor {
    DESCS
        .get(i as usize)
        .map_or(std::ptr::null(), |d| d as *const _)
}
unsafe extern "C" fn f_create(
    _f: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    // SAFETY: id is a valid C string.
    let id = unsafe { CStr::from_ptr(id) };
    let Some(i) = IDS.iter().position(|x| *x == id) else {
        return std::ptr::null();
    };
    let kind = KINDS[i];
    let state = Box::new(State {
        kind,
        host,
        param: AtomicU64::new(param_default(kind).to_bits()),
        sr: 44100.0,
        phase: 0.0,
        gate: false,
        note_key: -1,
        note_id: -1,
        freq: 440.0,
        vel: 1.0,
        timer_hits: AtomicU32::new(0),
        fd_hits: AtomicU32::new(0),
        main_hits: AtomicU32::new(0),
        rd: None,
        wr: None,
    });
    Box::into_raw(Box::new(clap_plugin {
        desc: &DESCS[i],
        plugin_data: Box::into_raw(state) as *mut c_void,
        init: Some(p_init),
        destroy: Some(p_destroy),
        activate: Some(p_activate),
        deactivate: Some(p_deactivate),
        start_processing: Some(p_start),
        stop_processing: Some(p_stop),
        reset: Some(p_reset),
        process: Some(p_process),
        get_extension: Some(p_ext),
        on_main_thread: Some(p_on_main),
    }))
}
static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(f_count),
    get_plugin_descriptor: Some(f_desc),
    create_plugin: Some(f_create),
};

unsafe extern "C" fn e_init(_path: *const c_char) -> bool {
    true
}
unsafe extern "C" fn e_deinit() {}
unsafe extern "C" fn e_factory(id: *const c_char) -> *const c_void {
    // SAFETY: id is a valid C string.
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(e_init),
    deinit: Some(e_deinit),
    get_factory: Some(e_factory),
};
