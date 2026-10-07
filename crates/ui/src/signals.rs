// SPDX-License-Identifier: GPL-3.0-or-later
//! Unix signals as a GLib source (Amendment 10): SIGTERM, SIGHUP, and SIGINT
//! make the app save and exit. `glib` 0.22 has no wrapper for
//! `g_unix_signal_source_new`, so this is a thin FFI call (the function is in
//! libglib-2.0, already linked through glib-sys). The callback runs on the
//! main context's thread, never in the signal handler.

use std::ffi::{c_int, c_void};

use gtk::glib::ffi;
use gtk::glib::translate::from_glib_full;
use gtk::glib::{MainContext, Source};

unsafe extern "C" {
    fn g_unix_signal_source_new(signum: c_int) -> *mut ffi::GSource;
}

type Callback = Box<dyn FnMut()>;

unsafe extern "C" fn trampoline(data: *mut c_void) -> ffi::gboolean {
    // SAFETY: `data` is the Box<Callback> leaked in `signal_source`; GLib
    // calls this on the thread iterating the main context, one call at a time.
    let f = unsafe { &mut *(data as *mut Callback) };
    f();
    ffi::G_SOURCE_CONTINUE
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    // SAFETY: matches the Box::into_raw in `signal_source`.
    drop(unsafe { Box::from_raw(data as *mut Callback) });
}

/// Calls `f` on the default main context each time `signum` arrives. Keep
/// the returned `Source` or call `.destroy()` to stop.
pub fn signal_source(signum: i32, f: impl FnMut() + 'static) -> Source {
    let data = Box::into_raw(Box::new(Box::new(f) as Callback)) as *mut c_void;
    // SAFETY: the new source is owned by us until `from_glib_full`; the
    // callback type is a plain GSourceFunc.
    let src = unsafe {
        let s = g_unix_signal_source_new(signum);
        ffi::g_source_set_callback(s, Some(trampoline), data, Some(destroy));
        from_glib_full::<_, Source>(s)
    };
    src.attach(Some(&MainContext::default()));
    src
}

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGTERM: i32 = 15;
