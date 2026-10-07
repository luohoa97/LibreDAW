// SPDX-License-Identifier: GPL-3.0-or-later
//! Key handling rules shared by the custom widgets (crates/ui/INTERACTIONS.md):
//! a widget handles only the key combinations it documents. Any other
//! modifier combination falls through to the window's accelerators, so
//! Ctrl+Return, Alt+Left, Ctrl+Z and the rest keep working while a grid
//! has the keyboard.

use gtk::gdk::ModifierType;

/// The modifiers that change a key's meaning (not Caps Lock or Num Lock).
pub fn mods(state: ModifierType) -> ModifierType {
    state
        & (ModifierType::SHIFT_MASK
            | ModifierType::CONTROL_MASK
            | ModifierType::ALT_MASK
            | ModifierType::SUPER_MASK
            | ModifierType::META_MASK
            | ModifierType::HYPER_MASK)
}

/// No modifier held.
pub fn plain(state: ModifierType) -> bool {
    mods(state).is_empty()
}

/// Exactly the modifiers `want` are held.
pub fn only(state: ModifierType, want: ModifierType) -> bool {
    mods(state) == want
}

/// A second press inside the double-click time on the same target is not
/// a new press: a double-click on a step toggles it once, not twice.
#[derive(Clone, Copy, Debug, Default)]
pub struct RepeatFilter<T: Copy + PartialEq> {
    last: Option<(T, i64)>,
}

impl<T: Copy + PartialEq> RepeatFilter<T> {
    pub fn new() -> Self {
        RepeatFilter { last: None }
    }

    /// Records a press on `target` at `now_ms`. Returns `true` when it is
    /// the second press of a double-click (and so must be ignored).
    pub fn is_repeat(&mut self, target: T, now_ms: i64, double_click_ms: i64) -> bool {
        match self.last {
            Some((t, at)) if t == target && now_ms - at <= double_click_ms => {
                // A third press starts over.
                self.last = None;
                true
            }
            _ => {
                self.last = Some((target, now_ms));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers() {
        assert!(plain(ModifierType::empty()));
        assert!(
            plain(ModifierType::LOCK_MASK),
            "Caps Lock is not a modifier"
        );
        assert!(!plain(ModifierType::CONTROL_MASK));
        assert!(!plain(ModifierType::ALT_MASK));
        assert!(only(
            ModifierType::SHIFT_MASK | ModifierType::LOCK_MASK,
            ModifierType::SHIFT_MASK
        ));
        assert!(!only(
            ModifierType::SHIFT_MASK | ModifierType::CONTROL_MASK,
            ModifierType::SHIFT_MASK
        ));
    }

    #[test]
    fn a_double_click_is_one_press() {
        let mut f = RepeatFilter::new();
        assert!(!f.is_repeat((0, 3), 1000, 400));
        assert!(
            f.is_repeat((0, 3), 1200, 400),
            "second press of a double-click"
        );
        assert!(
            !f.is_repeat((0, 3), 1300, 400),
            "a third press counts again"
        );
        assert!(!f.is_repeat((0, 4), 1350, 400), "another cell");
        assert!(!f.is_repeat((0, 4), 2000, 400), "too slow");
    }
}
