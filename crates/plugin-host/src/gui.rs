// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin GUI windows (SPEC 9.2, 9.5). Floating windows when the plugin
//! supports them, otherwise an X11 top-level created with x11rb that the
//! plugin embeds into. All functions run on the GTK main thread.

use crate::host::HostError;
use crate::inst::Inner;
use clap_sys::ext::gui::*;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::Ordering::*;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub(crate) struct X11Win {
    conn: Rc<RustConnection>,
    win: u32,
    root: u32,
    wm_delete: u32,
    wm_protocols: u32,
    net_active: u32,
    source: glib::Source,
    width: u32,
    height: u32,
    can_resize: bool,
}

#[derive(Default)]
pub(crate) struct GuiState {
    pub open: bool,
    pub floating: bool,
    pub x11: Option<X11Win>,
}

fn gui_err(msg: impl Into<String>) -> HostError {
    HostError::Gui(msg.into())
}

/// Pure parser for the `Xft.dpi` line of the X resource database.
pub fn parse_xft_dpi(resources: &str) -> Option<f64> {
    resources.lines().find_map(|l| {
        let v = l.strip_prefix("Xft.dpi:")?;
        v.trim().parse::<f64>().ok().filter(|d| *d > 0.0)
    })
}

fn is_wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// 9.2: 1.0 under Wayland (the compositor scales XWayland windows),
/// `Xft.dpi / 96` under X11.
pub fn gui_scale() -> f64 {
    if is_wayland_session() {
        return 1.0;
    }
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return 1.0;
    };
    let root = conn.setup().roots[screen].root;
    let reply = conn
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            1 << 16,
        )
        .ok()
        .and_then(|c| c.reply().ok());
    reply
        .and_then(|r| String::from_utf8(r.value).ok())
        .and_then(|s| parse_xft_dpi(&s))
        .map_or(1.0, |dpi| dpi / 96.0)
}

/// Run `f` on the plugin's gui extension.
macro_rules! gui_call {
    ($i:expr, $f:ident ( $($a:expr),* )) => {{
        let p = $i.plugin();
        let g = $i.exts.get().gui;
        // SAFETY: gui ext pointer and plugin are valid while the Instance
        // lives; we are on the main thread.
        unsafe { (*g).$f.map(|f| f(p $(, $a)*)) }
    }};
}

fn x11_api() -> *const std::ffi::c_char {
    CLAP_WINDOW_API_X11.as_ptr()
}

pub(crate) fn show(i: &Inner, title: &str) -> Result<(), HostError> {
    if i.exts.get().gui.is_null() {
        return Err(HostError::NoGui);
    }
    if i.gui.borrow().open {
        return raise(i);
    }
    let floating = gui_call!(i, is_api_supported(x11_api(), true)) == Some(true);
    if !floating && gui_call!(i, is_api_supported(x11_api(), false)) != Some(true) {
        return Err(gui_err(
            "plugin supports neither floating nor embedded X11 windows",
        ));
    }
    if gui_call!(i, create(x11_api(), floating)) != Some(true) {
        return Err(gui_err("gui.create failed"));
    }
    let _ = gui_call!(i, set_scale(gui_scale()));
    let ctitle = CString::new(title.replace('\0', "")).unwrap_or_default();
    if floating {
        let _ = gui_call!(i, suggest_title(ctitle.as_ptr()));
        if gui_call!(i, show()) != Some(true) {
            let _ = gui_call!(i, destroy());
            return Err(gui_err("gui.show failed"));
        }
        let mut g = i.gui.borrow_mut();
        g.open = true;
        g.floating = true;
        return Ok(());
    }
    match embed(i, title, &ctitle) {
        Ok(win) => {
            let mut g = i.gui.borrow_mut();
            g.open = true;
            g.floating = false;
            g.x11 = Some(win);
            Ok(())
        }
        Err(e) => {
            let _ = gui_call!(i, destroy());
            Err(e)
        }
    }
}

fn atom(c: &RustConnection, name: &[u8]) -> Result<u32, HostError> {
    c.intern_atom(false, name)
        .map_err(|e| gui_err(e.to_string()))?
        .reply()
        .map(|r| r.atom)
        .map_err(|e| gui_err(e.to_string()))
}

fn embed(i: &Inner, title: &str, ctitle: &CString) -> Result<X11Win, HostError> {
    let (mut w, mut h) = (0u32, 0u32);
    if gui_call!(i, get_size(&mut w, &mut h)) != Some(true) || w == 0 || h == 0 {
        return Err(gui_err("gui.get_size failed"));
    }
    let (conn, screen) = x11rb::connect(None).map_err(|e| {
        gui_err(format!(
            "cannot connect to an X11 display (XWayland missing?): {e}"
        ))
    })?;
    let conn = Rc::new(conn);
    let root = conn.setup().roots[screen].root;
    let win = conn.generate_id().map_err(|e| gui_err(e.to_string()))?;
    let x = |e: x11rb::errors::ConnectionError| gui_err(e.to_string());
    conn.create_window(
        0,
        win,
        root,
        0,
        0,
        w as u16,
        h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    )
    .map_err(x)?;
    let wm_protocols = atom(&conn, b"WM_PROTOCOLS")?;
    let wm_delete = atom(&conn, b"WM_DELETE_WINDOW")?;
    let net_active = atom(&conn, b"_NET_ACTIVE_WINDOW")?;
    let net_name = atom(&conn, b"_NET_WM_NAME")?;
    let utf8 = atom(&conn, b"UTF8_STRING")?;
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        title.as_bytes(),
    )
    .map_err(x)?;
    conn.change_property8(PropMode::REPLACE, win, net_name, utf8, title.as_bytes())
        .map_err(x)?;
    conn.change_property32(
        PropMode::REPLACE,
        win,
        wm_protocols,
        AtomEnum::ATOM,
        &[wm_delete],
    )
    .map_err(x)?;
    conn.map_window(win).map_err(x)?;
    conn.flush().map_err(x)?;
    let cleanup = |c: &RustConnection| {
        let _ = c.destroy_window(win);
        let _ = c.flush();
    };
    let clap_win = clap_window {
        api: x11_api(),
        specific: clap_window_handle {
            x11: u64::from(win),
        },
    };
    if gui_call!(i, set_parent(&clap_win)) != Some(true) {
        cleanup(&conn);
        return Err(gui_err("gui.set_parent failed"));
    }
    let _ = gui_call!(i, suggest_title(ctitle.as_ptr()));
    if gui_call!(i, show()) != Some(true) {
        cleanup(&conn);
        return Err(gui_err("gui.show failed"));
    }
    let can_resize = gui_call!(i, can_resize()) == Some(true);
    let fd = conn.stream().as_raw_fd();
    let ptr = i as *const Inner as usize;
    let source = crate::sources::fd_source(
        fd,
        crate::sources::COND_IN | crate::sources::COND_ERR | crate::sources::COND_HUP,
        Box::new(move |_, _| {
            // SAFETY: the source is destroyed before the Inner is freed.
            drain_x11(unsafe { &*(ptr as *const Inner) });
        }),
    );
    Ok(X11Win {
        conn,
        win,
        root,
        wm_delete,
        wm_protocols,
        net_active,
        source,
        width: w,
        height: h,
        can_resize,
    })
}

/// 9.5: re-raise an open window.
fn raise(i: &Inner) -> Result<(), HostError> {
    let g = i.gui.borrow();
    match &g.x11 {
        Some(x) => {
            let ev = ClientMessageEvent::new(32, x.win, x.net_active, [1, 0, 0, 0, 0]);
            let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
            let _ = x.conn.send_event(false, x.root, mask, ev);
            let _ = x.conn.map_window(x.win);
            let _ = x.conn.flush();
            Ok(())
        }
        None => {
            drop(g);
            let _ = gui_call!(i, show());
            Ok(())
        }
    }
}

/// hide, then destroy (WM_DELETE_WINDOW and `hide_gui`).
pub(crate) fn close(i: &Inner) {
    if !i.gui.borrow().open {
        return;
    }
    let _ = gui_call!(i, hide());
    let _ = gui_call!(i, destroy());
    release(i);
}

/// Drop our window state without touching the plugin.
fn release(i: &Inner) {
    let x = {
        let mut g = i.gui.borrow_mut();
        g.open = false;
        g.floating = false;
        g.x11.take()
    };
    if let Some(x) = x {
        x.source.destroy();
        let _ = x.conn.destroy_window(x.win);
        let _ = x.conn.flush();
    }
}

fn drain_x11(i: &Inner) {
    let Some(conn) = i.gui.borrow().x11.as_ref().map(|x| x.conn.clone()) else {
        return;
    };
    loop {
        match conn.poll_for_event() {
            Ok(Some(ev)) => handle_event(i, ev),
            Ok(None) => break,
            Err(_) => {
                // Connection lost: the window is gone.
                close(i);
                break;
            }
        }
        if !i.gui.borrow().open {
            break;
        }
    }
}

fn handle_event(i: &Inner, ev: Event) {
    let (win, wm_delete, wm_protocols) = match i.gui.borrow().x11.as_ref() {
        Some(x) => (x.win, x.wm_delete, x.wm_protocols),
        None => return,
    };
    match ev {
        Event::ClientMessage(m)
            if m.window == win && m.type_ == wm_protocols && m.data.as_data32()[0] == wm_delete =>
        {
            close(i);
        }
        Event::ConfigureNotify(c) if c.window == win => {
            let (w, h) = (u32::from(c.width), u32::from(c.height));
            let (old, can_resize) = match i.gui.borrow().x11.as_ref() {
                Some(x) => ((x.width, x.height), x.can_resize),
                None => return,
            };
            if (w, h) == old || !can_resize {
                return;
            }
            let (mut aw, mut ah) = (w, h);
            let _ = gui_call!(i, adjust_size(&mut aw, &mut ah));
            set_stored(i, aw, ah);
            let _ = gui_call!(i, set_size(aw, ah));
            if (aw, ah) != (w, h) {
                resize_window(i, aw, ah);
            }
        }
        _ => {}
    }
}

fn set_stored(i: &Inner, w: u32, h: u32) {
    if let Some(x) = i.gui.borrow_mut().x11.as_mut() {
        x.width = w;
        x.height = h;
    }
}

fn resize_window(i: &Inner, w: u32, h: u32) {
    if let Some(x) = i.gui.borrow().x11.as_ref() {
        let _ = x
            .conn
            .configure_window(x.win, &ConfigureWindowAux::new().width(w).height(h));
        let _ = x.conn.flush();
    }
}

/// Handle the flags set by `clap_host_gui` callbacks. Called from
/// `Instance::poll_main_thread`.
pub(crate) fn poll(i: &Inner) {
    let f = &i.flags;
    if f.gui_closed.swap(false, AcqRel) {
        let destroyed = f.gui_closed_destroyed.load(Acquire);
        if i.gui.borrow().open {
            if destroyed {
                release(i);
            } else {
                close(i);
            }
        }
    }
    if !i.gui.borrow().open {
        f.gui_resize.store(false, Release);
        f.gui_show.store(false, Release);
        f.gui_hide.store(false, Release);
        return;
    }
    if f.gui_resize.swap(false, AcqRel) {
        let v = f.gui_size.load(Acquire);
        let (w, h) = ((v >> 32) as u32, v as u32);
        if i.gui.borrow().x11.is_some() && w > 0 && h > 0 {
            set_stored(i, w, h);
            resize_window(i, w, h);
            let _ = gui_call!(i, set_size(w, h));
        }
    }
    if f.gui_show.swap(false, AcqRel) {
        if let Some(x) = i.gui.borrow().x11.as_ref() {
            let _ = x.conn.map_window(x.win);
            let _ = x.conn.flush();
        }
        let _ = gui_call!(i, show());
    }
    if f.gui_hide.swap(false, AcqRel) {
        let _ = gui_call!(i, hide());
        if let Some(x) = i.gui.borrow().x11.as_ref() {
            let _ = x.conn.unmap_window(x.win);
            let _ = x.conn.flush();
        }
    }
    f.gui_hints.store(false, Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xft_dpi_parses() {
        assert_eq!(
            parse_xft_dpi("Xft.antialias:\t1\nXft.dpi:\t144\n"),
            Some(144.0)
        );
        assert_eq!(parse_xft_dpi("Xft.dpi: 96"), Some(96.0));
        assert_eq!(parse_xft_dpi("Xft.dpi: x"), None);
        assert_eq!(parse_xft_dpi(""), None);
    }
}
