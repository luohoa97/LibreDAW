// SPDX-License-Identifier: GPL-3.0-or-later
//! Screenshots for testing without a screenshot tool: the window and every
//! open popover (menus are separate surfaces), composed into one PNG.
//!
//! `LIBREDAW_SHOT_DIR=/dir` makes SIGUSR1 write `/dir/<name>.png`, where
//! `<name>` is read from `/dir/next` (or a counter), and then write
//! `/dir/<name>.done`. A test script drives the app with synthetic input and
//! asks for a shot between steps. Off unless the variable is set.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, graphene};

/// Every mapped popover under `w`, depth first.
fn popovers(w: &gtk::Widget, out: &mut Vec<gtk::Popover>) {
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(p) = x.downcast_ref::<gtk::Popover>()
            && p.is_mapped()
        {
            out.push(p.clone());
        }
        popovers(&x, out);
        c = x.next_sibling();
    }
}

/// Where a native's widget origin is, in the toplevel surface.
fn origin(native: &gtk::Native) -> Option<(f64, f64)> {
    let surface = native.surface()?;
    let (tx, ty) = native.surface_transform();
    let mut x = tx;
    let mut y = ty;
    let mut s = surface;
    while let Some(p) = s.downcast_ref::<gdk::Popup>() {
        x += p.position_x() as f64;
        y += p.position_y() as f64;
        s = p.parent()?;
    }
    Some((x, y))
}

/// Renders `window` with its open popovers to `path`.
pub fn capture(window: &gtk::Window, path: &Path) -> Result<(), String> {
    let (w, h) = (window.width(), window.height());
    let snap = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(&snap, w as f64, h as f64);
    let win_native: gtk::Native = window.clone().upcast();
    let (wx, wy) = origin(&win_native).ok_or("no window surface")?;
    let mut pops = Vec::new();
    popovers(window.upcast_ref(), &mut pops);
    for p in pops {
        let native: gtk::Native = p.clone().upcast();
        let Some((px, py)) = origin(&native) else {
            continue;
        };
        snap.save();
        snap.translate(&graphene::Point::new((px - wx) as f32, (py - wy) as f32));
        gtk::WidgetPaintable::new(Some(&p)).snapshot(&snap, p.width() as f64, p.height() as f64);
        snap.restore();
    }
    let node = snap.to_node().ok_or("nothing drawn")?;
    let renderer = window
        .native()
        .and_then(|n| n.renderer())
        .ok_or("no renderer")?;
    let tex = renderer.render_texture(
        &node,
        Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32)),
    );
    tex.save_to_png(path).map_err(|e| e.to_string())
}

/// Installs the SIGUSR1 handler when `LIBREDAW_SHOT_DIR` is set.
pub fn install(window: &gtk::Window) {
    let Some(dir) = std::env::var_os("LIBREDAW_SHOT_DIR").map(PathBuf::from) else {
        return;
    };
    let n = Rc::new(Cell::new(0u32));
    let w = window.downgrade();
    let src = crate::signals::signal_source(10, move || {
        let Some(win) = w.upgrade() else { return };
        let name = std::fs::read_to_string(dir.join("next"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.contains('/'))
            .unwrap_or_else(|| {
                n.set(n.get() + 1);
                format!("shot-{}", n.get())
            });
        let r = capture(&win, &dir.join(format!("{name}.png")));
        if let Err(e) = &r {
            eprintln!("libredaw: shot {name}: {e}");
        }
        let _ = std::fs::write(dir.join(format!("{name}.done")), format!("{r:?}\n"));
    });
    // Lives as long as the process.
    std::mem::forget(src);
    // Log where presses land, so a test script can check its aim.
    let c = gtk::GestureClick::new();
    c.set_button(0);
    c.set_propagation_phase(gtk::PropagationPhase::Capture);
    c.connect_pressed(|g, n, x, y| {
        eprintln!(
            "libredaw: press button {} n {n} at {x:.0},{y:.0}",
            g.current_button()
        );
    });
    window.add_controller(c);
    let k = gtk::EventControllerKey::new();
    k.set_propagation_phase(gtk::PropagationPhase::Capture);
    k.connect_key_pressed(|_, key, code, st| {
        eprintln!("libredaw: key {key:?} code {code} state {st:?}");
        glib::Propagation::Proceed
    });
    window.add_controller(k);
}
