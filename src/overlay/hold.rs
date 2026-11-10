//! Held keys: arrows share one movement clock; text uses the compositor's
//! repeat settings. Pure: the time comes in as an argument.
//!
//! An arrow acts as it goes down, like a key repeat: then again every
//! `EVERY` once it has been down for `DELAY`, until it's let go. The input
//! method grab sends no repeats of its own, so wayhint keeps the clock.
//! How far a tick moves is `pick`'s to say: each one comes with its count,
//! so the pointer can speed up the longer the arrow is held.

use std::time::{Duration, Instant};

use crate::{geometry::Point, keys::KeyCode};

/// How long an arrow is down before it starts to repeat.
const DELAY: Duration = Duration::from_millis(250);
/// How often a held arrow ticks after that.
const EVERY: Duration = Duration::from_millis(25);

/// The latest held text key, using the compositor's repeat settings.
#[derive(Debug, Default)]
pub struct Repeat {
    timing: Option<(Duration, Duration)>,
    held: Option<(KeyCode, Instant)>,
}

impl Repeat {
    pub fn configure(&mut self, rate: u32, delay: u32) {
        self.timing = (rate > 0).then(|| {
            (
                Duration::from_millis(u64::from(delay)),
                Duration::from_secs(1) / rate,
            )
        });
        self.clear();
    }

    pub fn press(&mut self, code: KeyCode, now: Instant) {
        self.held = self.timing.map(|(delay, _)| (code, now + delay));
    }

    pub fn release(&mut self, code: KeyCode) {
        if self.held.is_some_and(|(held, _)| held == code) {
            self.clear();
        }
    }

    pub fn clear(&mut self) {
        self.held = None;
    }

    pub fn next(&self) -> Option<Instant> {
        self.held.map(|(_, next)| next)
    }

    /// Emit at most one repeat after a slow scan, avoiding a burst of text.
    pub fn due(&mut self, now: Instant) -> Option<KeyCode> {
        let (code, next) = self.held.as_mut()?;
        if *next > now {
            return None;
        }
        *next = now + self.timing?.1;
        Some(*code)
    }
}

#[derive(Debug, Default)]
pub struct Holds {
    keys: Vec<KeyCode>,
    /// Ticks so far: 0 until the first.
    ticks: u16,
    next: Option<Instant>,
}

impl Holds {
    /// An arrow went down at `now`, and its press counted.
    pub fn press(&mut self, code: KeyCode, now: Instant) {
        if self.keys.is_empty() {
            self.ticks = 0;
            self.next = Some(now + DELAY);
        }
        if !self.keys.contains(&code) {
            self.keys.push(code);
        }
    }

    /// A key was let go. True if it was a held arrow, which acted as it
    /// went down, so its release does nothing more.
    pub fn release(&mut self, code: KeyCode) -> bool {
        let before = self.keys.len();
        self.keys.retain(|&key| key != code);
        let released = self.keys.len() != before;
        if self.keys.is_empty() {
            self.clear();
        }
        released
    }

    /// Whether no arrow is down.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Forgets every arrow: new tags, or the keyboard handed back.
    pub fn clear(&mut self) {
        self.keys.clear();
        self.ticks = 0;
        self.next = None;
    }

    /// When the next tick is due, if an arrow is down.
    pub fn next(&self) -> Option<Instant> {
        self.next
    }

    /// The current combined direction, or none once every arrow is released.
    pub fn direction(&self) -> Option<Point> {
        if self.keys.is_empty() {
            return None;
        }
        let down = |code| i32::from(self.keys.contains(&code));
        Some(Point::new(
            down(KeyCode::RIGHT) - down(KeyCode::LEFT),
            down(KeyCode::DOWN) - down(KeyCode::UP),
        ))
    }

    /// One direction combining all held arrows, with its tick count.
    /// One tick each however late `now` is: a slow redraw slows the pointer
    /// down instead of making it jump.
    pub fn due(&mut self, now: Instant) -> Option<(Point, u16)> {
        if self.next? > now {
            return None;
        }
        self.ticks = self.ticks.saturating_add(1);
        self.next = Some(now + EVERY);
        Some((self.direction()?, self.ticks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_repeat_uses_the_seat_timing_and_stops_on_release() {
        let start = Instant::now();
        let a = KeyCode::from_evdev(30);
        let mut repeat = Repeat::default();
        repeat.configure(20, 300);
        repeat.press(a, start);
        assert_eq!(repeat.due(start + Duration::from_millis(299)), None);
        assert_eq!(repeat.due(start + Duration::from_millis(300)), Some(a));
        assert_eq!(repeat.next(), Some(start + Duration::from_millis(350)));
        repeat.release(KeyCode::BACKSPACE);
        assert_eq!(repeat.due(start + Duration::from_millis(900)), Some(a));
        assert_eq!(repeat.next(), Some(start + Duration::from_millis(950)));
        repeat.release(a);
        assert_eq!(repeat.next(), None);
        assert_eq!(repeat.due(start + Duration::from_secs(2)), None);
    }

    #[test]
    fn latest_text_key_replaces_repeat_and_clear_or_disable_cancels_it() {
        let start = Instant::now();
        let mut repeat = Repeat::default();
        repeat.configure(40, 250);
        repeat.press(KeyCode::SPACE, start);
        repeat.press(KeyCode::BACKSPACE, start + EVERY);
        repeat.release(KeyCode::SPACE);
        assert_eq!(repeat.due(start + DELAY), None);
        assert_eq!(repeat.due(start + DELAY + EVERY), Some(KeyCode::BACKSPACE));
        repeat.clear();
        assert_eq!(repeat.next(), None);
        repeat.press(KeyCode::SPACE, start);
        repeat.configure(0, 250);
        repeat.press(KeyCode::SPACE, start);
        assert_eq!(repeat.next(), None);
    }

    #[test]
    fn staggered_arrows_repeat_together_for_a_straight_diagonal() {
        let start = Instant::now();
        let mut holds = Holds::default();
        holds.press(KeyCode::UP, start);
        holds.press(KeyCode::LEFT, start + Duration::from_millis(12));
        let due = holds.due(start + DELAY);
        assert_eq!(
            due,
            Some((Point::new(-1, -1), 1)),
            "both axes must share each movement frame, even with staggered presses"
        );
    }

    #[test]
    fn a_held_arrow_ticks_after_the_delay_then_often() {
        let start = Instant::now();
        let mut holds = Holds::default();
        holds.press(KeyCode::UP, start);
        assert_eq!(holds.next(), Some(start + DELAY));
        assert_eq!(holds.due(start + DELAY / 2), None, "a tap never ticks");
        assert_eq!(holds.due(start + DELAY), Some((Point::new(0, -1), 1)));
        let late = start + DELAY + EVERY * 3;
        assert_eq!(
            holds.due(late),
            Some((Point::new(0, -1), 2)),
            "late: one tick"
        );
        assert_eq!(holds.next(), Some(late + EVERY));
        assert!(
            holds.release(KeyCode::UP),
            "a held arrow's release does nothing"
        );
        assert_eq!(holds.next(), None);
        assert!(!holds.release(KeyCode::UP));
    }

    #[test]
    fn releasing_one_axis_keeps_the_other_on_the_same_clock() {
        let start = Instant::now();
        let mut holds = Holds::default();
        holds.press(KeyCode::UP, start);
        holds.press(KeyCode::LEFT, start + EVERY);
        assert_eq!(holds.due(start + DELAY), Some((Point::new(-1, -1), 1)));
        assert!(holds.release(KeyCode::UP));
        let both = start + DELAY + EVERY;
        assert_eq!(
            holds.due(both),
            Some((Point::new(-1, 0), 2)),
            "releasing one axis must not restart the repeat delay or acceleration"
        );
        holds.clear();
        assert_eq!(holds.due(both + DELAY), None);
    }

    #[test]
    fn opposite_arrows_cancel_and_repressing_starts_fresh() {
        let start = Instant::now();
        let mut holds = Holds::default();
        holds.press(KeyCode::UP, start);
        holds.press(KeyCode::DOWN, start);
        holds.press(KeyCode::UP, start);
        assert_eq!(holds.due(start + DELAY), Some((Point::default(), 1)));
        holds.release(KeyCode::UP);
        holds.release(KeyCode::DOWN);
        let again = start + DELAY + EVERY;
        holds.press(KeyCode::LEFT, again);
        assert_eq!(holds.next(), Some(again + DELAY));
        assert_eq!(holds.due(again + DELAY), Some((Point::new(-1, 0), 1)));
    }
}
