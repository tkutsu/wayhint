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
//!
//! Keys act in the order they went down. Fast typing overlaps keys: `ba`
//! typed as b down, a down, a up, b up must not act on `a` first. A
//! release waits for the keys pressed before it, and goes with them when
//! the earliest comes up. A held modifier waits for nothing and holds
//! nothing back: Shift held over several letters is the usual case.
//!
//! [`Seen`] answers the other side: which releases belong to the app.

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
    /// Keys pressed while armed, in press order, until they act.
    live: Vec<Press>,
}

#[derive(Debug)]
struct Press {
    key: Key,
    mods: Mods,
    /// Let go, and waiting for an earlier key.
    up: bool,
    /// Set a modifier: waits for nothing while held.
    modifier: bool,
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

    /// Whether the press counts: it came after the round trip, and its key
    /// isn't already down.
    pub fn press(&mut self, key: Key, mods: Mods) -> bool {
        let counts = self.armed && !self.live.iter().any(|p| p.key.code == key.code);
        if counts {
            self.live.push(Press {
                key,
                mods,
                up: false,
                modifier: false,
            });
        }
        counts
    }

    /// The last key pressed set a modifier (the compositor says so right
    /// after the press): it holds no other key back.
    pub fn modifier(&mut self) {
        if let Some(press) = self.live.last_mut() {
            press.modifier = true;
        }
    }

    /// A key that acted as it went down (an arrow, a typed key): its
    /// release brings nothing, and keys pressed after it don't wait for it.
    pub fn forget(&mut self, code: KeyCode) {
        self.live.retain(|p| p.key.code != code);
    }

    /// The keys that act now, each with the modifiers as they were at its
    /// press, oldest first: this one, if its press counted, once every
    /// earlier key is up too, with the keys that were waiting for it.
    pub fn release(&mut self, code: KeyCode) -> Vec<(Key, Mods)> {
        let Some(press) = self.live.iter_mut().find(|p| p.key.code == code) else {
            return Vec::new();
        };
        press.up = true;
        let mut ready = Vec::new();
        let mut i = 0;
        while let Some(press) = self.live.get(i) {
            if press.up {
                let press = self.live.remove(i);
                ready.push((press.key, press.mods));
            } else if press.modifier {
                i += 1;
            } else {
                break;
            }
        }
        ready
    }
}

/// The keys a grab has seen go down, counted or not. Any other key let go
/// in the grab went down before it: the app saw that press, the grab took
/// the release, and the app holds the key until it's told. Super, from the
/// keybind that started wayhint, is the usual one.
#[derive(Debug, Default)]
pub struct Seen(Vec<KeyCode>);

impl Seen {
    pub fn press(&mut self, code: KeyCode) {
        if !self.0.contains(&code) {
            self.0.push(code);
        }
    }

    /// Whether this release is the app's: true when its press came before
    /// the grab.
    pub fn release(&mut self, code: KeyCode) -> bool {
        match self.0.iter().position(|&c| c == code) {
            Some(index) => {
                self.0.swap_remove(index);
                false
            }
            None => true,
        }
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
        assert_eq!(gate.release(A.code), [(A, Mods::NONE)]);
        assert_eq!(gate.release(A.code), [], "one release per press");
    }

    #[test]
    fn modifiers_are_what_they_were_at_the_press() {
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.synced(generation);
        let shifted = Key { shift: true, ..A };
        let mods = Mods {
            ctrl: true,
            alt: false,
        };
        gate.press(shifted, mods);
        assert_eq!(gate.release(A.code), [(shifted, mods)]);
    }

    #[test]
    fn a_press_before_the_round_trip_is_stale() {
        // A tap queued during the scan: press and release arrive early.
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.press(A, Mods::NONE);
        gate.synced(generation);
        assert_eq!(gate.release(A.code), []);
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
            [],
            "its press was aimed at the old tags"
        );
    }

    #[test]
    fn a_release_whose_press_came_before_the_grab_is_the_apps() {
        // SUPER+W: Super went down before the grab, and is let go in it.
        let super_key = KeyCode::from_evdev(125);
        let mut seen = Seen::default();
        seen.press(A.code);
        assert!(seen.release(super_key), "the app saw Super go down");
        assert!(!seen.release(A.code), "the grab saw A go down");
        assert!(seen.release(A.code), "a second release is no press of ours");
    }

    #[test]
    fn an_old_round_trip_arms_nothing() {
        let mut gate = KeyGate::default();
        let first = gate.redrawn();
        let _second = gate.redrawn();
        gate.synced(first);
        gate.press(A, Mods::NONE);
        assert_eq!(gate.release(A.code), []);
    }

    const B: Key = Key {
        code: KeyCode::from_evdev(48),
        shift: false,
    };
    const SHIFT: Key = Key {
        code: KeyCode::from_evdev(42),
        shift: false,
    };

    fn armed() -> KeyGate {
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.synced(generation);
        gate
    }

    #[test]
    fn overlapping_keys_act_in_the_order_they_went_down() {
        // `ba` typed fast: a comes up before b. Acting on a first would
        // pick a one-letter tag `a` instead of typing `ba`.
        let mut gate = armed();
        gate.press(B, Mods::NONE);
        gate.press(A, Mods::NONE);
        assert_eq!(gate.release(A.code), [], "a waits for b");
        assert_eq!(gate.release(B.code), [(B, Mods::NONE), (A, Mods::NONE)]);
    }

    #[test]
    fn a_held_modifier_holds_no_key_back() {
        let mut gate = armed();
        gate.press(SHIFT, Mods::NONE);
        gate.modifier();
        gate.press(Key { shift: true, ..A }, Mods::NONE);
        assert_eq!(
            gate.release(A.code),
            [(Key { shift: true, ..A }, Mods::NONE)]
        );
        assert_eq!(gate.release(SHIFT.code), [(SHIFT, Mods::NONE)]);
    }

    #[test]
    fn a_key_that_acted_on_its_press_holds_no_key_back() {
        let mut gate = armed();
        gate.press(B, Mods::NONE);
        gate.forget(B.code);
        gate.press(A, Mods::NONE);
        assert_eq!(gate.release(A.code), [(A, Mods::NONE)]);
        assert_eq!(gate.release(B.code), [], "its release brings nothing");
    }
}
