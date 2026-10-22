//! Choosing one target: tags on screen, keys in, until a tag is typed or
//! the user leaves. The glue between `overlay` (pixels and keys) and
//! `selection` (what the keys mean), so neither has to know the other.

use anyhow::Result;

use crate::{
    config::Alphabet,
    keys::KeyCode,
    overlay::{Event, Overlay, Tag},
    selection::{Input, Selector, Step},
    snapshot::Target,
};

/// How a pick ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The target at this index was chosen.
    Target(usize),
    /// `Esc`, Ctrl+C, or the overlay went away: nothing chosen.
    Leave,
}

/// Shows tags for `targets` and reads keys until one is picked.
pub fn pick(overlay: &mut Overlay, alphabet: &Alphabet, targets: &[Target]) -> Result<Pick> {
    let mut selector = Selector::new(targets.len(), alphabet.len());
    overlay.show(&tags(&selector, alphabet, targets))?;
    loop {
        let input = match overlay.next_event()? {
            Event::Key(KeyCode::ESC) => Input::Escape,
            Event::Key(KeyCode::BACKSPACE) => Input::Backspace,
            // Any other key: a tag letter, or ignored.
            Event::Key(code) => match alphabet.index_of(code) {
                Some(letter) => Input::Letter(letter),
                None => continue,
            },
            Event::Interrupted | Event::Closed | Event::KeyboardLost => return Ok(Pick::Leave),
        };
        match selector.press(input) {
            Step::Typing => overlay.update(&tags(&selector, alphabet, targets))?,
            Step::Picked(index) => return Ok(Pick::Target(index)),
            Step::Ignored => {}
            Step::Exit => return Ok(Pick::Leave),
        }
    }
}

/// The tags still possible after what's been typed, drawn upper case.
fn tags(selector: &Selector, alphabet: &Alphabet, targets: &[Target]) -> Vec<Tag> {
    let letters = alphabet.letters();
    selector
        .labels()
        .iter()
        .zip(targets)
        .filter(|(label, _)| selector.matches(label))
        .map(|(label, target)| Tag {
            at: target.hit,
            text: label.iter().map(|&i| upper(letters[i].label)).collect(),
            typed: selector.typed().len(),
        })
        .collect()
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
}
