//! Keyboard → GamePad mapping. Keys are identified by name (`"X"`,
//! `"ArrowUp"`, ...) so this crate stays independent of the GUI toolkit.
use std::collections::BTreeMap;

use wiiu_peripherals::{GamePadState, VpadButtons, controller::Stick};

/// Directions of the left stick.
const STICK_KEYS: [&str; 4] = ["stick_up", "stick_down", "stick_left", "stick_right"];

/// Action name → key name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct KeyBindings(pub BTreeMap<String, String>);

impl Default for KeyBindings {
    fn default() -> Self {
        let pairs = [
            ("a", "X"),
            ("b", "Z"),
            ("x", "S"),
            ("y", "A"),
            ("l", "Q"),
            ("r", "W"),
            ("zl", "1"),
            ("zr", "2"),
            ("plus", "Enter"),
            ("minus", "Backspace"),
            ("home", "H"),
            ("up", "ArrowUp"),
            ("down", "ArrowDown"),
            ("left", "ArrowLeft"),
            ("right", "ArrowRight"),
            ("stick_up", "I"),
            ("stick_down", "K"),
            ("stick_left", "J"),
            ("stick_right", "L"),
        ];
        Self(pairs.into_iter().map(|(a, k)| (a.to_owned(), k.to_owned())).collect())
    }
}

/// Button action names and their VPAD bits.
const BUTTONS: [(&str, VpadButtons); 15] = [
    ("a", VpadButtons::A),
    ("b", VpadButtons::B),
    ("x", VpadButtons::X),
    ("y", VpadButtons::Y),
    ("l", VpadButtons::L),
    ("r", VpadButtons::R),
    ("zl", VpadButtons::ZL),
    ("zr", VpadButtons::ZR),
    ("plus", VpadButtons::PLUS),
    ("minus", VpadButtons::MINUS),
    ("home", VpadButtons::HOME),
    ("up", VpadButtons::UP),
    ("down", VpadButtons::DOWN),
    ("left", VpadButtons::LEFT),
    ("right", VpadButtons::RIGHT),
];

impl KeyBindings {
    /// Builds a GamePad sample from `is_down(key_name)`.
    pub fn gamepad(&self, is_down: impl Fn(&str) -> bool) -> GamePadState {
        let down = |action: &str| self.0.get(action).is_some_and(|key| is_down(key));
        let held = BUTTONS
            .iter()
            .filter(|(action, _)| down(action))
            .fold(VpadButtons::empty(), |acc, (_, b)| acc | *b);
        let [up, dn, lf, rt] = STICK_KEYS.map(down);
        let axis = |neg: bool, pos: bool| f32::from(i8::from(pos) - i8::from(neg));
        GamePadState {
            held,
            left_stick: Stick { x: axis(lf, rt), y: axis(dn, up) },
            ..GamePadState::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys_to_buttons_and_stick() {
        let bindings = KeyBindings::default();
        let pad = bindings.gamepad(|key| matches!(key, "X" | "ArrowUp" | "L"));
        assert_eq!(pad.held, VpadButtons::A | VpadButtons::UP);
        assert_eq!((pad.left_stick.x, pad.left_stick.y), (1.0, 0.0));
    }
}
