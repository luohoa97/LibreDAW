// SPDX-License-Identifier: GPL-3.0-or-later
//! GTK main-thread API (docs/phase2-interfaces.md). Every function here runs
//! on the thread that created the `Instance` (the CLAP main thread).

use crate::gui;
use crate::inst::{self, Exts, Inner};
use crate::rt::{RtOutEvent, RtOutKind};
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::CLAP_EXT_GUI;
use clap_sys::ext::latency::CLAP_EXT_LATENCY;
use clap_sys::ext::note_ports::CLAP_EXT_NOTE_PORTS;
use clap_sys::ext::params::*;
use clap_sys::ext::posix_fd_support::CLAP_EXT_POSIX_FD_SUPPORT;
use clap_sys::ext::preset_load::*;
use clap_sys::ext::render::*;
use clap_sys::ext::state::CLAP_EXT_STATE;
use clap_sys::ext::timer_support::CLAP_EXT_TIMER_SUPPORT;
use clap_sys::factory::plugin_factory::*;
use clap_sys::factory::preset_discovery::CLAP_PRESET_DISCOVERY_LOCATION_FILE;
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::clap_version_is_compatible;
use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
use protocol::engine::{PluginEvent, PluginHandle};
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering::*;

pub use crate::gui::gui_scale;

#[derive(Debug)]
pub enum HostError {
    /// dlopen failed or `clap_entry` is missing or incompatible.
    Load(String),
    /// The library has no plugin with the requested id.
    NotFound(String),
    /// `create_plugin` or `init` failed.
    Init(String),
    /// Anything but a stereo output and at most one stereo input.
    PortLayout(String),
    Activate,
    NotActive,
    /// The audio thread still has the plugin.
    Processing,
    State(String),
    NoGui,
    Gui(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Load(s) => write!(f, "cannot load plugin library: {s}"),
            HostError::NotFound(s) => write!(f, "plugin not found: {s}"),
            HostError::Init(s) => write!(f, "plugin failed to initialize: {s}"),
            HostError::PortLayout(s) => write!(f, "unsupported port layout (stereo only): {s}"),
            HostError::Activate => write!(f, "plugin refused to activate"),
            HostError::NotActive => write!(f, "plugin is not active"),
            HostError::Processing => write!(f, "plugin is still processing"),
            HostError::State(s) => write!(f, "plugin state error: {s}"),
            HostError::NoGui => write!(f, "plugin has no GUI"),
            HostError::Gui(s) => write!(f, "plugin GUI error: {s}"),
        }
    }
}

impl std::error::Error for HostError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDesc {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: PathBuf,
    pub instrument: bool,
    pub effect: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub flags: u32,
}

impl ParamInfo {
    pub fn stepped(&self) -> bool {
        self.flags & CLAP_PARAM_IS_STEPPED != 0
    }
    pub fn hidden(&self) -> bool {
        self.flags & CLAP_PARAM_IS_HIDDEN != 0
    }
    pub fn readonly(&self) -> bool {
        self.flags & CLAP_PARAM_IS_READONLY != 0
    }
    pub fn automatable(&self) -> bool {
        self.flags & CLAP_PARAM_IS_AUTOMATABLE != 0
    }
}

// ---------------------------------------------------------------------------
// Libraries

/// A loaded `.clap` file with `clap_entry.init` called. Shared by all
/// instances from the same path; `deinit` runs when the last one goes.
struct LoadedLib {
    path: PathBuf,
    entry: *const clap_plugin_entry,
    // Dropped after `deinit` in `Drop::drop`.
    _lib: Library,
}

impl Drop for LoadedLib {
    fn drop(&mut self) {
        // SAFETY: entry points into `_lib`, still mapped; init succeeded.
        unsafe {
            if let Some(f) = (*self.entry).deinit {
                f();
            }
        }
    }
}

thread_local! {
    static LIBS: RefCell<Vec<Weak<LoadedLib>>> = const { RefCell::new(Vec::new()) };
}

fn load_lib(path: &Path) -> Result<Rc<LoadedLib>, HostError> {
    let existing = LIBS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|w| w.strong_count() > 0);
        l.iter().filter_map(Weak::upgrade).find(|x| x.path == path)
    });
    if let Some(l) = existing {
        return Ok(l);
    }
    let err = |s: String| HostError::Load(format!("{}: {s}", path.display()));
    // SAFETY: loading a plugin runs its initializers; that is the job.
    let lib = unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL) }
        .map_err(|e| err(e.to_string()))?;
    // SAFETY: `clap_entry` is a static of type clap_plugin_entry by the CLAP
    // ABI; the symbol value is its address.
    let entry: *const clap_plugin_entry = unsafe {
        *lib.get::<*const clap_plugin_entry>(b"clap_entry\0")
            .map_err(|e| err(format!("no clap_entry: {e}")))?
    };
    if entry.is_null() {
        return Err(err("clap_entry is null".into()));
    }
    let cpath =
        CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| err("bad path".into()))?;
    // SAFETY: entry is valid for the lifetime of `lib`.
    unsafe {
        if !clap_version_is_compatible((*entry).clap_version) {
            return Err(err("incompatible CLAP version".into()));
        }
        match (*entry).init {
            Some(init) if init(cpath.as_ptr()) => {}
            _ => return Err(err("clap_entry.init failed".into())),
        }
    }
    let l = Rc::new(LoadedLib {
        path: path.to_path_buf(),
        entry,
        _lib: lib,
    });
    LIBS.with(|v| v.borrow_mut().push(Rc::downgrade(&l)));
    Ok(l)
}

fn factory(l: &LoadedLib) -> Result<*const clap_plugin_factory, HostError> {
    // SAFETY: entry is valid while `l` lives.
    let f = unsafe {
        let get = (*l.entry)
            .get_factory
            .ok_or_else(|| HostError::Load("clap_entry has no get_factory".into()))?;
        get(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory
    };
    if f.is_null() {
        return Err(HostError::Load(format!(
            "{}: no plugin factory",
            l.path.display()
        )));
    }
    Ok(f)
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: descriptor strings are NUL-terminated.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn describe(path: &Path) -> Result<Vec<PluginDesc>, HostError> {
    let lib = load_lib(path)?;
    let f = factory(&lib)?;
    let mut out = Vec::new();
    // SAFETY: the factory and its descriptors are valid while `lib` lives;
    // nothing is instantiated.
    unsafe {
        let n = (*f).get_plugin_count.map_or(0, |g| g(f));
        for i in 0..n {
            let Some(d) = (*f)
                .get_plugin_descriptor
                .map(|g| g(f, i))
                .filter(|d| !d.is_null())
            else {
                continue;
            };
            let d = &*d;
            let mut features = Vec::new();
            let mut p = d.features;
            while !p.is_null() && !(*p).is_null() {
                features.push(cstr(*p));
                p = p.add(1);
            }
            out.push(PluginDesc {
                id: cstr(d.id),
                name: cstr(d.name),
                vendor: cstr(d.vendor),
                version: cstr(d.version),
                path: path.to_path_buf(),
                instrument: features.iter().any(|x| x == "instrument"),
                effect: features.iter().any(|x| x == "audio-effect"),
            });
        }
    }
    Ok(out)
}

/// Standard CLAP search paths: `CLAP_PATH`, `~/.clap`, `/usr/lib/clap`,
/// `/usr/lib64/clap`.
pub fn clap_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::env::var("CLAP_PATH")
        .map(|s| {
            s.split(':')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    if let Some(h) = std::env::var_os("HOME") {
        v.push(Path::new(&h).join(".clap"));
    }
    v.push("/usr/lib/clap".into());
    v.push("/usr/lib64/clap".into());
    v.extend(flatpak_extension_dirs());
    v
}

/// Prefix of the Flatpak LinuxAudio plugin extensions (SPEC 19.3).
const EXT_PREFIX: &str = "org.freedesktop.LinuxAudio.Plugins.";

/// CLAP directories of the installed `org.freedesktop.LinuxAudio.Plugins.*`
/// extensions: inside the Flatpak, the mounts under `/app/extensions/Plugins`;
/// outside it, the user and system Flatpak runtime installs.
pub fn flatpak_extension_dirs() -> Vec<PathBuf> {
    if Path::new("/.flatpak-info").exists() {
        return mounted_extension_dirs(Path::new("/app/extensions/Plugins"));
    }
    let mut roots = Vec::new();
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        roots.push(Path::new(&d).join("flatpak"));
    } else if let Some(h) = std::env::var_os("HOME") {
        roots.push(Path::new(&h).join(".local/share/flatpak"));
    }
    roots.push("/var/lib/flatpak".into());
    installed_extension_dirs(&roots)
}

/// Mounted extensions: `<root>/<Name>/clap` and `<root>/<Name>/lib/clap`
/// (what Surge XT, Odin2 and Dexed ship), plus the merged `<root>/lib/clap`.
pub fn mounted_extension_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("lib/clap"), root.join("clap")];
    for d in sorted_dirs(root) {
        if d.ends_with("lib") || d.ends_with("clap") {
            continue;
        }
        out.push(d.join("clap"));
        out.push(d.join("lib/clap"));
    }
    out.retain(|p| p.is_dir());
    out
}

/// Flatpak installations (`<root>/runtime/<ext id>/<arch>/<branch>/active/
/// files/clap`). When several branches of one extension are installed, only
/// the highest branch is used, so a plugin does not appear twice.
pub fn installed_extension_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let arch = std::env::consts::ARCH;
    let mut out = Vec::new();
    for root in roots {
        for ext in sorted_dirs(&root.join("runtime")) {
            let named = ext
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(EXT_PREFIX));
            if !named {
                continue;
            }
            let mut branches = sorted_dirs(&ext.join(arch));
            branches.reverse();
            let found = branches
                .into_iter()
                .map(|b| b.join("active/files/clap"))
                .find(|p| p.is_dir());
            out.extend(found);
        }
    }
    out
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn find_clap_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if depth < 4 {
                find_clap_files(&p, depth + 1, out);
            }
        } else if p.extension().is_some_and(|e| e == "clap") {
            out.push(p);
        }
    }
}

/// Scan the given directories (or `.clap` files) for plugins without
/// instantiating any. Libraries that fail to load are skipped with a warning
/// on stderr.
pub fn scan_paths(paths: &[PathBuf]) -> Vec<PluginDesc> {
    inst::mark_main_thread();
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            find_clap_files(p, 0, &mut files);
        } else if p.is_file() {
            files.push(p.clone());
        }
    }
    files.dedup();
    let mut out = Vec::new();
    for f in files {
        match describe(&f) {
            Ok(mut d) => out.append(&mut d),
            Err(e) => eprintln!("libredaw: scan: {e}"),
        }
    }
    out
}

pub fn scan() -> Vec<PluginDesc> {
    scan_paths(&clap_paths())
}

// ---------------------------------------------------------------------------
// Instance

pub struct Instance {
    inner: Box<Inner>,
    id: String,
    activated: bool,
    rate: f64,
    max_frames: u32,
    changes: Vec<RtOutEvent>,
    // Last: the library must outlive plugin destruction.
    _lib: Rc<LoadedLib>,
}

/// Port layout the host accepts: output port 0 is stereo (the sound), at
/// most one input port that is stereo. Extra output ports (Surge XT has
/// per-scene outputs) are fed scratch buffers and ignored. Returns whether
/// there is an input and the channel counts of the extra outputs.
fn check_ports(
    plugin: *const clap_plugin,
    ap: *const clap_plugin_audio_ports,
) -> Result<(bool, Vec<u32>), HostError> {
    if ap.is_null() {
        return Err(HostError::PortLayout("no audio-ports extension".into()));
    }
    // SAFETY: ext pointer and plugin are valid after init.
    unsafe {
        let (count, get) = ((*ap).count.unwrap(), (*ap).get.unwrap());
        let (n_in, n_out) = (count(plugin, true), count(plugin, false));
        if n_out < 1 || n_in > 1 {
            return Err(HostError::PortLayout(format!(
                "{n_in} input and {n_out} output ports"
            )));
        }
        let mut extra = Vec::new();
        for is_in in [true, false] {
            let n = if is_in { n_in } else { n_out };
            for i in 0..n {
                let mut info: clap_audio_port_info = std::mem::zeroed();
                if !get(plugin, i, is_in, &mut info) {
                    return Err(HostError::PortLayout(format!("cannot query port {i}")));
                }
                if i == 0 && info.channel_count != 2 {
                    return Err(HostError::PortLayout(format!(
                        "{} port has {} channels",
                        if is_in { "input" } else { "output" },
                        info.channel_count
                    )));
                }
                if !is_in && i > 0 {
                    extra.push(info.channel_count);
                }
            }
        }
        Ok((n_in == 1, extra))
    }
}

fn get_ext<T>(plugin: *const clap_plugin, id: &CStr) -> *const T {
    // SAFETY: plugin is valid; get_extension is part of the ABI.
    unsafe {
        (*plugin)
            .get_extension
            .map_or(std::ptr::null(), |g| g(plugin, id.as_ptr()) as *const T)
    }
}

impl Instance {
    /// Load the library, create and init the plugin, validate its ports.
    /// Must be called on the GTK main thread.
    pub fn create(desc: &PluginDesc) -> Result<Instance, HostError> {
        let _cwd = inst::CwdGuard::new();
        inst::mark_main_thread();
        let lib = load_lib(&desc.path)?;
        let f = factory(&lib)?;
        let cid =
            CString::new(desc.id.as_str()).map_err(|_| HostError::NotFound(desc.id.clone()))?;
        // Input presence is unknown until init; fixed up below.
        let inner = Inner::new(desc.name.clone(), false);
        // SAFETY: factory valid; the host struct lives in the box, which
        // outlives the plugin.
        let plugin = unsafe { (*f).create_plugin.map(|c| c(f, &inner.host, cid.as_ptr())) }
            .filter(|p| !p.is_null())
            .ok_or_else(|| HostError::NotFound(desc.id.clone()))?;
        inner.plugin.set(plugin);
        let me = Instance {
            inner,
            id: desc.id.clone(),
            activated: false,
            rate: 0.0,
            max_frames: 0,
            changes: Vec::new(),
            _lib: lib,
        };
        // On any error below, Drop destroys the plugin.
        // SAFETY: plugin valid until destroy.
        let ok = unsafe { (*plugin).init.is_some_and(|i| i(plugin)) };
        if !ok {
            return Err(HostError::Init(desc.id.clone()));
        }
        me.inner.exts.set(Exts {
            audio_ports: get_ext(plugin, CLAP_EXT_AUDIO_PORTS),
            note_ports: get_ext(plugin, CLAP_EXT_NOTE_PORTS),
            params: get_ext(plugin, CLAP_EXT_PARAMS),
            state: get_ext(plugin, CLAP_EXT_STATE),
            gui: get_ext(plugin, CLAP_EXT_GUI),
            latency: get_ext(plugin, CLAP_EXT_LATENCY),
            render: get_ext(plugin, CLAP_EXT_RENDER),
            timer: get_ext(plugin, CLAP_EXT_TIMER_SUPPORT),
            fd: get_ext(plugin, CLAP_EXT_POSIX_FD_SUPPORT),
            preset_load: {
                let p: *const clap_plugin_preset_load = get_ext(plugin, CLAP_EXT_PRESET_LOAD);
                if p.is_null() {
                    get_ext(plugin, CLAP_EXT_PRESET_LOAD_COMPAT)
                } else {
                    p
                }
            },
        });
        let (has_input, extra) = check_ports(plugin, me.inner.exts.get().audio_ports)?;
        me.inner.set_ports(has_input, &extra);
        Ok(me)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The handle for `EngineCommand::AttachPlugin`. Valid until `drop`.
    pub fn handle(&self) -> PluginHandle {
        PluginHandle(&*self.inner as *const Inner as *mut c_void)
    }

    pub fn is_active(&self) -> bool {
        self.activated
    }

    /// Choose offline (export) or realtime render mode. Call while
    /// deactivated. Returns false if the plugin does not take it.
    pub fn set_offline(&mut self, offline: bool) -> bool {
        let e = self.inner.exts.get();
        if self.activated || e.render.is_null() {
            return false;
        }
        let mode = if offline {
            CLAP_RENDER_OFFLINE
        } else {
            CLAP_RENDER_REALTIME
        };
        // SAFETY: valid ext and plugin; main thread, deactivated.
        unsafe {
            (*e.render)
                .set
                .is_some_and(|s| s(self.inner.plugin(), mode))
        }
    }

    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) -> Result<(), HostError> {
        let _cwd = inst::CwdGuard::new();
        if self.activated {
            return Ok(());
        }
        let p = self.inner.plugin();
        // SAFETY: valid plugin, main thread, not active.
        let ok = unsafe {
            (*p).activate
                .is_some_and(|a| a(p, sample_rate, 1, max_frames.max(1)))
        };
        if !ok {
            return Err(HostError::Activate);
        }
        self.activated = true;
        self.rate = sample_rate;
        self.max_frames = max_frames;
        self.inner.active.store(true, Release);
        Ok(())
    }

    pub fn deactivate(&mut self) {
        if !self.activated {
            return;
        }
        let p = self.inner.plugin();
        if self.inner.processing.swap(false, AcqRel) {
            // The engine should have detached us; stop here as a last resort.
            // SAFETY: valid plugin.
            unsafe {
                if let Some(s) = (*p).stop_processing {
                    s(p);
                }
            }
        }
        self.inner.active.store(false, Release);
        // SAFETY: valid plugin, main thread.
        unsafe {
            if let Some(d) = (*p).deactivate {
                d(p);
            }
        }
        self.activated = false;
    }

    /// Plugin-reported latency in frames (reported, not compensated).
    pub fn latency(&self) -> u32 {
        let e = self.inner.exts.get();
        if e.latency.is_null() {
            return 0;
        }
        // SAFETY: valid ext and plugin; main thread.
        unsafe { (*e.latency).get.map_or(0, |g| g(self.inner.plugin())) }
    }

    /// Load a preset file through the `preset-load` extension (a Surge XT
    /// `.fxp`, a Dexed `.syx`). Errors if the plugin does not support it or
    /// rejects the file. Main thread; the plugin may be active.
    pub fn load_preset(&mut self, path: &Path) -> Result<(), HostError> {
        let e = self.inner.exts.get();
        if e.preset_load.is_null() {
            return Err(HostError::State(
                "plugin has no preset-load extension".into(),
            ));
        }
        let loc = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| HostError::State("bad preset path".into()))?;
        // SAFETY: valid ext and plugin; main thread; `loc` outlives the call.
        let ok = unsafe {
            (*e.preset_load).from_location.is_some_and(|f| {
                f(
                    self.inner.plugin(),
                    CLAP_PRESET_DISCOVERY_LOCATION_FILE,
                    loc.as_ptr(),
                    std::ptr::null(),
                )
            })
        };
        if ok {
            Ok(())
        } else {
            Err(HostError::State(format!(
                "cannot load preset {}",
                path.display()
            )))
        }
    }

    pub fn save_state(&mut self) -> Result<Vec<u8>, HostError> {
        let e = self.inner.exts.get();
        if e.state.is_null() {
            return Err(HostError::State("plugin has no state extension".into()));
        }
        unsafe extern "C" fn write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
            // SAFETY: ctx is the Vec we pass below; buf holds `size` bytes.
            unsafe {
                let v = &mut *((*s).ctx as *mut Vec<u8>);
                v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, size as usize));
            }
            size as i64
        }
        let mut data: Vec<u8> = Vec::new();
        let os = clap_ostream {
            ctx: &mut data as *mut Vec<u8> as *mut c_void,
            write: Some(write),
        };
        // SAFETY: valid ext and plugin; main thread; `os` outlives the call.
        let ok = unsafe { (*e.state).save.is_some_and(|f| f(self.inner.plugin(), &os)) };
        if ok {
            Ok(data)
        } else {
            Err(HostError::State("state.save failed".into()))
        }
    }

    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), HostError> {
        let e = self.inner.exts.get();
        if e.state.is_null() {
            return Err(HostError::State("plugin has no state extension".into()));
        }
        struct Reader<'a> {
            data: &'a [u8],
            pos: usize,
        }
        unsafe extern "C" fn read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
            // SAFETY: ctx is the Reader we pass below; buf has room for `size`.
            unsafe {
                let r = &mut *((*s).ctx as *mut Reader);
                let n = (size as usize).min(r.data.len() - r.pos);
                std::ptr::copy_nonoverlapping(r.data.as_ptr().add(r.pos), buf as *mut u8, n);
                r.pos += n;
                n as i64
            }
        }
        let mut r = Reader {
            data: bytes,
            pos: 0,
        };
        let is = clap_istream {
            ctx: &mut r as *mut Reader as *mut c_void,
            read: Some(read),
        };
        // SAFETY: as in save_state.
        let ok = unsafe { (*e.state).load.is_some_and(|f| f(self.inner.plugin(), &is)) };
        if ok {
            Ok(())
        } else {
            Err(HostError::State("state.load failed".into()))
        }
    }

    pub fn params(&mut self) -> Vec<ParamInfo> {
        let e = self.inner.exts.get();
        if e.params.is_null() {
            return Vec::new();
        }
        let p = self.inner.plugin();
        let mut out = Vec::new();
        // SAFETY: valid ext and plugin; main thread.
        unsafe {
            let (Some(count), Some(get)) = ((*e.params).count, (*e.params).get_info) else {
                return out;
            };
            for i in 0..count(p) {
                let mut info: clap_param_info = std::mem::zeroed();
                if get(p, i, &mut info) {
                    out.push(ParamInfo {
                        id: info.id,
                        name: cstr(info.name.as_ptr()),
                        module: cstr(info.module.as_ptr()),
                        min: info.min_value,
                        max: info.max_value,
                        default: info.default_value,
                        flags: info.flags,
                    });
                }
            }
        }
        out
    }

    pub fn param_value(&mut self, id: u32) -> Option<f64> {
        let e = self.inner.exts.get();
        if e.params.is_null() {
            return None;
        }
        let mut v = 0.0;
        // SAFETY: valid ext and plugin; main thread.
        let ok = unsafe {
            (*e.params)
                .get_value
                .is_some_and(|g| g(self.inner.plugin(), id, &mut v))
        };
        ok.then_some(v)
    }

    /// Send parameter values while the plugin is not processing (`params.flush`).
    /// Output events are kept for `take_param_changes`. Does nothing while
    /// the audio thread has the plugin: use the plugin event ring then.
    pub fn flush_params(&mut self, values: &[PluginEvent]) {
        let e = self.inner.exts.get();
        if e.params.is_null() || self.inner.processing.load(Acquire) {
            return;
        }
        let evs: Vec<clap_event_param_value> = values
            .iter()
            .map(|v| clap_event_param_value {
                header: clap_event_header {
                    size: size_of::<clap_event_param_value>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: 0,
                },
                param_id: v.param_id,
                cookie: std::ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: v.value,
            })
            .collect();
        unsafe extern "C" fn size(l: *const clap_input_events) -> u32 {
            // SAFETY: ctx is the Vec passed below.
            unsafe { (*((*l).ctx as *const Vec<clap_event_param_value>)).len() as u32 }
        }
        unsafe extern "C" fn get(l: *const clap_input_events, i: u32) -> *const clap_event_header {
            // SAFETY: as above.
            unsafe {
                let v = &*((*l).ctx as *const Vec<clap_event_param_value>);
                v.get(i as usize)
                    .map_or(std::ptr::null(), |e| &e.header as *const _)
            }
        }
        unsafe extern "C" fn push(
            l: *const clap_output_events,
            e: *const clap_event_header,
        ) -> bool {
            // SAFETY: ctx is the Vec<RtOutEvent> passed below.
            unsafe {
                if let Some(ev) = inst::decode_out(e) {
                    (*((*l).ctx as *mut Vec<RtOutEvent>)).push(ev);
                }
            }
            true
        }
        let inl = clap_input_events {
            ctx: &evs as *const _ as *mut c_void,
            size: Some(size),
            get: Some(get),
        };
        let outl = clap_output_events {
            ctx: &mut self.changes as *mut Vec<RtOutEvent> as *mut c_void,
            try_push: Some(push),
        };
        // SAFETY: valid ext and plugin; lists outlive the call.
        unsafe {
            if let Some(f) = (*e.params).flush {
                f(self.inner.plugin(), &inl, &outl);
            }
        }
    }

    /// Plugin-originated parameter events collected outside processing.
    pub fn take_param_changes(&mut self) -> Vec<RtOutEvent> {
        std::mem::take(&mut self.changes)
    }

    /// Drain messages the plugin logged from the audio thread.
    pub fn drain_log(&mut self, f: impl FnMut(i32, &str)) {
        self.inner.log.drain(f);
    }

    /// The plugin asked to be saved (state.mark_dirty) since the last call.
    pub fn take_dirty(&self) -> bool {
        self.inner.flags.dirty.swap(false, AcqRel)
    }

    /// The plugin reported a latency change since the last call.
    pub fn take_latency_changed(&self) -> bool {
        self.inner.flags.latency.swap(false, AcqRel)
    }

    /// The plugin asked for `process` (request_process) since the last call.
    pub fn take_process_requested(&self) -> bool {
        self.inner.flags.process.swap(false, AcqRel)
    }

    pub fn show_gui(&mut self, title: &str) -> Result<(), HostError> {
        let _cwd = inst::CwdGuard::new();
        gui::show(&self.inner, title)
    }

    pub fn hide_gui(&mut self) {
        let _cwd = inst::CwdGuard::new();
        gui::close(&self.inner);
    }

    /// Timer and fd sources the plugin currently has registered.
    pub fn source_counts(&self) -> (usize, usize) {
        let s = self.inner.sources.borrow();
        (s.timers.len(), s.fds.len())
    }

    pub fn gui_open(&self) -> bool {
        self.inner.gui.borrow().open
    }

    /// Call from the 10 ms GLib source (SPEC 4.4, 9.1): acts on the flags
    /// that host callbacks set from any thread.
    pub fn poll_main_thread(&mut self) {
        let _cwd = inst::CwdGuard::new();
        let i = &*self.inner;
        i.log
            .drain(|sev, msg| eprintln!("[plugin {} log {sev}] {msg}", i.name));
        if i.flags.callback.swap(false, AcqRel) {
            let p = i.plugin();
            // SAFETY: valid plugin; main thread.
            unsafe {
                if let Some(f) = (*p).on_main_thread {
                    f(p);
                }
            }
        }
        let restart = i.flags.restart.load(Acquire);
        let flush = i.flags.flush.load(Acquire);
        let busy = i.processing.load(Acquire);
        if restart && self.activated && !busy {
            self.inner.flags.restart.store(false, Release);
            let (rate, frames) = (self.rate, self.max_frames);
            self.deactivate();
            if self.activate(rate, frames).is_err() {
                eprintln!("libredaw: plugin {} failed to restart", self.id);
            }
        }
        if flush && !busy {
            self.inner.flags.flush.store(false, Release);
            self.flush_params(&[]);
        }
        gui::poll(&self.inner);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // The engine must have detached us first (9.1).
        gui::close(&self.inner);
        self.deactivate();
        self.inner.sources.borrow_mut().destroy_all();
        let p = self.inner.plugin();
        // SAFETY: valid plugin, main thread, deactivated, sources removed.
        unsafe {
            if let Some(d) = (*p).destroy {
                d(p);
            }
        }
        // A plugin may register sources while destroying; remove those too.
        self.inner.sources.borrow_mut().destroy_all();
    }
}

impl RtOutEvent {
    pub fn is_gesture(&self) -> bool {
        self.kind != RtOutKind::ParamValue
    }
}

/// The root of the Flatpak extension (or install prefix) a plugin lives in:
/// the parent of its `clap` directory. Factory presets sit under
/// `<root>/share/...`.
pub fn extension_root(desc: &PluginDesc) -> Option<PathBuf> {
    Some(desc.path.parent()?.parent()?.to_path_buf())
}
