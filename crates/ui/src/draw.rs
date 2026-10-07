// SPDX-License-Identifier: GPL-3.0-or-later
//! Colors and small drawing helpers for the custom widgets (`snapshot()`,
//! SPEC 11).
//!
//! The palette is derived from the libadwaita theme the widget sits in (view
//! background and foreground, accent, success, warning, and error colors),
//! so the grids, the roll, and the meters match the standard widgets around
//! them in light and dark mode and follow the user's accent color. Only the
//! way the colors are combined is ours.

use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};

fn rgba(r: f32, g: f32, b: f32, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(r, g, b, a)
}

/// A named theme color, if the theme defines it.
#[allow(deprecated)]
fn named(w: &impl IsA<gtk::Widget>, name: &str) -> Option<gdk::RGBA> {
    w.style_context().lookup_color(name)
}

/// `t` of the way from `bg` to `c`, opaque.
pub fn mix(c: &gdk::RGBA, bg: &gdk::RGBA, t: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        bg.red() + (c.red() - bg.red()) * t,
        bg.green() + (c.green() - bg.green()) * t,
        bg.blue() + (c.blue() - bg.blue()) * t,
        1.0,
    )
}

fn with_alpha(c: &gdk::RGBA, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), a)
}

/// Colors for the grid widgets.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: gdk::RGBA,
    pub fg: gdk::RGBA,
    pub row_even: gdk::RGBA,
    pub row_odd: gdk::RGBA,
    pub row_black_key: gdk::RGBA,
    pub line_bar: gdk::RGBA,
    pub line_beat: gdk::RGBA,
    pub line_sub: gdk::RGBA,
    pub text: gdk::RGBA,
    pub text_dim: gdk::RGBA,
    pub accent: gdk::RGBA,
    pub accent_fg: gdk::RGBA,
    pub note: gdk::RGBA,
    pub note_edge: gdk::RGBA,
    pub note_sel: gdk::RGBA,
    pub playhead: gdk::RGBA,
    pub key_white: gdk::RGBA,
    pub key_black: gdk::RGBA,
    pub key_text: gdk::RGBA,
    pub cell_off: gdk::RGBA,
    pub cell_off_alt: gdk::RGBA,
    pub cell_on: gdk::RGBA,
    pub hatch: gdk::RGBA,
    pub cursor: gdk::RGBA,
    pub outside: gdk::RGBA,
    pub ok: gdk::RGBA,
    pub warn: gdk::RGBA,
    pub error: gdk::RGBA,
}

impl Palette {
    /// The palette for the theme `w` is drawn in.
    pub fn of(w: &impl IsA<gtk::Widget>) -> Palette {
        let dark = adw::StyleManager::default().is_dark();
        let bg = named(w, "view_bg_color").unwrap_or(if dark {
            rgba(0.118, 0.118, 0.118, 1.0)
        } else {
            rgba(1.0, 1.0, 1.0, 1.0)
        });
        let fg = named(w, "view_fg_color").unwrap_or(if dark {
            rgba(1.0, 1.0, 1.0, 1.0)
        } else {
            rgba(0.0, 0.0, 0.0, 0.8)
        });
        let fg = with_alpha(&fg, 1.0);
        let accent = named(w, "accent_bg_color").unwrap_or(rgba(0.208, 0.518, 0.894, 1.0));
        let ok = named(w, "success_color").unwrap_or(rgba(0.18, 0.76, 0.49, 1.0));
        let warn = named(w, "warning_color").unwrap_or(rgba(0.96, 0.83, 0.18, 1.0));
        let error = named(w, "error_color").unwrap_or(rgba(0.88, 0.11, 0.14, 1.0));
        // Selected notes: the accent pushed toward the foreground.
        let note_sel = mix(&fg, &accent, 0.55);
        Palette {
            bg,
            fg,
            row_even: bg,
            row_odd: mix(&fg, &bg, 0.035),
            row_black_key: mix(&fg, &bg, 0.075),
            line_bar: mix(&fg, &bg, 0.42),
            line_beat: mix(&fg, &bg, 0.2),
            line_sub: mix(&fg, &bg, 0.09),
            text: fg,
            text_dim: mix(&fg, &bg, 0.6),
            accent,
            accent_fg: named(w, "accent_fg_color").unwrap_or(rgba(1.0, 1.0, 1.0, 1.0)),
            note: accent,
            note_edge: mix(&bg, &accent, 0.45),
            note_sel,
            playhead: error,
            key_white: mix(&fg, &bg, 0.05),
            key_black: mix(&fg, &bg, 0.2),
            key_text: mix(&fg, &bg, 0.6),
            cell_off: mix(&fg, &bg, 0.1),
            cell_off_alt: mix(&fg, &bg, 0.15),
            cell_on: accent,
            hatch: with_alpha(&warn, 0.55),
            cursor: fg,
            outside: with_alpha(&mix(&fg, &bg, 0.08), 0.75),
            ok,
            warn,
            error,
        }
    }
}

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
}

pub fn fill(s: &gtk::Snapshot, color: &gdk::RGBA, x: f64, y: f64, w: f64, h: f64) {
    if w > 0.0 && h > 0.0 {
        s.append_color(color, &rect(x, y, w, h));
    }
}

/// A one-pixel-wide vertical line centered on the pixel column of `x`.
pub fn vline(s: &gtk::Snapshot, color: &gdk::RGBA, x: f64, y0: f64, y1: f64) {
    fill(s, color, x.floor(), y0, 1.0, y1 - y0);
}

pub fn hline(s: &gtk::Snapshot, color: &gdk::RGBA, y: f64, x0: f64, x1: f64) {
    fill(s, color, x0, y.floor(), x1 - x0, 1.0);
}

pub fn rounded(s: &gtk::Snapshot, color: &gdk::RGBA, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = radius.min(w / 2.0).min(h / 2.0) as f32;
    let rr = gsk::RoundedRect::from_rect(rect(x, y, w, h), r);
    s.push_rounded_clip(&rr);
    s.append_color(color, &rect(x, y, w, h));
    s.pop();
}

/// Draws `text` with its top-left corner at `(x, y)`. Returns the layout
/// size in pixels.
pub fn text(
    w: &impl IsA<gtk::Widget>,
    s: &gtk::Snapshot,
    color: &gdk::RGBA,
    x: f64,
    y: f64,
    text: &str,
    bold: bool,
) -> (i32, i32) {
    let layout = w.create_pango_layout(Some(text));
    if bold {
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        layout.set_attributes(Some(&attrs));
    }
    s.save();
    s.translate(&graphene::Point::new(x as f32, y as f32));
    s.append_layout(&layout, color);
    s.restore();
    layout.pixel_size()
}

/// Draws `text` clipped to a box (for labels inside notes and rows).
#[allow(clippy::too_many_arguments)]
pub fn text_in(
    w: &impl IsA<gtk::Widget>,
    s: &gtk::Snapshot,
    color: &gdk::RGBA,
    x: f64,
    y: f64,
    bw: f64,
    bh: f64,
    label: &str,
) {
    s.push_clip(&rect(x, y, bw, bh));
    let layout = w.create_pango_layout(Some(label));
    let (_, th) = layout.pixel_size();
    s.save();
    s.translate(&graphene::Point::new(
        x as f32,
        (y + (bh - th as f64) / 2.0) as f32,
    ));
    s.append_layout(&layout, color);
    s.restore();
    s.pop();
}
