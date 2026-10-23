//! Which key releases count. Pure: no Wayland, only the rule.
//!
//! The rule, from the plan's "Reading keys": a release counts only if its
//! press arrived after the current tags were up. After drawing new tags the
//! overlay asks the compositor for a round trip (`wl_display.sync`); its
//! answer comes after every key event the compositor had already queued, so
//! a press seen before the answer was aimed at tags that are gone. No
//! timestamps: Wayland key times have no defined start, so they can't be
//! compared with when wayhint drew.
//!
//! The one rule covers keys typed during a scan, a key held across a
//! redraw, and the keybind's own keys still held when wayhint starts.
//!
//! Each press is kept with the modifiers held at that moment, and the
//! release hands them back: people often let go of Shift before the key,
//! and `Shift+-` still meant `_`; Ctrl let go a moment early still meant a
//! Ctrl-click.

use crate::{
    action::Mods,
    keys::{Key, KeyCode},
};

#[derive(Debug, Default)]
pub struct KeyGate {
    /// Which drawing the gate waits for; bumped on every redraw.
    generation: u32,
    /// The round trip for `generation` has come back.
    armed: bool,
    /// Keys pressed while armed, not yet released, with their modifiers.
    live: Vec<(Key, Mods)>,
}

impl KeyGate {
    /// New tags were committed: forget every press, wait for the round trip.
    /// Returns the generation the round trip must answer for.
    pub fn redrawn(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1);
        self.armed = false;
        self.live.clear();
        self.generation
    }

    /// A round trip came back. One sent for an older drawing arms nothing.
    pub fn synced(&mut self, generation: u32) {
        if generation == self.generation {
            self.armed = true;
        }
    }

    pub fn press(&mut self, key: Key, mods: Mods) {
        if self.armed && !self.live.iter().any(|(k, _)| k.code == key.code) {
            self.live.push((key, mods));
        }
    }

    /// The key and modifiers as they were at the press, if this release
    /// acts: only if its press counted.
    pub fn release(&mut self, code: KeyCode) -> Option<(Key, Mods)> {
        let index = self.live.iter().position(|(k, _)| k.code == code)?;
        Some(self.live.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Key = Key {
        code: KeyCode::from_evdev(30),
        shift: false,
    };

    #[test]
    fn a_press_after_the_round_trip_acts_on_release() {
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.synced(generation);
        gate.press(A, Mods::NONE);
        assert_eq!(gate.release(A.code), Some((A, Mods::NONE)));
        assert_eq!(gate.release(A.code), None, "one release per press");
    }

    #[test]
    fn modifiers_are_what_they_were_at_the_press() {
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.synced(generation);
        let shifted = Key { shift: true, ..A };
        let mods = Mods {
            shift: true,
            ctrl: true,
            alt: false,
        };
        gate.press(shifted, mods);
        assert_eq!(gate.release(A.code), Some((shifted, mods)));
    }

    #[test]
    fn a_press_before_the_round_trip_is_stale() {
        // A tap queued during the scan: press and release arrive early.
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.press(A, Mods::NONE);
        gate.synced(generation);
        assert_eq!(gate.release(A.code), None);
    }

    #[test]
    fn a_key_held_across_a_redraw_is_dropped() {
        let mut gate = KeyGate::default();
        let first = gate.redrawn();
        gate.synced(first);
        gate.press(A, Mods::NONE);
        let second = gate.redrawn();
        gate.synced(second);
        assert_eq!(
            gate.release(A.code),
            None,
            "its press was aimed at the old tags"
        );
    }

    #[test]
    fn an_old_round_trip_arms_nothing() {
        let mut gate = KeyGate::default();
        let first = gate.redrawn();
        let _second = gate.redrawn();
        gate.synced(first);
        gate.press(A, Mods::NONE);
        assert_eq!(gate.release(A.code), None);
    }
}
