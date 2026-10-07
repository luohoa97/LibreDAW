// SPDX-License-Identifier: GPL-3.0-or-later
//! The one table of keyboard shortcuts (docs/ui-design.md 5.1). Accelerators,
//! tooltips, and the shortcuts window are all generated from it, so they
//! cannot drift apart. No GTK here.

pub struct Shortcut {
    /// Action name (`win.play-pause`), or empty for a key handled inside one
    /// widget (listed so the shortcuts window is complete).
    pub action: &'static str,
    /// Accelerators in GTK syntax. The first one is shown.
    pub accels: &'static [&'static str],
    pub title: &'static str,
    pub group: &'static str,
}

const fn s(
    action: &'static str,
    accels: &'static [&'static str],
    title: &'static str,
    group: &'static str,
) -> Shortcut {
    Shortcut {
        action,
        accels,
        title,
        group,
    }
}

pub const GROUPS: [&str; 7] = [
    "General",
    "Transport",
    "Editing",
    "View",
    "Steps",
    "Notes",
    "Mixer",
];

pub const SHORTCUTS: &[Shortcut] = &[
    // General
    s("win.new", &["<Control>n"], "New Project", "General"),
    s("win.open", &["<Control>o"], "Open Project", "General"),
    s("win.home", &["<Control><Shift>h"], "Home", "General"),
    s("win.save", &["<Control>s"], "Save", "General"),
    s("win.save-as", &["<Control><Shift>s"], "Save As", "General"),
    s("win.export", &["<Control>e"], "Export Audio", "General"),
    s(
        "app.preferences",
        &["<Control>comma"],
        "Preferences",
        "General",
    ),
    s(
        "win.show-help-overlay",
        &["<Control>question", "<Control><Shift>slash"],
        "Keyboard Shortcuts",
        "General",
    ),
    s("win.close", &["<Control>w"], "Close Window", "General"),
    s("app.quit", &["<Control>q"], "Quit", "General"),
    // Transport
    s("win.play-pause", &["space"], "Play or Stop", "Transport"),
    s(
        "win.play-from-start",
        &["<Shift>space"],
        "Play from Start",
        "Transport",
    ),
    s("win.go-start", &["Home"], "Go to Start", "Transport"),
    s("win.metronome", &["<Control>m"], "Metronome", "Transport"),
    // Editing
    s("win.undo", &["<Control>z"], "Undo", "Editing"),
    s("win.redo", &["<Control><Shift>z"], "Redo", "Editing"),
    s("", &[], "Select all", "Editing"),
    s(
        "win.rename",
        &["F2"],
        "Rename the selected channel",
        "Editing",
    ),
    // View
    s("win.sounds", &["F9"], "Show or hide Sounds", "View"),
    s(
        "win.inspector",
        &["<Shift>F9"],
        "Show or hide Inspector",
        "View",
    ),
    s(
        "win.view-timeline",
        &["<Control>1"],
        "Go to Timeline",
        "View",
    ),
    s("win.view-mixer", &["<Control>2"], "Go to Mixer", "View"),
    s(
        "win.zoom-in",
        &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
        "Zoom in",
        "View",
    ),
    s(
        "win.zoom-out",
        &["<Control>minus", "<Control>KP_Subtract"],
        "Zoom out",
        "View",
    ),
    s("win.zoom-reset", &["<Control>0"], "Reset zoom", "View"),
    s(
        "win.edit-notes",
        &["<Control>Return"],
        "Edit the notes of the selected channel",
        "View",
    ),
    // Steps (inside the step grid)
    s("", &["Left"], "Move the cursor", "Steps"),
    s("", &["Return"], "Turn the step on or off", "Steps"),
    s("", &["Delete"], "Clear the step", "Steps"),
    s("", &["Home"], "First step of the row", "Steps"),
    s("", &["End"], "Last step of the row", "Steps"),
    // Notes (inside the piano roll)
    s(
        "",
        &["Return"],
        "Add or remove a note at the cursor",
        "Notes",
    ),
    s(
        "",
        &["<Shift>Right"],
        "Make the selected notes longer",
        "Notes",
    ),
    s(
        "",
        &["<Control>Right"],
        "Move the selected notes later",
        "Notes",
    ),
    s("", &["<Control>Up"], "Move the selected notes up", "Notes"),
    s("", &["Delete"], "Delete the selected notes", "Notes"),
    s("", &["<Control>a"], "Select all notes", "Notes"),
    s("", &["Escape"], "Clear the selection", "Notes"),
    // Mixer (inside a strip)
    s("", &["m"], "Mute the track", "Mixer"),
    s("", &["s"], "Solo the track", "Mixer"),
    s("", &["Home"], "Set the fader to 0 dB", "Mixer"),
];

/// Looks up the first accelerator of an action.
pub fn accel_of(action: &str) -> Option<&'static str> {
    SHORTCUTS
        .iter()
        .find(|s| s.action == action)
        .and_then(|s| s.accels.first().copied())
}

/// `<Control><Shift>z` becomes `Shift+Ctrl+Z` (the order the GNOME HIG
/// uses). `space` becomes `Space`, `comma` becomes `,`.
pub fn accel_label(accel: &str) -> String {
    let mut shift = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut rest = accel;
    while let Some(r) = rest.strip_prefix('<') {
        let Some((m, tail)) = r.split_once('>') else {
            break;
        };
        match m {
            "Shift" => shift = true,
            "Control" | "Primary" => ctrl = true,
            "Alt" => alt = true,
            _ => {}
        }
        rest = tail;
    }
    let key = match rest {
        "space" => "Space".to_string(),
        "comma" => ",".to_string(),
        "question" => "?".to_string(),
        "slash" => "/".to_string(),
        "plus" | "KP_Add" => "+".to_string(),
        "minus" | "KP_Subtract" => "-".to_string(),
        "equal" => "=".to_string(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    };
    let mut out = String::new();
    if shift {
        out.push_str("Shift+");
    }
    if ctrl {
        out.push_str("Ctrl+");
    }
    if alt {
        out.push_str("Alt+");
    }
    out.push_str(&key);
    out
}

/// A tooltip with the shortcut of `action` in parentheses, if it has one.
pub fn tooltip(text: &str, action: &str) -> String {
    match accel_of(action) {
        Some(a) => format!("{text} ({})", accel_label(a)),
        None => text.to_string(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `GtkBuilder` XML of the shortcuts window, generated from the table.
pub fn shortcuts_window_xml() -> String {
    let mut x = String::from(
        "<interface><object class=\"GtkShortcutsWindow\" id=\"help\">\
         <property name=\"modal\">1</property>\
         <child><object class=\"GtkShortcutsSection\">\
         <property name=\"section-name\">shortcuts</property>\
         <property name=\"max-height\">14</property>",
    );
    for g in GROUPS {
        x.push_str(&format!(
            "<child><object class=\"GtkShortcutsGroup\"><property name=\"title\">{}</property>",
            xml_escape(g)
        ));
        for sc in SHORTCUTS.iter().filter(|s| s.group == g) {
            if sc.accels.is_empty() {
                continue;
            }
            x.push_str(&format!(
                "<child><object class=\"GtkShortcutsShortcut\">\
                 <property name=\"title\">{}</property>\
                 <property name=\"accelerator\">{}</property></object></child>",
                xml_escape(sc.title),
                xml_escape(&sc.accels.join(" "))
            ));
        }
        x.push_str("</object></child>");
    }
    x.push_str("</object></child></object></interface>");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_hig_order() {
        assert_eq!(accel_label("<Control>z"), "Ctrl+Z");
        assert_eq!(accel_label("<Control><Shift>z"), "Shift+Ctrl+Z");
        assert_eq!(accel_label("<Shift>F9"), "Shift+F9");
        assert_eq!(accel_label("space"), "Space");
        assert_eq!(accel_label("<Control>comma"), "Ctrl+,");
        assert_eq!(accel_label("<Control>question"), "Ctrl+?");
        assert_eq!(accel_label("Home"), "Home");
    }

    #[test]
    fn tooltips_carry_the_shortcut() {
        assert_eq!(tooltip("Undo", "win.undo"), "Undo (Ctrl+Z)");
        assert_eq!(tooltip("Redo", "win.redo"), "Redo (Shift+Ctrl+Z)");
        assert_eq!(tooltip("Play", "win.play-pause"), "Play (Space)");
        assert_eq!(tooltip("Nothing", "win.unknown"), "Nothing");
    }

    #[test]
    fn no_accelerator_is_bound_twice() {
        let mut seen = std::collections::HashMap::new();
        for sc in SHORTCUTS.iter().filter(|s| !s.action.is_empty()) {
            for a in sc.accels {
                if let Some(prev) = seen.insert(*a, sc.action) {
                    panic!("{a} bound to {prev} and {}", sc.action);
                }
            }
        }
    }

    #[test]
    fn no_shortcut_uses_super_or_alt() {
        for sc in SHORTCUTS {
            for a in sc.accels {
                assert!(!a.contains("Super") && !a.contains("Alt"), "{a}");
            }
        }
    }

    #[test]
    fn every_group_has_a_listed_shortcut() {
        for g in GROUPS {
            assert!(
                SHORTCUTS
                    .iter()
                    .any(|s| s.group == g && !s.accels.is_empty()),
                "{g}"
            );
        }
        for sc in SHORTCUTS {
            assert!(GROUPS.contains(&sc.group), "{}", sc.group);
        }
    }

    #[test]
    fn window_xml_is_escaped_and_complete() {
        let x = shortcuts_window_xml();
        assert!(x.contains("&lt;Control&gt;n"));
        assert!(x.contains("Keyboard Shortcuts"));
        assert!(!x.contains("<Control>"));
        assert_eq!(x.matches("<object class=\"GtkShortcutsGroup\"").count(), 7);
    }
}
