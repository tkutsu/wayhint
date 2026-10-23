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

use anyhow::{Context as _, Result, bail};
use xkbcommon::xkb;

use crate::action::Mods;

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
    pub const LEFT_SHIFT: Self = Self(42);
    const RIGHT_SHIFT: Self = Self(54);
    pub const LEFT_CTRL: Self = Self(29);
    const RIGHT_CTRL: Self = Self(97);
    pub const LEFT_ALT: Self = Self(56);

    /// A code as the input-method grab or `wl_keyboard` reports it.
    pub const fn from_evdev(code: u32) -> Self {
        Self(code)
    }

    /// The code as the virtual keyboard protocol takes it: evdev again.
    pub const fn evdev(self) -> u32 {
        self.0
    }

    fn to_xkb(self) -> xkb::Keycode {
        xkb::Keycode::new(self.0 + 8)
    }

    /// The modifier this key is, by position, or none. The overlay follows
    /// these like every other key, so Caps Lock never counts as Shift.
    /// Right Alt is left out: on many layouts it's `AltGr`, which picks a
    /// key's third character, and holding it means no Alt-click.
    pub const fn modifier(self) -> Mods {
        Mods {
            shift: self.0 == Self::LEFT_SHIFT.0 || self.0 == Self::RIGHT_SHIFT.0,
            ctrl: self.0 == Self::LEFT_CTRL.0 || self.0 == Self::RIGHT_CTRL.0,
            alt: self.0 == Self::LEFT_ALT.0,
        }
    }
}

/// The keys a config writes by name, in evdev codes. Names match in any
/// case (`home`, `Home`); [`name_of`] gives them back as written here.
const NAMED: [(&str, KeyCode); 14] = [
    ("Up", KeyCode(103)),
    ("Down", KeyCode(108)),
    ("Left", KeyCode(105)),
    ("Right", KeyCode(106)),
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

/// One xkb layout, compiled into a keymap, for turning config characters
/// into keys and keys back into the characters drawn on tags.
pub struct Layout {
    name: String,
    keymap: xkb::Keymap,
}

impl Layout {
    /// Compiles a layout from xkb's `layout` or `layout(variant)` form:
    /// `"us"`, `"de"`, `"us(colemak)"`.
    pub fn new(spec: &str) -> Result<Self> {
        let (layout, variant) = match spec.split_once('(') {
            Some((layout, rest)) => {
                let variant = rest
                    .strip_suffix(')')
                    .with_context(|| format!("layout {spec:?}: missing ')'"))?;
                (layout, variant)
            }
            None => (spec, ""),
        };
        if layout.is_empty() {
            bail!("layout {spec:?}: no layout name");
        }
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
        .with_context(|| format!("layout {spec:?}: xkb doesn't know it"))?;
        Ok(Self {
            name: spec.to_string(),
            keymap,
        })
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
        let us = Layout::new("us").unwrap();
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
        let gr = Layout::new("gr").unwrap();
        assert_eq!(gr.decode('α').map(|k| k.code), Some(KeyCode(30)));
        assert_eq!(gr.char_at(KeyCode(30)), Some('α'));
    }

    #[test]
    fn variant_form_and_unknown_layouts() {
        let colemak = Layout::new("us(colemak)").unwrap();
        // Colemak moves `s` to the position US calls `d`.
        assert_eq!(colemak.decode('s').map(|k| k.code), Some(KeyCode(32)));
        assert!(Layout::new("no-such-layout").is_err());
        assert!(Layout::new("us(colemak").is_err());
    }

    #[test]
    fn named_keys_match_in_any_case() {
        assert_eq!(named("pgup"), Some(KeyCode(104)));
        assert_eq!(named("HOME").and_then(name_of), Some("Home"));
        assert_eq!(named("PageUp"), None);
    }

    #[test]
    fn modifiers_go_by_position() {
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        assert_eq!(KeyCode(54).modifier(), shift, "right Shift");
        assert_eq!(KeyCode(58).modifier(), Mods::NONE, "Caps Lock isn't Shift");
        assert!(KeyCode(97).modifier().ctrl, "right Ctrl");
        assert_eq!(KeyCode(100).modifier(), Mods::NONE, "right Alt is AltGr");
        assert_eq!(KeyCode(30).modifier(), Mods::NONE, "a letter");
    }

    #[test]
    fn home_row_reads_asdfghjkl_on_us() {
        let us = Layout::new("us").unwrap();
        let printed: String = HOME_ROW.iter().filter_map(|&k| us.char_at(k)).collect();
        assert_eq!(printed, "asdfghjkl");
    }
}
