//! Keeps the typed-input timeout independent of scans and message updates.

use std::time::{Duration, Instant};

use crate::status::Status;

pub struct Autohide {
    delay: Duration,
    status: Status,
    pub until: Option<Instant>,
    pub hidden: bool,
}

impl Autohide {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            status: Status::default(),
            until: None,
            hidden: false,
        }
    }

    /// Edits and mode changes reveal the input; scans and match counts do not.
    pub fn observe(&mut self, status: &Status, now: Instant) {
        let changed = self.status.mode != status.mode
            || self.status.input != status.input
            || self.status.verb != status.verb
            || self.status.holding != status.holding
            || self.status.line().is_some() != status.line().is_some();
        self.status = status.clone();
        if changed {
            self.reveal(now);
        }
    }

    /// A key reveals the current input even when its query stays the same.
    pub fn reveal(&mut self, now: Instant) -> bool {
        let hidden = self.hidden;
        self.hidden = false;
        self.until =
            (!self.delay.is_zero() && self.status.line().is_some()).then(|| now + self.delay);
        hidden
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|until| until <= now) {
            self.until = None;
            self.hidden = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Notice;

    #[test]
    fn scans_and_messages_do_not_extend_the_deadline_but_keys_and_edits_do() {
        let start = Instant::now();
        let delay = Duration::from_secs(3);
        let mut hide = Autohide::new(delay);
        let mut status = Status::plain("query");
        hide.observe(&status, start);
        assert_eq!(hide.until, Some(start + delay));
        status.info = Status::saying(Notice::NoMatch, "no match").info;
        hide.observe(&status, start + Duration::from_secs(2));
        assert!(!hide.expire(start + Duration::from_millis(2999)));
        assert!(hide.expire(start + delay));
        hide.observe(&status, start + Duration::from_secs(4));
        assert!(hide.hidden, "identical refreshes keep the input hidden");
        assert!(hide.reveal(start + Duration::from_secs(5)));
        assert_eq!(hide.until, Some(start + Duration::from_secs(8)));
        status.input.push('x');
        hide.observe(&status, start + Duration::from_secs(6));
        assert_eq!(hide.until, Some(start + Duration::from_secs(9)));
        hide.observe(&Status::default(), start + Duration::from_secs(7));
        assert_eq!(hide.until, None, "empty frames cancel the timeout");
    }

    #[test]
    fn zero_disables_autohide() {
        let start = Instant::now();
        let mut hide = Autohide::new(Duration::ZERO);
        hide.observe(&Status::plain("query"), start);
        assert_eq!(hide.until, None);
        assert!(!hide.expire(start + Duration::from_secs(60)));
        assert!(!hide.hidden);
    }
}
