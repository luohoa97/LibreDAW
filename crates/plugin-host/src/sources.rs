// SPDX-License-Identifier: GPL-3.0-or-later
//! GLib sources on the thread-default main context: timers and unix fds.
//!
//! `glib` 0.22 has no `unix_fd_add_local`, so the fd source is a thin FFI
//! wrapper over `g_unix_fd_source_new` (part of libglib-2.0, already linked
//! through glib-sys).

use glib::ffi;
use glib::translate::from_glib_full;
use glib::{ControlFlow, MainContext, Priority, Source};
use std::ffi::{c_int, c_uint, c_void};
use std::time::Duration;

unsafe extern "C" {
    fn g_unix_fd_source_new(fd: c_int, condition: ffi::GIOCondition) -> *mut ffi::GSource;
}

pub const COND_IN: u32 = 1;
pub const COND_OUT: u32 = 4;
pub const COND_ERR: u32 = 8;
pub const COND_HUP: u32 = 16;

type FdFn = Box<dyn FnMut(i32, u32)>;

unsafe extern "C" fn fd_tramp(fd: c_int, cond: c_uint, data: *mut c_void) -> ffi::gboolean {
    // SAFETY: `data` is the Box<FdFn> leaked in `fd_source`; glib calls this
    // only on the thread that owns the context, one call at a time.
    let f = unsafe { &mut *(data as *mut FdFn) };
    f(fd, cond);
    ffi::G_SOURCE_CONTINUE
}

unsafe extern "C" fn fd_destroy(data: *mut c_void) {
    // SAFETY: matches the Box::into_raw in `fd_source`.
    drop(unsafe { Box::from_raw(data as *mut FdFn) });
}

/// Attach an fd watch to the thread-default main context. The closure runs
/// on the thread that iterates that context.
pub fn fd_source(fd: i32, cond: u32, f: FdFn) -> Source {
    let data = Box::into_raw(Box::new(f)) as *mut c_void;
    // SAFETY: the new source is owned by us until `from_glib_full`; the
    // callback type matches GUnixFDSourceFunc, which g_source_set_callback
    // takes as an untyped GSourceFunc.
    let src = unsafe {
        let s = g_unix_fd_source_new(fd, cond);
        let func: unsafe extern "C" fn(c_int, c_uint, *mut c_void) -> ffi::gboolean = fd_tramp;
        ffi::g_source_set_callback(
            s,
            Some(std::mem::transmute::<
                unsafe extern "C" fn(c_int, c_uint, *mut c_void) -> ffi::gboolean,
                unsafe extern "C" fn(*mut c_void) -> ffi::gboolean,
            >(func)),
            data,
            Some(fd_destroy),
        );
        from_glib_full::<_, Source>(s)
    };
    src.attach(Some(&MainContext::ref_thread_default()));
    src
}

struct SendPtr<T>(T);
// SAFETY: the closure only runs on the context-owning thread (the GTK main
// thread); Send is demanded by glib's signature, not needed in practice.
unsafe impl<T> Send for SendPtr<T> {}
impl<F: FnMut()> SendPtr<F> {
    fn call(&mut self) {
        (self.0)()
    }
}

/// A repeating timer on the thread-default main context.
pub fn timer_source(period_ms: u32, f: impl FnMut() + 'static) -> Source {
    let mut f = SendPtr(f);
    let src = glib::timeout_source_new(
        Duration::from_millis(u64::from(period_ms.max(1))),
        None,
        Priority::DEFAULT,
        move || {
            f.call();
            ControlFlow::Continue
        },
    );
    src.attach(Some(&MainContext::ref_thread_default()));
    src
}
