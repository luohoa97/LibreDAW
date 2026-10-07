// SPDX-License-Identifier: GPL-3.0-or-later
//! Plain language (SPEC 20.6): the words the UI may not show, and the test
//! that reads every label, title, tooltip, toast and accessible name in
//! this crate's sources and fails on any of them.

/// (word as it may appear, the word to show instead). Matched as whole
/// words, without case, in user-visible strings.
pub const FORBIDDEN: &[(&str, &str)] = &[
    ("step", "Grid (or hit)"),
    ("steps", "Grid (or hits)"),
    ("step sequencer", "Grid"),
    ("piano roll", "Piano"),
    ("velocity", "Volume"),
    ("ratchet", "Repeats"),
    ("quantize", "Snap to Grid"),
    ("linked", "Copy That Changes Together"),
    ("unique", "Edit Separately"),
    ("master", "Main Output"),
    ("pan", "Left/Right"),
    ("channel", "Instrument"),
    ("channels", "Instruments"),
    ("render", "Export"),
    ("bounce", "Export"),
    ("insert", "Effects"),
    ("inserts", "Effects"),
    ("playlist", "Timeline"),
    ("preset", "sound"),
    ("presets", "sounds"),
];

/// The whole-word lowercase words of `s`.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// The forbidden terms in `text`, with their replacements.
pub fn violations(text: &str) -> Vec<(&'static str, &'static str)> {
    let ws = words(text);
    let lower = text.to_lowercase();
    FORBIDDEN
        .iter()
        .filter(|(bad, _)| {
            if bad.contains(' ') {
                lower.contains(bad)
            } else {
                ws.iter().any(|w| w == bad)
            }
        })
        .copied()
        .collect()
}

/// "BPM" alone is not a label; "120 BPM" is a value with its unit.
pub fn bare_bpm(text: &str) -> bool {
    text.trim() == "BPM"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls whose string arguments the user sees.
    const MARKERS: &[&str] = &[
        "set_label(",
        "set_title(",
        "set_tooltip_text(",
        "with_label(",
        "Label::new(",
        ".append(Some(",
        "append_submenu(Some(",
        "toast(",
        "toast_action(",
        "set_description(",
        "Property::Label(",
        "Property::ValueText(",
        "set_subtitle(",
        "set_placeholder_text(",
        ".label(",
        "add_titled_with_icon(",
        "AlertDialog::new(",
        "WindowTitle::new(",
        "MenuItem::new(",
        "set_button_label(",
    ];

    /// String literals on a line.
    fn literals(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur: Option<String> = None;
        let mut esc = false;
        for c in line.chars() {
            match (&mut cur, c) {
                (Some(s), '\\') if !esc => {
                    esc = true;
                    s.push(c);
                    continue;
                }
                (Some(s), '"') if !esc => {
                    out.push(std::mem::take(s));
                    cur = None;
                }
                (Some(s), _) => s.push(c),
                (None, '"') => cur = Some(String::new()),
                (None, _) => {}
            }
            esc = false;
        }
        out
    }

    fn sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let name = p.display().to_string();
                    if name.ends_with("tests.rs") || name.ends_with("vocabulary.rs") {
                        continue;
                    }
                    out.push((name, std::fs::read_to_string(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut out,
        );
        out
    }

    #[test]
    fn patterns_are_named_in_the_lane_and_menus() {
        // SPEC 20.7: "Pattern" is shown, with its one-line explanation.
        assert!(violations("Make Pattern").is_empty());
        assert!(violations(crate::pattern_logic::TOOLTIP).is_empty());
        assert!(!violations("Pan").is_empty(), "Left/Right instead");
    }

    #[test]
    fn the_checker_finds_jargon() {
        assert!(!violations("Show the Velocity Lane").is_empty());
        assert!(violations("Volume of each hit").is_empty());
        assert!(!violations("Open the piano roll").is_empty());
        assert!(violations("Snap to Grid").is_empty(), "Grid is our word");
        assert!(violations("Copy That Changes Together").is_empty());
        assert!(bare_bpm(" BPM"));
        assert!(!bare_bpm("120 BPM"));
    }

    /// Labels that live in tables and menus, not in widget calls.
    #[test]
    fn tables_and_menus_use_plain_words() {
        use gtk::gio;
        use gtk::prelude::*;
        let mut texts: Vec<String> = Vec::new();
        for (l, _) in crate::view_math::SNAPS {
            texts.push(l.to_string());
        }
        for l in crate::lane_logic::Lane::ALL {
            texts.push(l.label().to_string());
            texts.push(l.tooltip().to_string());
        }
        fn labels(menu: &gio::MenuModel, out: &mut Vec<String>) {
            for i in 0..menu.n_items() {
                if let Some(l) = menu
                    .item_attribute_value(i, "label", None)
                    .and_then(|v| v.get::<String>())
                {
                    out.push(l.replace('_', ""));
                }
                for link in ["section", "submenu"] {
                    if let Some(sub) = menu.item_link(i, link) {
                        labels(&sub, out);
                    }
                }
            }
        }
        for m in [
            crate::menus::main_menu(),
            crate::menus::channel_menu(),
            crate::menus::clip_menu(),
            crate::menus::pattern_menu(),
            crate::menus::shape_lane_menu(),
            crate::menus::shape_point_menu(),
            crate::menus::roll_menu(),
            crate::menus::strip_menu(),
            crate::menus::sound_menu(),
            crate::menus::effects_menu(),
        ] {
            labels(m.upcast_ref(), &mut texts);
        }
        let bad: Vec<String> = texts
            .iter()
            .filter(|t| !violations(t).is_empty())
            .map(|t| format!("{t}: {:?}", violations(t)))
            .collect();
        assert!(
            bad.is_empty(),
            "plain language (SPEC 20.6):\n{}",
            bad.join("\n")
        );
    }

    /// Every visible string in the crate follows the 20.6 vocabulary.
    #[test]
    fn no_visible_string_uses_daw_jargon() {
        let mut found = Vec::new();
        for (file, text) in sources() {
            let lines: Vec<&str> = text.lines().collect();
            let mut in_tests = false;
            for (i, line) in lines.iter().enumerate() {
                if line.contains("#[cfg(test)]") {
                    in_tests = true;
                }
                if in_tests {
                    break;
                }
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                // The call and, for calls split over lines, the next two.
                let marked = MARKERS.iter().any(|m| line.contains(m))
                    || (1..=2).any(|k| {
                        i >= k
                            && MARKERS.iter().any(|m| lines[i - k].contains(m))
                            && lines[i - k].trim_end().ends_with('(')
                    });
                if !marked {
                    continue;
                }
                for lit in literals(line) {
                    // Action names, CSS classes and format holes are not text.
                    if lit.contains('.') && !lit.contains(' ') || lit.starts_with("ldaw-") {
                        continue;
                    }
                    for (bad, good) in violations(&lit) {
                        found.push(format!("{file}:{}: \"{lit}\": \"{bad}\" -> {good}", i + 1));
                    }
                    // The tempo field shows "Tempo [120] BPM": there the unit is fine.
                    if bare_bpm(&lit) && !file.ends_with("transport.rs") {
                        found.push(format!("{file}:{}: \"BPM\" alone -> Tempo", i + 1));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "plain language (SPEC 20.6):\n{}",
            found.join("\n")
        );
    }
}
