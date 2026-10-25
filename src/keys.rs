//! Physical keys, and decoding the characters a config file is written in.
//!
//! wayhint matches keys by position, never by character, so a second layout
//! (Greek) can't break the tags. Positions are evdev codes: what the
//! input-method grab and SCTK's `KeyEvent::raw_code` report. xkbcommon
//! numbers the same keys 8 higher, and mixing the two is an easy bug (Space
//! is evdev 57, and xkb 57 is `n`), so evdev codes get a type of their own
//! and the +8 happens in exactly one place, `KeyCode::to_xkb`.
//!
//! The config is written the other way round, in characters: `alphabet =
//! "ciaehtns"`. [`Layout`] turns each character into the key that types it on
//! the user's first layout, once, at load time.

use anyhow::{Context as _, Result};
use xkbcommon::xkb;

/// A key by physical position: its evdev code.
///
/// The field is private, so the only ways to make one are `from_evdev` (the
/// grab's numbers) and the layout lookup below (xkb's numbers, converted
/// once). That's the newtype pattern: same bits as a `u32`, but the compiler
/// won't let an xkb code stand in for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyCode(u32);

impl KeyCode {
    pub const ESC: Self = Self(1);
    pub const BACKSPACE: Self = Self(14);
    pub const TAB: Self = Self(15);
    pub const ENTER: Self = Self(28);
    pub const SPACE: Self = Self(57);
    pub const UP: Self = Self(103);
    pub const DOWN: Self = Self(108);
    pub const LEFT: Self = Self(105);
    pub const RIGHT: Self = Self(106);
    // The keys a modifier click presses on the virtual keyboard. Which
    // modifiers the user holds comes from the compositor instead (see
    // `keyboard::held`): the keymap can put Ctrl on any key.
    pub const LEFT_SHIFT: Self = Self(42);
    pub const LEFT_CTRL: Self = Self(29);
    pub const LEFT_ALT: Self = Self(56);

    /// A code as the input-method grab or `wl_keyboard` reports it.
    pub const fn from_evdev(code: u32) -> Self {
        Self(code)
    }

    /// Whether this is one of the four arrows, which nudge and repeat.
    pub const fn is_arrow(self) -> bool {
        matches!(self, Self::UP | Self::DOWN | Self::LEFT | Self::RIGHT)
    }

    /// The code as the virtual keyboard protocol takes it: evdev again.
    pub const fn evdev(self) -> u32 {
        self.0
    }

    fn to_xkb(self) -> xkb::Keycode {
        xkb::Keycode::new(self.0 + 8)
    }
}

/// The keys a config writes by name, in evdev codes. Names match in any
/// case (`home`, `Home`); [`name_of`] gives them back as written here.
const NAMED: [(&str, KeyCode); 14] = [
    ("Up", KeyCode::UP),
    ("Down", KeyCode::DOWN),
    ("Left", KeyCode::LEFT),
    ("Right", KeyCode::RIGHT),
    ("PgUp", KeyCode(104)),
    ("PgDn", KeyCode(109)),
    ("Home", KeyCode(102)),
    ("End", KeyCode(107)),
    ("Delete", KeyCode(111)),
    ("Tab", KeyCode::TAB),
    ("Space", KeyCode::SPACE),
    ("Enter", KeyCode::ENTER),
    ("Esc", KeyCode::ESC),
    ("Backspace", KeyCode::BACKSPACE),
];

/// The key a name stands for: `"pgup"` is Page Up.
pub fn named(name: &str) -> Option<KeyCode> {
    NAMED
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, code)| code)
}

/// A named key's name, for printing the config back.
pub fn name_of(code: KeyCode) -> Option<&'static str> {
    NAMED.iter().find(|&&(_, c)| c == code).map(|&(n, _)| n)
}

/// Every name, for the message when a config uses an unknown one.
pub fn names() -> impl Iterator<Item = &'static str> {
    NAMED.iter().map(|&(n, _)| n)
}

/// The 47 printable keys of the main block, in evdev codes: the number row,
/// the three letter rows, and the punctuation around them. Tags and command
/// keys only ever come from here, so a keypad `1` can't stand in for the `1`
/// above `q`.
const MAIN_BLOCK: [std::ops::RangeInclusive<u32>; 4] = [2..=13, 16..=27, 30..=41, 43..=53];

/// The home row, `asdfghjkl` on US: the default alphabet, as positions, so
/// it's the home row on every layout (`qsdfghjkl` printed on AZERTY).
pub const HOME_ROW: [KeyCode; 9] = [
    KeyCode(30),
    KeyCode(31),
    KeyCode(32),
    KeyCode(33),
    KeyCode(34),
    KeyCode(35),
    KeyCode(36),
    KeyCode(37),
    KeyCode(38),
];

fn main_block() -> impl Iterator<Item = KeyCode> {
    MAIN_BLOCK.into_iter().flatten().map(KeyCode)
}

/// A key plus whether Shift is part of it: on US, `"` is
/// `Key { code: 40, shift: true }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub shift: bool,
}

/// The first layout of a keymap, for turning config characters into keys
/// and keys back into the characters drawn on tags. The first, because a
/// second one (Greek) is the one you switch to, and tags work there too.
pub struct Layout {
    name: String,
    keymap: xkb::Keymap,
}

impl Layout {
    /// The first layout of `keymap`, xkb text as the compositor sends it
    /// ([`crate::keyboard::seat_keymap`]). It's the keymap your keys really
    /// type with, however it was made: a layout name, a custom layout, or a
    /// whole keymap file (Hyprland's `kb_file`).
    pub fn from_keymap(keymap: &str) -> Result<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            keymap.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::COMPILE_NO_FLAGS,
        )
        .context("the keyboard's keymap doesn't compile")?;
        Ok(Self::of(keymap))
    }

    /// A layout by its xkb names, `("us", "colemak")`, for tests: they
    /// can't ask a compositor.
    ///
    /// # Panics
    ///
    /// If xkb doesn't know the layout.
    #[cfg(test)]
    pub fn named(layout: &str, variant: &str) -> Self {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        // RMLVO: rules, model, layout, variant, options. "evdev" and "pc105"
        // are what every Linux desktop uses; options don't move printable keys.
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            layout,
            variant,
            None,
            xkb::COMPILE_NO_FLAGS,
        )
        .expect("xkb knows the layout");
        Self::of(keymap)
    }

    /// Named as the keymap names its first layout: "English (US)".
    fn of(keymap: xkb::Keymap) -> Self {
        Self {
            name: keymap.layout_get_name(0).to_string(),
            keymap,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The character a key types at a shift level (0 plain, 1 Shift) on this
    /// layout's first group, if it types a printable one.
    fn char_at_level(&self, code: KeyCode, level: u32) -> Option<char> {
        let syms = self.keymap.key_get_syms_by_level(code.to_xkb(), 0, level);
        let &[sym] = syms else { return None };
        char::from_u32(xkb::keysym_to_utf32(sym)).filter(|c| !c.is_control() && *c != '\0')
    }

    /// What a key prints without Shift, for drawing it on a tag.
    pub fn char_at(&self, code: KeyCode) -> Option<char> {
        self.char_at_level(code, 0)
    }

    /// The main-block key that types `ch`: unshifted if any key does,
    /// otherwise shifted. `None` if this layout can't type it there.
    pub fn decode(&self, ch: char) -> Option<Key> {
        [(0, false), (1, true)]
            .into_iter()
            .find_map(|(level, shift)| {
                main_block()
                    .find(|&code| self.char_at_level(code, level) == Some(ch))
                    .map(|code| Key { code, shift })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests read the system's xkb data (`xkeyboard-config`), which every
    // desktop and CI image with libxkbcommon has.

    #[test]
    fn decodes_plain_and_shifted_us_keys() {
        let us = Layout::named("us", "");
        assert_eq!(
            us.decode('a'),
            Some(Key {
                code: KeyCode(30),
                shift: false
            })
        );
        assert_eq!(
            us.decode('"'),
            Some(Key {
                code: KeyCode(40),
                shift: true
            })
        );
        assert_eq!(
            us.decode('1'),
            Some(Key {
                code: KeyCode(2),
                shift: false
            }),
            "number row, not keypad"
        );
        assert_eq!(us.decode('α'), None);
    }

    #[test]
    fn same_position_prints_differently_per_layout() {
        let gr = Layout::named("gr", "");
        assert_eq!(gr.decode('α').map(|k| k.code), Some(KeyCode(30)));
        assert_eq!(gr.char_at(KeyCode(30)), Some('α'));
        let colemak = Layout::named("us", "colemak");
        // Colemak moves `s` to the position US calls `d`.
        assert_eq!(colemak.decode('s').map(|k| k.code), Some(KeyCode(32)));
    }

    #[test]
    fn a_sent_keymap_decodes_with_its_first_layout() {
        // What the compositor sends for `kb_layout = us,gr`: one keymap,
        // two layouts. The config is read with the first.
        let both = Layout::named("us,gr", "");
        let sent = both.keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        let layout = Layout::from_keymap(&sent).unwrap();
        assert_eq!(layout.name(), "English (US)");
        assert_eq!(layout.decode('a').map(|k| k.code), Some(KeyCode(30)));
        assert_eq!(layout.decode('α'), None, "Greek is the second layout");
    }

    #[test]
    fn named_keys_match_in_any_case() {
        assert_eq!(named("pgup"), Some(KeyCode(104)));
        assert_eq!(named("HOME").and_then(name_of), Some("Home"));
        assert_eq!(named("PageUp"), None);
    }

    #[test]
    fn home_row_reads_asdfghjkl_on_us() {
        let us = Layout::named("us", "");
        let printed: String = HOME_ROW.iter().filter_map(|&k| us.char_at(k)).collect();
        assert_eq!(printed, "asdfghjkl");
    }
}
