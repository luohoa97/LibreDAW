// SPDX-License-Identifier: GPL-3.0-or-later
//! Theme colors for the custom widgets (docs/ui-design.md 4.1).
//!
//! libadwaita 1.5 has no API to read a named color, and
//! `StyleContext::lookup_color` is deprecated. Instead a small set of hidden
//! probe widgets sit in the window, each styled in `style.css` with
//! `color: @<named color>`. Reading a probe's `color()` returns the color of
//! the active style, so light, dark, and high-contrast all work, and no hex
//! value appears in drawing code.
//!
//! The result is cached. `generation()` changes whenever the style may have
//! changed (dark or high-contrast switch, CSS reload); widgets compare it to
//! the generation their render-node caches were built with.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::gdk;

/// The named colors, in probe order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    ViewBg,
    ViewFg,
    WindowBg,
    WindowFg,
    CardBg,
    AccentBg,
    AccentFg,
    Accent,
    Success,
    Warning,
    Error,
    ErrorBg,
}

impl Role {
    pub const ALL: [Role; 12] = [
        Role::ViewBg,
        Role::ViewFg,
        Role::WindowBg,
        Role::WindowFg,
        Role::CardBg,
        Role::AccentBg,
        Role::AccentFg,
        Role::Accent,
        Role::Success,
        Role::Warning,
        Role::Error,
        Role::ErrorBg,
    ];

    fn class(self) -> &'static str {
        match self {
            Role::ViewBg => "ldaw-probe-view-bg",
            Role::ViewFg => "ldaw-probe-view-fg",
            Role::WindowBg => "ldaw-probe-window-bg",
            Role::WindowFg => "ldaw-probe-window-fg",
            Role::CardBg => "ldaw-probe-card-bg",
            Role::AccentBg => "ldaw-probe-accent-bg",
            Role::AccentFg => "ldaw-probe-accent-fg",
            Role::Accent => "ldaw-probe-accent",
            Role::Success => "ldaw-probe-success",
            Role::Warning => "ldaw-probe-warning",
            Role::Error => "ldaw-probe-error",
            Role::ErrorBg => "ldaw-probe-error-bg",
        }
    }
}

/// Number of channel identity colors (docs/ui-design.md 4.1).
pub const CHANNEL_COLORS: usize = 10;

/// All colors the custom widgets use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub roles: [gdk::RGBA; 12],
    pub channel: [gdk::RGBA; CHANNEL_COLORS],
    pub dark: bool,
    pub high_contrast: bool,
}

impl Colors {
    pub fn get(&self, r: Role) -> gdk::RGBA {
        self.roles[r as usize]
    }

    /// The identity color of a channel (an index chosen from its id, so a
    /// theme change recolors everything and project files stay
    /// theme-neutral).
    pub fn channel_color(&self, id: u32) -> gdk::RGBA {
        self.channel[id as usize % CHANNEL_COLORS]
    }

    /// Colors when no probe is installed (unit tests, headless): the
    /// libadwaita light palette as numbers. Never used when a window exists.
    pub fn fallback() -> Colors {
        let c = |r: f32, g: f32, b: f32| gdk::RGBA::new(r, g, b, 1.0);
        let grey = c(0.5, 0.5, 0.5);
        Colors {
            roles: [
                c(1.0, 1.0, 1.0),
                c(0.2, 0.2, 0.2),
                c(0.98, 0.98, 0.98),
                c(0.2, 0.2, 0.2),
                c(1.0, 1.0, 1.0),
                c(0.2, 0.5, 0.9),
                c(1.0, 1.0, 1.0),
                c(0.1, 0.4, 0.8),
                c(0.2, 0.6, 0.3),
                c(0.7, 0.5, 0.0),
                c(0.8, 0.1, 0.1),
                c(0.8, 0.1, 0.1),
            ],
            channel: [grey; CHANNEL_COLORS],
            dark: false,
            high_contrast: false,
        }
    }
}

struct Probe {
    root: gtk::Box,
    roles: Vec<gtk::Widget>,
    channels: Vec<gtk::Widget>,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    static CACHE: Cell<Option<(u64, Colors)>> = const { Cell::new(None) };
    static GENERATION: Cell<u64> = const { Cell::new(1) };
}

/// Changes whenever colors may have changed.
pub fn generation() -> u64 {
    GENERATION.with(Cell::get)
}

/// Marks the colors stale. Also schedules a second bump on the next main
/// loop iteration, because libadwaita may swap its stylesheet after the
/// notification that got us here.
pub fn invalidate() {
    GENERATION.with(|g| g.set(g.get() + 1));
    CACHE.with(|c| c.set(None));
}

fn sync_dark_class() {
    PROBE.with(|p| {
        if let Some(p) = p.borrow().as_ref() {
            if adw::StyleManager::default().is_dark() {
                p.root.add_css_class("dark");
            } else {
                p.root.remove_css_class("dark");
            }
        }
    });
}

/// Creates the probe widgets. Add the returned widget to the window (it is
/// invisible and takes no space). Call once.
pub fn install() -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.set_can_target(false);
    root.set_can_focus(false);
    root.set_focusable(false);
    root.set_halign(gtk::Align::Start);
    root.set_valign(gtk::Align::Start);
    root.add_css_class("ldaw-probes");
    root.set_accessible_role(gtk::AccessibleRole::Presentation);
    let mut roles = Vec::new();
    for r in Role::ALL {
        let w = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        w.add_css_class(r.class());
        w.set_can_target(false);
        root.append(&w);
        roles.push(w.upcast::<gtk::Widget>());
    }
    let mut channels = Vec::new();
    for i in 0..CHANNEL_COLORS {
        let w = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        w.add_css_class(&format!("ldaw-probe-ch{i}"));
        w.set_can_target(false);
        root.append(&w);
        channels.push(w.upcast::<gtk::Widget>());
    }
    let out = root.clone().upcast::<gtk::Widget>();
    PROBE.with(|p| {
        *p.borrow_mut() = Some(Probe {
            root,
            roles,
            channels,
        })
    });
    sync_dark_class();
    let sm = adw::StyleManager::default();
    for prop in ["notify::dark", "notify::high-contrast"] {
        sm.connect_notify_local(Some(&prop[8..]), |_, _| {
            sync_dark_class();
            invalidate();
            gtk::glib::idle_add_local_once(invalidate);
        });
    }
    out
}

/// Redraws `w` when the style switches between light, dark, and high
/// contrast. Custom widgets call this once when they are created. The
/// redraw runs a moment after the notification, when libadwaita has
/// swapped its stylesheet.
pub fn watch(w: &impl IsA<gtk::Widget>) {
    let weak = w.upcast_ref::<gtk::Widget>().downgrade();
    let sm = adw::StyleManager::default();
    for prop in ["dark", "high-contrast"] {
        let weak_for_handler = weak.clone();
        let weak2_src = weak.clone();
        let id = sm.connect_notify_local(Some(prop), move |_, _| {
            let weak = weak_for_handler.clone();
            for ms in [0u64, 120] {
                let weak = weak.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(ms),
                    move || {
                        invalidate();
                        if let Some(w) = weak.upgrade() {
                            w.queue_draw();
                        }
                    },
                );
            }
        });
        // Disconnect with the widget so the handler does not pile up.
        let weak2 = weak2_src.clone();
        let cell = std::cell::RefCell::new(Some(id));
        if let Some(w) = weak2.upgrade() {
            w.connect_destroy(move |_| {
                if let Some(id) = cell.borrow_mut().take() {
                    adw::StyleManager::default().disconnect(id);
                }
            });
        }
    }
}

/// The current colors.
pub fn colors() -> Colors {
    let gen_now = generation();
    if let Some((g, c)) = CACHE.with(Cell::get)
        && g == gen_now
    {
        return c;
    }
    let read = PROBE.with(|p| {
        let p = p.borrow();
        let p = p.as_ref()?;
        let mut c = Colors::fallback();
        for (i, w) in p.roles.iter().enumerate() {
            c.roles[i] = w.color();
        }
        for (i, w) in p.channels.iter().enumerate() {
            c.channel[i] = w.color();
        }
        let sm = adw::StyleManager::default();
        c.dark = sm.is_dark();
        c.high_contrast = sm.is_high_contrast();
        Some(c)
    });
    // A theme without libadwaita's named colors (a plain GTK theme through
    // GTK_THEME) leaves every probe the same color: use the fallback so the
    // custom widgets stay readable.
    let usable = |c: &Colors| c.get(Role::ViewBg) != c.get(Role::ViewFg);
    let c = match read {
        Some(c) if usable(&c) => c,
        _ => {
            let dark = adw::StyleManager::default().is_dark();
            let mut f = Colors::fallback();
            if dark {
                let w = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
                let g = gdk::RGBA::new(0.12, 0.12, 0.12, 1.0);
                f.roles[Role::ViewBg as usize] = g;
                f.roles[Role::ViewFg as usize] = w;
                f.roles[Role::WindowBg as usize] = g;
                f.roles[Role::WindowFg as usize] = w;
                f.roles[Role::CardBg as usize] = g;
                f.dark = true;
            }
            f
        }
    };
    CACHE.with(|cache| cache.set(Some((gen_now, c))));
    c
}

/// WCAG relative luminance of an sRGB color.
pub fn luminance(c: &gdk::RGBA) -> f32 {
    fn lin(v: f32) -> f32 {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(c.red()) + 0.7152 * lin(c.green()) + 0.0722 * lin(c.blue())
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast_ratio(a: &gdk::RGBA, b: &gdk::RGBA) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Black or white, whichever reads better on `fill` (text on notes).
pub fn readable_on(fill: &gdk::RGBA) -> gdk::RGBA {
    let black = gdk::RGBA::new(0.0, 0.0, 0.0, 1.0);
    let white = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
    if contrast_ratio(fill, &black) >= contrast_ratio(fill, &white) {
        black
    } else {
        white
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> gdk::RGBA {
        gdk::RGBA::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
    }

    #[test]
    fn contrast_of_black_and_white_is_21() {
        let w = rgb(255, 255, 255);
        let b = rgb(0, 0, 0);
        assert!((contrast_ratio(&w, &b) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(&w, &w) - 1.0).abs() < 1e-6);
        assert_eq!(contrast_ratio(&w, &b), contrast_ratio(&b, &w));
    }

    #[test]
    fn readable_text_color_follows_luminance() {
        assert_eq!(readable_on(&rgb(255, 255, 255)), rgb(0, 0, 0));
        assert_eq!(readable_on(&rgb(10, 10, 10)), rgb(255, 255, 255));
        // The libadwaita blue_3 is light enough for black text.
        assert_eq!(readable_on(&rgb(98, 160, 234)), rgb(0, 0, 0));
    }

    #[test]
    fn channel_color_wraps() {
        let c = Colors::fallback();
        assert_eq!(
            c.channel_color(3),
            c.channel_color(3 + CHANNEL_COLORS as u32)
        );
    }

    #[test]
    fn libadwaita_channel_tones_have_contrast() {
        // blue_3 on the dark view background and blue_5 on the light one
        // (the tones the stylesheet picks) must be visible: at least 3:1.
        let dark_bg = rgb(30, 30, 30);
        let light_bg = rgb(255, 255, 255);
        assert!(contrast_ratio(&rgb(98, 160, 234), &dark_bg) >= 3.0);
        assert!(contrast_ratio(&rgb(26, 95, 180), &light_bg) >= 3.0);
    }
}
