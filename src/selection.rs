//! Tags and typing them. Pure: no keys, screens or targets, only numbers
//! and the [`Act`] a pick carries.
//!
//! A label is a list of letter indices into the alphabet (`[0]` is the
//! first letter, `[8, 2]` the ninth then the third), so this module never
//! needs to know what the letters are or which keys type them. The overlay
//! turns key presses into [`Input`]s and draws whatever [`Selector`] says
//! is still possible.

use crate::{
    action::{Act, Action, Mods},
    geometry::Point,
};

/// Labels for `count` targets from an alphabet of `size` letters.
///
/// The rules, from the plan:
///
/// - Prefix-free: no label starts another, so typing never has to wait to
///   see whether more is coming.
/// - Shortest first, as short as possible: with up to `size` targets every
///   label is one letter.
/// - Priority order: earlier letters are used more. Prefixes come from the
///   end of the alphabet, so the strongest keys stay single-key labels.
/// - Deterministic: the same count always gives the same labels.
///
/// How: start with one label per letter. While there are too few, replace
/// the last of the shortest labels with itself plus each letter. Each step
/// trades one label for `size`, so the list grows by `size - 1`, and it
/// stays sorted by length because the expanded label was the last short one.
pub fn labels(count: usize, size: usize) -> Vec<Vec<usize>> {
    assert!(size >= 2, "the config guarantees two letters or more");
    let mut labels: Vec<Vec<usize>> = (0..size).map(|letter| vec![letter]).collect();
    while labels.len() < count {
        let shortest = labels[0].len();
        // The last label of the shortest length: just before the first longer one.
        let last = labels
            .iter()
            .rposition(|l| l.len() == shortest)
            .unwrap_or(0);
        let prefix = labels[last].clone();
        let children = (0..size).map(|letter| [prefix.as_slice(), &[letter]].concat());
        // `splice` replaces a range with the items of an iterator in place:
        // here the one prefix label with its `size` children.
        labels.splice(last..=last, children);
    }
    labels.truncate(count);
    labels
}

/// One thing the user did, already decoded from a key, with the modifiers
/// held on that key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A tag letter, by its index in the alphabet.
    Letter(usize, Mods),
    /// A verb key: the tag being typed gets this action instead of the
    /// default.
    Verb(Action, Mods),
    /// `Space`: act where the pointer already is, no tag.
    Here(Mods),
    /// An arrow: move where this command lands, by this many px.
    Nudge(Point),
    /// The repeat key: the last command again, on the same target.
    Repeat,
    Backspace,
    Escape,
}

/// What happened after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still typing: redraw, showing only the labels that match.
    Typing,
    /// A whole label was typed: do `act` at this target, by index.
    Picked { index: usize, act: Act },
    /// `Space`: do `act` where the pointer is.
    Here(Act),
    /// The repeat key. The run knows what the last command was.
    Repeat,
    /// A letter no label continues with. Nothing changes, nothing runs.
    Ignored,
    /// `Esc` with nothing typed or armed: leave.
    Exit,
}

/// The labels on screen, the letters typed so far, the armed verb and the
/// nudge.
///
/// A command is a tag or `Space`, with a verb and arrows anywhere before
/// its last key: `k e h`, `e k h` and `e → k h` all double-click tag
/// `eh`. Nothing ever waits on a timeout: a verb key is never a tag letter
/// (the config takes verb keys out of the alphabet), and labels are
/// prefix-free, so the last letter always acts. Arrows add up, and only
/// this command lands where they moved it.
#[derive(Debug, Clone)]
pub struct Selector {
    labels: Vec<Vec<usize>>,
    typed: Vec<usize>,
    /// The action a bare tag does: the config's `""` verb.
    default: Action,
    armed: Option<Action>,
    /// The armed verb's key needed Shift (`_` is Shift and `-`).
    armed_shifted: bool,
    /// How many letters were typed when the verb was, so Backspace takes
    /// back whichever came last.
    armed_at: usize,
    nudge: Point,
}

impl Selector {
    pub fn new(count: usize, alphabet_size: usize, default: Action) -> Self {
        Self {
            labels: labels(count, alphabet_size),
            typed: Vec::new(),
            default,
            armed: None,
            armed_shifted: false,
            armed_at: 0,
            nudge: Point::default(),
        }
    }

    pub fn labels(&self) -> &[Vec<usize>] {
        &self.labels
    }

    pub fn typed(&self) -> &[usize] {
        &self.typed
    }

    /// The verb typed before the tag, if any, for the status pill.
    pub fn armed(&self) -> Option<Action> {
        self.armed
    }

    /// How far the arrows have moved this command so far, for the dots
    /// that show where it lands.
    pub fn nudge(&self) -> Point {
        self.nudge
    }

    /// What a pick would do now, with `held` on its last key.
    ///
    /// `Option::unwrap_or` takes the armed verb if there is one; `Copy`
    /// types like `Action` are copied out, so the selector keeps its own.
    /// The modifiers are those on the last key, except a Shift that may be
    /// left over from a shifted verb: typing `_` then a tag fast, Shift is
    /// often still down on the tag, and that's no Shift-click. The cost: a
    /// shifted verb can't Shift-click.
    fn act(&self, held: Mods) -> Act {
        Act {
            action: self.armed.unwrap_or(self.default),
            mods: Mods {
                shift: held.shift && !self.armed_shifted,
                ..held
            },
            nudge: self.nudge,
        }
    }

    /// Drops the armed verb, if any.
    fn disarm(&mut self) {
        self.armed = None;
        self.armed_shifted = false;
    }

    /// Starts the next command: nothing typed, armed or nudged.
    fn reset(&mut self) {
        self.typed.clear();
        self.disarm();
        self.nudge = Point::default();
    }

    /// Whether anything of a command is in: letters, a verb or a nudge.
    fn started(&self) -> bool {
        !self.typed.is_empty() || self.armed.is_some() || self.nudge != Point::default()
    }

    /// Whether a label is still possible: it starts with what's typed.
    pub fn matches(&self, label: &[usize]) -> bool {
        label.starts_with(&self.typed)
    }

    /// Applies one input. `match` on the enum makes the compiler check that
    /// every kind of input is handled: add a variant and this stops building
    /// until it says what the new one does.
    pub fn press(&mut self, input: Input) -> Step {
        match input {
            Input::Letter(letter, held) => {
                let mut next = self.typed.clone();
                next.push(letter);
                if let Some(index) = self.labels.iter().position(|l| *l == next) {
                    let act = self.act(held);
                    self.reset();
                    Step::Picked { index, act }
                } else if self.labels.iter().any(|l| l.starts_with(&next)) {
                    self.typed = next;
                    Step::Typing
                } else {
                    Step::Ignored
                }
            }
            // Letters typed before a verb stay, as before an arrow: the verb
            // says what, the letters and the nudge where. A second verb
            // replaces the first, so a wrong one needs no Backspace.
            Input::Verb(action, held) => {
                self.armed = Some(action);
                self.armed_shifted = held.shift;
                self.armed_at = self.typed.len();
                Step::Typing
            }
            Input::Here(held) => {
                let act = self.act(held);
                self.reset();
                Step::Here(act)
            }
            // Letters typed stay, so a tag half typed can still be moved.
            Input::Nudge(by) => {
                self.nudge = self.nudge.offset(by);
                Step::Typing
            }
            // A whole command of its own, nudge included: whatever was
            // typed before it is dropped, a verb and arrows too.
            Input::Repeat => {
                self.reset();
                Step::Repeat
            }
            // Backspace takes back the last letter or verb, whichever came
            // last. The opposite arrow takes back a nudge.
            Input::Backspace => {
                if self.armed.is_some() && self.armed_at == self.typed.len() {
                    self.disarm();
                } else {
                    self.typed.pop();
                }
                Step::Typing
            }
            // Esc clears what's typed, armed and nudged first; only a
            // second Esc leaves.
            Input::Escape if self.started() => {
                self.reset();
                Step::Typing
            }
            Input::Escape => Step::Exit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Labels as text over "asdf", to make the tests readable.
    fn text(count: usize) -> Vec<String> {
        labels(count, 4)
            .iter()
            .map(|l| l.iter().map(|&i| "asdf".as_bytes()[i] as char).collect())
            .collect()
    }

    #[test]
    fn few_targets_get_single_letters_in_priority_order() {
        assert_eq!(text(0), Vec::<String>::new());
        assert_eq!(text(1), ["a"]);
        assert_eq!(text(4), ["a", "s", "d", "f"]);
    }

    #[test]
    fn prefixes_come_from_the_end_of_the_alphabet() {
        assert_eq!(text(5), ["a", "s", "d", "fa", "fs"]);
        assert_eq!(text(7), ["a", "s", "d", "fa", "fs", "fd", "ff"]);
        assert_eq!(text(8), ["a", "s", "da", "ds", "dd", "df", "fa", "fs"]);
        assert_eq!(text(16).len(), 16);
        assert!(text(16).iter().all(|l| l.len() == 2));
        assert_eq!(
            text(17)[0],
            "aa",
            "past size^2, the first letter becomes a prefix too"
        );
    }

    /// Five targets over four letters: a s d fa fs. Bare tags left-click.
    fn five() -> Selector {
        Selector::new(5, 4, Action::Left)
    }

    /// `action` with `mods` held, not nudged.
    const fn act(action: Action, mods: Mods) -> Act {
        Act {
            action,
            mods,
            nudge: Point::new(0, 0),
        }
    }

    const fn picked(index: usize, action: Action) -> Step {
        Step::Picked {
            index,
            act: act(action, Mods::NONE),
        }
    }

    /// Keys with nothing held, to keep the tests short.
    const fn letter(index: usize) -> Input {
        Input::Letter(index, Mods::NONE)
    }

    const fn verb(action: Action) -> Input {
        Input::Verb(action, Mods::NONE)
    }

    const SHIFT: Mods = Mods {
        shift: true,
        ..Mods::NONE
    };
    const CTRL: Mods = Mods {
        ctrl: true,
        ..Mods::NONE
    };

    #[test]
    fn typing_a_label_picks_it() {
        let mut s = five();
        assert_eq!(s.press(letter(3)), Step::Typing, "f is a prefix");
        assert!(s.matches(&[3, 0]) && !s.matches(&[0]));
        assert_eq!(s.press(letter(1)), picked(4, Action::Left), "fs");
        assert_eq!(s.typed(), [0; 0], "a pick starts over");
    }

    #[test]
    fn backspace_and_escape_step_back() {
        let mut s = five();
        s.press(letter(3));
        assert_eq!(s.press(Input::Backspace), Step::Typing);
        assert_eq!(s.typed(), [0; 0]);
        s.press(letter(3));
        assert_eq!(s.press(Input::Escape), Step::Typing, "first Esc clears");
        assert_eq!(s.press(Input::Escape), Step::Exit, "second Esc leaves");
    }

    #[test]
    fn a_verb_applies_to_the_next_pick_only() {
        let mut s = five();
        s.press(verb(Action::Right));
        assert_eq!(s.armed(), Some(Action::Right));
        assert_eq!(s.press(letter(0)), picked(0, Action::Right));
        assert_eq!(s.armed(), None, "the next tag is a bare one again");
        assert_eq!(s.press(letter(0)), picked(0, Action::Left));
    }

    #[test]
    fn a_verb_can_come_between_letters() {
        let mut s = five();
        s.press(letter(3));
        s.press(verb(Action::Right));
        assert_eq!((s.armed(), s.typed()), (Some(Action::Right), &[3][..]));
        assert_eq!(
            s.press(letter(1)),
            picked(4, Action::Right),
            "f r s is r fs"
        );
    }

    #[test]
    fn a_second_verb_replaces_the_first_and_keeps_letters() {
        let mut s = five();
        s.press(verb(Action::Right));
        s.press(letter(3));
        s.press(verb(Action::Double));
        assert_eq!((s.armed(), s.typed()), (Some(Action::Double), &[3][..]));
    }

    #[test]
    fn backspace_takes_back_the_last_letter_or_verb() {
        let mut s = five();
        s.press(verb(Action::Hover));
        s.press(letter(3));
        s.press(Input::Backspace);
        assert_eq!(s.armed(), Some(Action::Hover), "the letter went first");
        s.press(Input::Backspace);
        assert_eq!(s.armed(), None);
        s.press(letter(3));
        s.press(verb(Action::Hover));
        s.press(Input::Backspace);
        assert_eq!(
            (s.armed(), s.typed()),
            (None, &[3][..]),
            "the verb came last"
        );
        s.press(verb(Action::Hover));
        assert_eq!(s.press(Input::Escape), Step::Typing, "Esc disarms first");
        assert_eq!(s.press(Input::Escape), Step::Exit);
    }

    #[test]
    fn space_acts_where_the_pointer_is() {
        let mut s = five();
        let here = |action, mods| Step::Here(act(action, mods));
        assert_eq!(
            s.press(Input::Here(Mods::NONE)),
            here(Action::Left, Mods::NONE)
        );
        s.press(verb(Action::Middle));
        assert_eq!(s.press(Input::Here(CTRL)), here(Action::Middle, CTRL));
        assert_eq!(s.armed(), None);
    }

    /// One arrow press of 14 px.
    const RIGHT: Input = Input::Nudge(Point::new(14, 0));
    const UP: Input = Input::Nudge(Point::new(0, -14));

    #[test]
    fn arrows_add_up_and_go_with_the_pick() {
        let mut s = five();
        s.press(RIGHT);
        s.press(letter(3));
        s.press(RIGHT);
        s.press(UP);
        assert_eq!(s.nudge(), Point::new(28, -14), "around a half-typed tag");
        let Step::Picked { index: 4, act } = s.press(letter(1)) else {
            panic!("fs picks the fifth target");
        };
        assert_eq!(act.nudge, Point::new(28, -14));
        assert_eq!(
            s.nudge(),
            Point::default(),
            "the next command starts unmoved"
        );
    }

    #[test]
    fn a_verb_keeps_the_nudge_and_space_takes_it() {
        let mut s = five();
        s.press(UP);
        s.press(verb(Action::Right));
        let Step::Here(act) = s.press(Input::Here(Mods::NONE)) else {
            panic!("Space acts");
        };
        assert_eq!((act.action, act.nudge), (Action::Right, Point::new(0, -14)));
    }

    #[test]
    fn backspace_leaves_the_nudge_and_esc_clears_it() {
        let mut s = five();
        s.press(RIGHT);
        s.press(verb(Action::Hover));
        s.press(Input::Backspace);
        assert_eq!((s.armed(), s.nudge()), (None, Point::new(14, 0)));
        assert_eq!(
            s.press(Input::Escape),
            Step::Typing,
            "a nudge alone is a start"
        );
        assert_eq!(s.nudge(), Point::default());
        assert_eq!(s.press(Input::Escape), Step::Exit);
    }

    #[test]
    fn modifiers_on_the_last_letter_go_with_the_pick() {
        let mut s = five();
        s.press(Input::Letter(3, CTRL));
        let step = s.press(Input::Letter(1, SHIFT));
        assert_eq!(
            step,
            Step::Picked {
                index: 4,
                act: act(Action::Left, SHIFT)
            },
            "only the last letter's count"
        );
    }

    #[test]
    fn shift_left_over_from_a_shifted_verb_is_dropped() {
        let mut s = five();
        s.press(Input::Verb(Action::Double, SHIFT));
        let both = Mods {
            ctrl: true,
            ..SHIFT
        };
        assert_eq!(
            s.press(Input::Letter(0, both)),
            Step::Picked {
                index: 0,
                act: act(Action::Double, CTRL)
            }
        );
        assert_eq!(
            s.press(Input::Letter(0, SHIFT)),
            Step::Picked {
                index: 0,
                act: act(Action::Left, SHIFT)
            },
            "the next command starts clean"
        );
    }

    #[test]
    fn repeat_drops_what_was_typed() {
        let mut s = five();
        s.press(verb(Action::Right));
        s.press(letter(3));
        s.press(RIGHT);
        assert_eq!(s.press(Input::Repeat), Step::Repeat);
        assert_eq!((s.armed(), s.typed()), (None, &[][..]));
        assert_eq!(s.nudge(), Point::default(), "the repeat brings its own");
    }

    #[test]
    fn a_letter_no_label_continues_is_ignored() {
        let mut s = five();
        s.press(letter(3));
        assert_eq!(s.press(letter(3)), Step::Ignored, "no ff with 5 targets");
        assert_eq!(s.typed(), [3], "the prefix stays");
    }

    // Property tests: proptest makes up thousands of (count, size) pairs and
    // checks each rule holds for all of them, then shrinks any failure to the
    // smallest case that still fails.
    proptest! {
        #[test]
        fn labels_obey_every_rule(count in 0usize..600, size in 2usize..12) {
            let ls = labels(count, size);
            prop_assert_eq!(ls.len(), count);
            for (i, a) in ls.iter().enumerate() {
                for b in &ls[i + 1..] {
                    prop_assert!(!b.starts_with(a) && !a.starts_with(b), "{:?} and {:?}", a, b);
                }
            }
            prop_assert!(ls.windows(2).all(|w| w[0].len() <= w[1].len()), "shortest first");
            // As short as possible: the smallest length L with size^L >= count.
            let mut needed = 1;
            while size.pow(needed) < count {
                needed += 1;
            }
            let longest = ls.iter().map(Vec::len).max().unwrap_or(0);
            prop_assert!(longest <= needed as usize);
            prop_assert_eq!(labels(count, size), ls, "deterministic");
        }
    }
}
