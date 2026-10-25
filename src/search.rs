//! Text mode: the tags go, typing finds text on screen, and Enter acts on
//! the current match. The glue between `overlay` (keys and pixels), `text`
//! (what the keys find) and the apps ([`Source`]), as `pick` is for tags.
//!
//! ```text
//! text key -> tags hidden -> text read once -> keys: query edits, arrows
//!                                                |
//!                 Esc / Backspace with nothing typed: back to the tags
//!                 Enter: the armed verb (or the default) on the current match
//! ```
//!
//! The text is read once, as text mode starts. Where a match is, the app is
//! asked only for matches it wasn't asked about before, and only once the
//! keys typed so far are all in. Nothing is scanned again while text mode
//! is up; the match is checked again before acting on it (see `run.rs`).

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action},
    desktop,
    geometry::Rect,
    keys::KeyCode,
    overlay::{Event, Frame, Highlight, Overlay},
    pick::{Pick, Source, pill},
    selection::Selector,
    snapshot::{Snapshot, Target},
    text::Search,
};

/// A match chosen with Enter: what to do, the target it makes, and what its
/// text must still read for the run to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// The text's object, its box and where the match shows: clicked at the
    /// middle of its first visible line, and re-checked as a tag's target.
    pub target: Target,
    pub act: Act,
    /// Characters `start..end` of the object, and the text they held. `None`
    /// for a text that is a name: the re-check reads the name anyway.
    pub check: Option<(u32, u32, String)>,
}

/// Text mode, from the text key until a match is chosen (`Some` pick), or
/// until Esc or Backspace with nothing typed go back to the tags (`None`).
/// `selector` holds the verb and the nudge typed before the text key.
pub fn search(
    overlay: &mut Overlay,
    shown: &Snapshot,
    selector: &Selector,
    source: &mut dyn Source,
) -> Result<Option<Pick>> {
    if let Err(e) = overlay.start_typing() {
        desktop::warn(&format!("wayhint: no text mode: {e:#}"));
        return Ok(None);
    }
    let result = keys(overlay, shown, selector, source);
    overlay.stop_typing();
    result
}

/// Text mode's keys, with the overlay typing.
fn keys(
    overlay: &mut Overlay,
    shown: &Snapshot,
    selector: &Selector,
    source: &mut dyn Source,
) -> Result<Option<Pick>> {
    let screen = overlay.monitor().rect;
    let verb = selector.armed().map(Action::name);
    // The tags go before the text is read: none is left to be typed.
    overlay.update(&frame(None, false, verb, screen))?;
    let mut search = Search::new(source.read_text(shown));
    overlay.update(&frame(Some(&search), false, verb, screen))?;
    // The query changed since the matches were found.
    let mut stale = false;
    loop {
        let Some(event) = overlay.next_event(None)? else {
            continue;
        };
        let (key, mods, typed) = match event {
            Event::Typed(key, mods, typed) => (key, mods, typed),
            // Enter, on its release.
            Event::Key(key, mods) => (key, mods, None),
            Event::Held(..) => continue,
            Event::Interrupted | Event::Closed | Event::KeyboardLost => {
                return Ok(Some(Pick::Leave));
            }
            Event::Resized => bail!("the monitor changed size; run wayhint again"),
            Event::Workspace => return Ok(Some(Pick::Rescan)),
        };
        let edit = match key.code {
            // Esc clears the query and Backspace takes back its last
            // character; with nothing typed, both go back to the tags.
            KeyCode::ESC => {
                if !search.clear() {
                    return Ok(None);
                }
                true
            }
            KeyCode::BACKSPACE => {
                if !search.pop() {
                    return Ok(None);
                }
                true
            }
            KeyCode::ENTER
            | KeyCode::RIGHT
            | KeyCode::DOWN
            | KeyCode::LEFT
            | KeyCode::UP
            | KeyCode::TAB
            | KeyCode::HOME
            | KeyCode::END => false,
            // Ctrl and Alt type nothing; AltGr isn't Alt.
            _ => match typed {
                Some(c) if !mods.ctrl && !mods.alt => {
                    search.push(c);
                    true
                }
                _ => continue,
            },
        };
        stale |= edit;
        // Every key typed so far first: fast typing asks the apps once.
        // A key that moves through the matches needs them found.
        if stale && (!edit || !overlay.has_events()?) {
            find(&mut search, shown, source);
            stale = false;
        }
        match key.code {
            KeyCode::ENTER => {
                if let Some(chosen) = choose(&search, shown, selector.act(mods)) {
                    return Ok(Some(Pick::Text(Box::new(chosen))));
                }
            }
            KeyCode::RIGHT | KeyCode::DOWN => search.next(),
            KeyCode::TAB if !mods.shift => search.next(),
            KeyCode::LEFT | KeyCode::UP | KeyCode::TAB => search.previous(),
            KeyCode::HOME => search.first(),
            KeyCode::END => search.last(),
            _ => {}
        }
        overlay.update(&frame(Some(&search), stale, verb, screen))?;
    }
}

/// Finds the query's matches: asks the apps where the new ones are, then
/// keeps those that show.
fn find(search: &mut Search, shown: &Snapshot, source: &mut dyn Source) {
    let wanted = search.wanted();
    if !wanted.is_empty() {
        let boxes = source.locate(shown, search.texts(), &wanted);
        search.located(&wanted, boxes);
    }
    search.update(&|window, at| source.shows(shown, window, at));
}

/// The current match as a command: the text's object as the target, and
/// the text to read again.
fn choose(search: &Search, shown: &Snapshot, act: Act) -> Option<Chosen> {
    let found = search.current()?;
    let text = &search.texts()[found.span.text];
    let source = &shown.texts[text.source];
    let part = found.parts[0];
    let (start, end) = (found.span.start, found.span.end);
    Some(Chosen {
        target: Target {
            object: Some(source.object.clone()),
            window: text.window,
            rect: text.rect,
            visible: part,
            hit: part.center(),
            role: source.role.clone(),
            name: text.name.clone(),
            editable: source.editable,
            clips: source.clips.clone(),
            opens: Vec::new(),
        },
        act,
        check: (!source.named).then(|| (start, end, text.chars(start, end))),
    })
}

/// What text mode shows: every match's lines, the current one's marked,
/// and the pill. `search` is `None` while the text is being read, and
/// `stale` while the query's matches are being found: the pill shows `...`
/// for the count then, and nothing is marked. The pill names the armed
/// `verb`, dimmed, before the query.
fn frame(search: Option<&Search>, stale: bool, verb: Option<&str>, screen: Rect) -> Frame {
    let prefix = verb.map(|v| format!("{v} ")).unwrap_or_default();
    let typed = prefix.chars().count();
    let status = match search {
        None => "text ...".to_string(),
        Some(search) if search.query().is_empty() => "text".to_string(),
        Some(search) if stale => format!("{}  ...", search.query()),
        Some(search) => {
            let (at, count) = search.position();
            format!("{}  {at}/{count}", search.query())
        }
    };
    let search = search.filter(|_| !stale);
    let current = search.and_then(Search::current).map(|m| m.span);
    let highlights = search
        .map(|search| {
            search
                .matches()
                .iter()
                .flat_map(|m| {
                    let current = Some(m.span) == current;
                    m.parts.iter().map(move |&rect| Highlight { rect, current })
                })
                .collect()
        })
        .unwrap_or_default();
    Frame {
        tags: vec![pill(screen, format!("{prefix}{status}"), typed)],
        dots: Vec::new(),
        highlights,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{geometry::Point, text::Text};

    fn search(contents: &[&str]) -> Search {
        let texts = contents
            .iter()
            .zip(0..)
            .map(|(content, row)| Text {
                source: 0,
                window: 0,
                name: String::new(),
                rect: Rect::new(0, row * 20, 300, 20),
                area: Rect::new(0, 0, 1000, 1000),
                start: 0,
                content: (*content).into(),
            })
            .collect();
        Search::new(texts)
    }

    /// Types `query` and places each match on its text's row, 10 px a
    /// character.
    fn typed(search: &mut Search, query: &str) {
        query.chars().for_each(|c| search.push(c));
        let wanted = search.wanted();
        #[allow(clippy::cast_possible_wrap)]
        let boxes = wanted
            .iter()
            .map(|s| {
                let y = search.texts()[s.text].rect.y;
                let x = s.start as i32 * 10;
                vec![Rect::new(x, y, (s.end - s.start) as i32 * 10, 20)]
            })
            .collect();
        search.located(&wanted, boxes);
        search.update(&|_, _: Point| true);
    }

    const SCREEN: Rect = Rect::new(0, 0, 1920, 1080);

    #[test]
    fn the_tags_go_and_the_pill_says_what_is_typed() {
        let loading = frame(None, false, None, SCREEN);
        assert_eq!(loading.tags.len(), 1, "only the pill");
        assert_eq!(loading.tags[0].text, "text ...");
        let mut found = search(&["contact", "falcon"]);
        let pill = |found: &Search, stale| {
            frame(Some(found), stale, Some("double"), SCREEN).tags[0].clone()
        };
        assert_eq!(pill(&found, false).text, "double text");
        typed(&mut found, "con");
        assert_eq!(pill(&found, false).text, "double con  1/2");
        assert_eq!(
            pill(&found, false).typed,
            "double ".len(),
            "the verb is dimmed"
        );
        assert_eq!(pill(&found, true).text, "double con  ...", "being found");
    }

    #[test]
    fn every_match_is_highlighted_and_the_current_one_marked() {
        let mut found = search(&["contact", "falcon"]);
        typed(&mut found, "con");
        found.next();
        let shown = frame(Some(&found), false, None, SCREEN);
        let marks: Vec<(i32, bool)> = shown
            .highlights
            .iter()
            .map(|h| (h.rect.y, h.current))
            .collect();
        assert_eq!(marks, [(0, false), (20, true)]);
        assert!(
            frame(Some(&found), true, None, SCREEN)
                .highlights
                .is_empty(),
            "stale: none"
        );
        found.clear();
        found.update(&|_, _| true);
        let cleared = frame(Some(&found), false, None, SCREEN);
        assert!(
            cleared.highlights.is_empty(),
            "nothing typed, nothing marked"
        );
    }
}
