//! The characters keys type, for text mode's query.
//!
//! Tags go by position (see `keys.rs`); a query is text, so here, and only
//! here, a key is read as the active layout types it: with Shift and `AltGr`,
//! on the second layout (Greek), and with dead keys composed (on the Greek
//! layout `;` is the tonos, and `;` then `ι` is `ί`). Caps Lock is left
//! out: it changes nothing in wayhint, the query included.
//!
//! The modifiers and the layout come from the compositor with each key, so
//! the only state kept here is a dead key waiting for the next.

use xkbcommon::xkb::{self, compose};

use crate::{action::Mods, keyboard::Locks, keys::KeyCode};

/// Caps Lock's bit in an xkb modifier mask.
const CAPS_LOCK: u32 = 1 << 1;

pub struct Typist {
    state: xkb::State,
    /// `None` when the locale has no compose table: a dead key types nothing.
    compose: Option<compose::State>,
}

impl Typist {
    /// For `keymap`, xkb text as the compositor sent it, composing as the
    /// locale says (`LC_ALL`, `LC_CTYPE`, `LANG`).
    pub fn new(keymap: &str) -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            keymap.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::COMPILE_NO_FLAGS,
        )?;
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|name| std::env::var_os(name).filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "C".into());
        let compose = compose::Table::new_from_locale(&context, &locale, compose::COMPILE_NO_FLAGS)
            .ok()
            .map(|table| compose::State::new(&table, compose::STATE_NO_FLAGS));
        Some(Self {
            state: xkb::State::new(&keymap),
            compose,
        })
    }

    /// The character `code` types with the modifiers `held` (depressed and
    /// latched, as the compositor sent them) and `locks`, if it types one
    /// now: none for a dead key, which waits for the next, or a key that
    /// prints nothing.
    pub fn press(&mut self, code: KeyCode, held: u32, locks: Locks) -> Option<char> {
        self.state
            .update_mask(held, 0, locks.locked & !CAPS_LOCK, 0, 0, locks.group);
        let key = code.to_xkb();
        let sym = self.state.key_get_one_sym(key);
        // A key the compose table has no use for (Shift, most letters) is
        // ignored by it, and types as it is.
        let status = self.compose.as_mut().and_then(|compose| {
            (compose.feed(sym) == compose::FeedResult::Accepted).then(|| compose.status())
        });
        let text = match (status, &mut self.compose) {
            (Some(compose::Status::Composing), _) => return None,
            (Some(compose::Status::Composed), Some(compose)) => {
                let text = compose.utf8();
                compose.reset();
                text?
            }
            (Some(compose::Status::Cancelled), _) => {
                self.reset();
                return None;
            }
            _ => self.state.key_get_utf8(key),
        };
        // One printable character; a sequence that composes to several is
        // rare enough to drop.
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if !c.is_control() => Some(c),
            _ => None,
        }
    }

    /// The character a key pressed before typing started types: only the
    /// Shift held at its press is known (Shift is bit 0 of an xkb mask).
    /// Typed while the text was being read, such keys are the query's
    /// first letters.
    pub fn press_late(&mut self, code: KeyCode, held: Mods, locks: Locks) -> Option<char> {
        self.press(code, u32::from(held.shift), locks)
    }

    /// Forgets a dead key waiting for the next.
    pub fn reset(&mut self) {
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keymap the compositor sends for `kb_layout = us,gr`.
    fn us_gr() -> String {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            "us,gr",
            "",
            None,
            xkb::COMPILE_NO_FLAGS,
        )
        .expect("xkb knows us and gr");
        keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    }

    const A: KeyCode = KeyCode::from_evdev(30);
    const I: KeyCode = KeyCode::from_evdev(23);
    const SEMICOLON: KeyCode = KeyCode::from_evdev(39);
    const SHIFT: u32 = 1;

    #[test]
    fn keys_type_as_the_active_layout_prints_them() {
        let mut typist = Typist::new(&us_gr()).unwrap();
        let us = Locks::default();
        let greek = Locks { group: 1, ..us };
        assert_eq!(typist.press(A, 0, us), Some('a'));
        assert_eq!(typist.press(A, SHIFT, us), Some('A'));
        assert_eq!(typist.press(A, 0, greek), Some('α'));
        let caps = Locks {
            locked: CAPS_LOCK,
            ..us
        };
        assert_eq!(
            typist.press(A, 0, caps),
            Some('a'),
            "Caps Lock changes nothing"
        );
        assert_eq!(typist.press(KeyCode::ENTER, 0, us), None);
    }

    #[test]
    fn a_dead_key_composes_with_the_next() {
        let mut typist = Typist::new(&us_gr()).unwrap();
        let greek = Locks {
            group: 1,
            ..Locks::default()
        };
        if typist.compose.is_none() {
            return; // no compose table for this locale: nothing to test
        }
        assert_eq!(typist.press(SEMICOLON, 0, greek), None, "the tonos waits");
        assert_eq!(typist.press(I, 0, greek), Some('ί'));
        assert_eq!(typist.press(I, 0, greek), Some('ι'), "only once");
        typist.press(SEMICOLON, 0, greek);
        typist.reset();
        assert_eq!(typist.press(I, 0, greek), Some('ι'), "reset forgets it");
    }
}
