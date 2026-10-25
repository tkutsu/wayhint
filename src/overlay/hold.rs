//! Arrows held down, and when each one ticks again. Pure: the time comes in
//! as an argument.
//!
//! An arrow acts as it goes down, like a key repeat: then again every
//! `EVERY` once it has been down for `DELAY`, until it's let go. The input
//! method grab sends no repeats of its own, so wayhint keeps the clock.
//! How far a tick moves is `pick`'s to say: each one comes with its count,
//! so the nudge can speed up the longer the arrow is held.

use std::time::{Duration, Instant};

use crate::{
    action::Mods,
    keys::{Key, KeyCode},
};

/// How long an arrow is down before it starts to repeat.
const DELAY: Duration = Duration::from_millis(250);
/// How often a held arrow ticks after that.
const EVERY: Duration = Duration::from_millis(25);

#[derive(Debug, Default)]
pub struct Holds(Vec<Hold>);

#[derive(Debug)]
struct Hold {
    /// The key and modifiers as they were at the press, as the gate keeps them.
    key: Key,
    mods: Mods,
    /// Ticks so far: 0 until the first.
    ticks: u16,
    next: Instant,
}

impl Holds {
    /// An arrow went down at `now`, and its press counted.
    pub fn press(&mut self, key: Key, mods: Mods, now: Instant) {
        if !self.0.iter().any(|h| h.key.code == key.code) {
            self.0.push(Hold {
                key,
                mods,
                ticks: 0,
                next: now + DELAY,
            });
        }
    }

    /// A key was let go. True if it was a held arrow, which acted as it
    /// went down, so its release does nothing more.
    pub fn release(&mut self, code: KeyCode) -> bool {
        let before = self.0.len();
        self.0.retain(|h| h.key.code != code);
        self.0.len() != before
    }

    /// Whether no arrow is down.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forgets every arrow: new tags, or the keyboard handed back.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// When the next tick is due, if an arrow is down.
    pub fn next(&self) -> Option<Instant> {
        self.0.iter().map(|h| h.next).min()
    }

    /// The arrows due a tick at `now`, with their count (1 for the first).
    /// One tick each however late `now` is: a slow redraw slows the nudge
    /// down instead of making it jump.
    pub fn due(&mut self, now: Instant) -> Vec<(Key, Mods, u16)> {
        self.0
            .iter_mut()
            .filter(|h| h.next <= now)
            .map(|h| {
                h.ticks = h.ticks.saturating_add(1);
                h.next = now + EVERY;
                (h.key, h.mods, h.ticks)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: Key = Key {
        code: KeyCode::UP,
        shift: false,
    };
    const LEFT: Key = Key {
        code: KeyCode::LEFT,
        shift: false,
    };

    #[test]
    fn a_held_arrow_ticks_after_the_delay_then_often() {
        let start = Instant::now();
        let mut holds = Holds::default();
        holds.press(UP, Mods::NONE, start);
        assert_eq!(holds.next(), Some(start + DELAY));
        assert_eq!(holds.due(start + DELAY / 2), [], "a tap never ticks");
        assert_eq!(holds.due(start + DELAY), [(UP, Mods::NONE, 1)]);
        let late = start + DELAY + EVERY * 3;
        assert_eq!(holds.due(late), [(UP, Mods::NONE, 2)], "late: one tick");
        assert_eq!(holds.next(), Some(late + EVERY));
        assert!(
            holds.release(UP.code),
            "a held arrow's release does nothing"
        );
        assert_eq!(holds.next(), None);
        assert!(!holds.release(UP.code));
    }

    #[test]
    fn two_arrows_held_tick_apart() {
        let start = Instant::now();
        let mut holds = Holds::default();
        holds.press(UP, Mods::NONE, start);
        holds.press(LEFT, Mods::NONE, start + EVERY);
        assert_eq!(holds.due(start + DELAY), [(UP, Mods::NONE, 1)]);
        let both = start + DELAY + EVERY;
        assert_eq!(
            holds.due(both),
            [(UP, Mods::NONE, 2), (LEFT, Mods::NONE, 1)],
            "up and left together go diagonally"
        );
        holds.clear();
        assert_eq!(holds.due(both + DELAY), []);
    }
}
