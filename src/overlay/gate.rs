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

use crate::keys::KeyCode;

#[derive(Debug, Default)]
pub struct KeyGate {
    /// Which drawing the gate waits for; bumped on every redraw.
    generation: u32,
    /// The round trip for `generation` has come back.
    armed: bool,
    /// Keys pressed while armed, not yet released.
    live: Vec<KeyCode>,
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

    pub fn press(&mut self, key: KeyCode) {
        if self.armed && !self.live.contains(&key) {
            self.live.push(key);
        }
    }

    /// Whether this release acts: only if its press counted.
    pub fn release(&mut self, key: KeyCode) -> bool {
        let before = self.live.len();
        self.live.retain(|&k| k != key);
        self.live.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: KeyCode = KeyCode::from_evdev(30);

    #[test]
    fn a_press_after_the_round_trip_acts_on_release() {
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.synced(generation);
        gate.press(A);
        assert!(gate.release(A));
        assert!(!gate.release(A), "one release per press");
    }

    #[test]
    fn a_press_before_the_round_trip_is_stale() {
        // A tap queued during the scan: press and release arrive early.
        let mut gate = KeyGate::default();
        let generation = gate.redrawn();
        gate.press(A);
        gate.synced(generation);
        assert!(!gate.release(A));
    }

    #[test]
    fn a_key_held_across_a_redraw_is_dropped() {
        let mut gate = KeyGate::default();
        let first = gate.redrawn();
        gate.synced(first);
        gate.press(A);
        let second = gate.redrawn();
        gate.synced(second);
        assert!(!gate.release(A), "its press was aimed at the old tags");
    }

    #[test]
    fn an_old_round_trip_arms_nothing() {
        let mut gate = KeyGate::default();
        let first = gate.redrawn();
        let _second = gate.redrawn();
        gate.synced(first);
        gate.press(A);
        assert!(!gate.release(A));
    }
}
