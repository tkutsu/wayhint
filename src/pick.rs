//! Choosing one target: tags on screen, keys in, until a tag is typed or
//! the user leaves. The glue between `overlay` (pixels and keys) and
//! `selection` (what the keys mean), so neither has to know the other.

use anyhow::{Result, bail};

use crate::{
    action::Action,
    config::Config,
    geometry::{Point, Rect},
    keys::{Key, KeyCode},
    overlay::{Event, Overlay, Tag},
    selection::{Input, Selector, Step},
    snapshot::Target,
};

/// How far above the monitor's bottom edge the status pill sits, in px.
const PILL_LIFT: i32 = 48;

/// How a pick ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Do `action` at the target at `index`.
    Target { index: usize, action: Action },
    /// Do `action` where the pointer is now (`Space`).
    Here(Action),
    /// `Esc`, Ctrl+C, or the overlay went away: nothing chosen.
    Leave,
}

/// Shows tags for `targets` and reads keys until one is picked. `verb`
/// (from `--verb`) is armed as if its key were the first one typed.
pub fn pick(
    overlay: &mut Overlay,
    config: &Config,
    targets: &[Target],
    verb: Option<Action>,
) -> Result<Pick> {
    let mut selector = Selector::new(
        targets.len(),
        config.alphabet.len(),
        config.verbs.default_action(),
    );
    if let Some(action) = verb {
        selector.press(Input::Verb(action));
    }
    let screen = overlay.monitor().rect;
    overlay.show(&tags(&selector, config, targets, screen))?;
    loop {
        let input = match overlay.next_event()? {
            Event::Key(key) => match input_of(key, config) {
                Some(input) => input,
                None => continue,
            },
            Event::Interrupted | Event::Closed | Event::KeyboardLost => return Ok(Pick::Leave),
            Event::Resized => bail!("the monitor changed size; run wayhint again"),
        };
        match selector.press(input) {
            Step::Typing => overlay.update(&tags(&selector, config, targets, screen))?,
            Step::Picked { index, action } => return Ok(Pick::Target { index, action }),
            Step::Here(action) => return Ok(Pick::Here(action)),
            Step::Ignored => {}
            Step::Exit => return Ok(Pick::Leave),
        }
    }
}

/// What a key means: a built-in key, else a verb, else a tag letter, else
/// nothing. The config keeps verb keys out of the alphabet, so the order of
/// the last two never decides anything. Shift on a tag letter is kept for
/// Shift-click (spike test 9); until then it picks like the plain letter.
fn input_of(key: Key, config: &Config) -> Option<Input> {
    match key.code {
        KeyCode::ESC => Some(Input::Escape),
        KeyCode::BACKSPACE => Some(Input::Backspace),
        KeyCode::SPACE => Some(Input::Here),
        code => config
            .verbs
            .action_of(key)
            .map(Input::Verb)
            .or_else(|| config.alphabet.index_of(code).map(Input::Letter)),
    }
}

/// The tags still possible after what's been typed, drawn upper case.
///
/// Each tag reads as the whole command: everything typed so far, dimmed,
/// then the letters still to type. So an armed verb's key goes in front
/// (`kEA` after `k`, with `k` and nothing else dimmed), where the eyes
/// already are. Its name also shows as a pill at the bottom of `screen`,
/// for a verb that has no key (`--verb`) and for `Space`, which uses no
/// tag. The pill is drawn as one more tag: same look, no new drawing code.
fn tags(selector: &Selector, config: &Config, targets: &[Target], screen: Rect) -> Vec<Tag> {
    let letters = config.alphabet.letters();
    let prefix = selector
        .armed()
        .and_then(|action| config.verbs.label(action))
        .unwrap_or("");
    let typed = prefix.chars().count() + selector.typed().len();
    let mut tags: Vec<Tag> = selector
        .labels()
        .iter()
        .zip(targets)
        .filter(|(label, _)| selector.matches(label))
        .map(|(label, target)| Tag {
            at: target.hit,
            text: prefix
                .chars()
                .chain(label.iter().map(|&i| upper(letters[i].label)))
                .collect(),
            typed,
        })
        .collect();
    if let Some(action) = selector.armed() {
        tags.push(Tag {
            at: Point::new(screen.center().x, screen.bottom() - PILL_LIFT),
            text: action.name().into(),
            typed: 0,
        });
    }
    tags
}

/// A letter in upper case when that's one character (`ß` would be `SS`,
/// which would make the tag longer than the keys that type it).
fn upper(letter: char) -> char {
    let mut upper = letter.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => letter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_keeps_one_character() {
        assert_eq!(upper('a'), 'A');
        assert_eq!(upper('α'), 'Α');
        assert_eq!(upper(';'), ';');
        assert_eq!(upper('ß'), 'ß');
    }

    #[test]
    fn keys_decode_built_ins_then_verbs_then_letters() {
        let config = Config::parse("[keys]\nhover = \"_\"", || Ok("us".into())).unwrap();
        let key = |code, shift| Key {
            code: KeyCode::from_evdev(code),
            shift,
        };
        assert_eq!(input_of(key(57, false), &config), Some(Input::Here));
        assert_eq!(
            input_of(key(19, false), &config),
            Some(Input::Verb(Action::Right))
        );
        assert_eq!(
            input_of(key(12, true), &config),
            Some(Input::Verb(Action::Hover))
        );
        assert_eq!(
            input_of(key(12, false), &config),
            None,
            "- alone is nothing"
        );
        assert_eq!(input_of(key(30, true), &config), Some(Input::Letter(0)));
    }
}
