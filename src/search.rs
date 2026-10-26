//! Text mode: the tags go, typing finds text on screen, and Enter acts on
//! the current match. The glue between `overlay` (keys and pixels), `text`
//! (what the keys find) and the apps ([`Source`]), as `pick` is for tags.
//!
//! ```text
//! text key -> tags hidden -> text read once -> keys: query edits, arrows
//!                                                |
//!                 Backspace with nothing typed: back to the tags
//!                 Esc: leave wayhint
//!                 Enter: the armed verb (or the default) on the current match
//!                 one match left (with [text] pick_unique): as Enter
//!                 a character that matches nothing: dropped, and named
//! ```
//!
//! The text is read once, as text mode starts. Where a match is, the app is
//! asked only for matches it wasn't asked about before, and only once the
//! keys typed so far are all in. Nothing is scanned again while text mode
//! is up; the match is checked again before acting on it (see `run.rs`).

use std::time::Instant;

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action, Mods},
    config::{Config, TextCursor},
    desktop,
    geometry::Point,
    keys::KeyCode,
    overlay::{Event, Frame, Highlight, Overlay},
    pick::{Pick, Source},
    selection::Selector,
    snapshot::{Snapshot, Target},
    status::{Message, Status},
    style::{Input, Notice},
    text::{Check, Place, Search},
};

/// How many characters `[text] pick_unique` waits for before acting on
/// the only match: fewer match too much by chance.
pub const UNIQUE: usize = 3;

/// A match chosen with Enter: what to do, the target it makes, and what its
/// text must still read for the run to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// The text's object, its box and where the match shows: clicked at the
    /// configured cursor position, and re-checked as a tag's target.
    /// A terminal's text has no object: its window is re-checked, as for a
    /// tag found in pixels.
    pub target: Target,
    pub act: Act,
    /// What the match must still read. `None` for a text that is a name:
    /// the re-check reads the name anyway.
    pub check: Option<Check>,
}

/// Text mode, from the text key until a match is chosen or Esc leaves
/// (`Some` pick), or until Backspace with nothing typed goes back to the
/// tags (`None`).
/// `selector` holds the verb and the nudge typed before the text key.
pub fn search(
    overlay: &mut Overlay,
    shown: &Snapshot,
    selector: &Selector,
    source: &mut dyn Source,
    config: &Config,
) -> Result<Option<Pick>> {
    if let Err(e) = overlay.start_typing() {
        desktop::warn(&format!("wayhint: no text mode: {e:#}"));
        return Ok(None);
    }
    let result = keys(overlay, shown, selector, source, config);
    overlay.stop_typing();
    result
}

/// Text mode's keys, with the overlay typing.
fn keys(
    overlay: &mut Overlay,
    shown: &Snapshot,
    selector: &Selector,
    source: &mut dyn Source,
    config: &Config,
) -> Result<Option<Pick>> {
    let (input, cursor) = (&config.input, config.text_cursor);
    let verb = selector.armed().map(Action::name);
    // The tags go before the text is read: none is left to be typed.
    overlay.update(&frame(None, false, verb, input, None))?;
    let mut search = Search::new(source.read_text(shown));
    overlay.update(&frame(Some(&search), false, verb, input, None))?;
    // The query changed since the matches were found.
    let mut stale = false;
    // What was typed that matched nothing, and until when it's named.
    let mut missed: Option<(String, Instant)> = None;
    loop {
        let Some(event) = overlay.next_event(missed.as_ref().map(|m| m.1))? else {
            missed = None;
            overlay.update(&frame(Some(&search), stale, verb, input, None))?;
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
            // Esc leaves, as with the tags. Backspace takes back the last
            // character; with nothing typed, it goes back to the tags.
            KeyCode::ESC => return Ok(Some(Pick::Leave)),
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
            let dropped = config.info.no_match
                && key.code != KeyCode::BACKSPACE
                && drop_unmatched(&mut search, shown, source).is_some_and(|tried| {
                    let until = Instant::now() + config.info.duration;
                    missed = Some((format!("no text has \"{tried}\""), until));
                    true
                });
            // Typed into one match, not backed into it or left there by
            // a miss.
            if config.pick_unique
                && !dropped
                && key.code != KeyCode::BACKSPACE
                && unique(&search)
                && let Some(chosen) = choose(&search, shown, selector.act(Mods::NONE), cursor)
            {
                return Ok(Some(Pick::Text(Box::new(chosen))));
            }
        }
        match key.code {
            KeyCode::ENTER => {
                if let Some(chosen) = choose(&search, shown, selector.act(mods), cursor) {
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
        let said = missed.as_ref().map(|m| m.0.as_str());
        overlay.update(&frame(Some(&search), stale, verb, input, said))?;
    }
}

/// Takes back the characters at the query's end that leave nothing to
/// match, as typed with the keys that matched nothing: a fast typist's
/// keys after a miss go with it. The query as typed if any went.
fn drop_unmatched(
    search: &mut Search,
    shown: &Snapshot,
    source: &mut dyn Source,
) -> Option<String> {
    let tried = search.query().to_owned();
    while search.matches().is_empty() && search.pop() {
        find(search, shown, source);
    }
    (search.query() != tried).then_some(tried)
}

/// Whether the query is long enough, `UNIQUE` characters, and has just one
/// match.
fn unique(search: &Search) -> bool {
    search.query().chars().count() >= UNIQUE && search.matches().len() == 1
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
fn choose(search: &Search, shown: &Snapshot, act: Act, cursor: TextCursor) -> Option<Chosen> {
    let found = search.current()?;
    let text = &search.texts()[found.span.text];
    let part = match cursor {
        TextCursor::Start | TextCursor::Center => found.parts[0],
        TextCursor::End => *found.parts.last()?,
    };
    let hit = Point::new(
        match cursor {
            TextCursor::Start => part.x,
            TextCursor::Center => part.center().x,
            TextCursor::End => part.right() - 1,
        },
        part.center().y,
    );
    let (start, end) = (found.span.start, found.span.end);
    let check = Check {
        place: text.place.clone(),
        start,
        end,
        text: text.chars(start, end),
    };
    let (object, role, editable, clips, check) = match text.place {
        Place::Object(index) => {
            let source = &shown.texts[index];
            let check = (!source.named).then_some(check);
            let object = Some(source.object.clone());
            (
                object,
                source.role.clone(),
                source.editable,
                source.clips.clone(),
                check,
            )
        }
        Place::Cells(_) => (None, "terminal".into(), false, Vec::new(), Some(check)),
        Place::Glyphs(_) => (None, "picture".into(), false, Vec::new(), Some(check)),
    };
    Some(Chosen {
        target: Target {
            object,
            window: text.window,
            rect: text.rect,
            visible: part,
            hit,
            role,
            name: text.name.clone(),
            editable,
            clips,
            opens: Vec::new(),
        },
        act,
        check,
    })
}

/// What text mode shows: every match's lines, the current one's marked,
/// and the input box. `search` is `None` while the text is being read, and
/// `stale` while the query's matches are being found: the box shows `...`
/// for the count then, and nothing is marked. The box names the armed
/// `verb`, dimmed, before the query; with no query yet, it shows
/// `[input] placeholder`, dimmed too. Under it, `missed` says what was
/// typed that matched nothing.
fn frame(
    search: Option<&Search>,
    stale: bool,
    verb: Option<&'static str>,
    input: &Input,
    missed: Option<&str>,
) -> Frame {
    let status = Status {
        info: missed.map(|m| Message::new(Notice::NoMatch, m)),
        ..Status::text(
            verb,
            search.map(Search::query),
            search.filter(|_| !stale).map(Search::position),
            input,
        )
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
        highlights,
        status,
        ..Frame::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::Rect,
        style::Style,
        text::{Span, Text},
    };

    fn search(contents: &[&str]) -> Search {
        let texts = contents
            .iter()
            .zip(0..)
            .map(|(content, row)| Text {
                place: crate::text::Place::Object(0),
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

    fn frame(search: Option<&Search>, stale: bool, verb: Option<&'static str>) -> Frame {
        super::frame(search, stale, verb, &Input::new(&Style::default()), None)
    }

    /// Apps that place every span on its text's row, 10 px a character,
    /// all in view.
    struct Rows;

    impl Source for Rows {
        fn rescan(&mut self, _: &mut dyn FnMut()) -> Result<Option<Snapshot>> {
            unreachable!("text mode never scans")
        }

        fn keep(&self, _: &mut Snapshot) {
            unreachable!("text mode opens no block")
        }

        fn read_text(&mut self, _: &Snapshot) -> Vec<Text> {
            unreachable!("the texts are given")
        }

        #[allow(clippy::cast_possible_wrap)]
        fn locate(&mut self, _: &Snapshot, texts: &[Text], spans: &[Span]) -> Vec<Vec<Rect>> {
            spans
                .iter()
                .map(|s| {
                    let y = texts[s.text].rect.y;
                    let x = s.start as i32 * 10;
                    vec![Rect::new(x, y, (s.end - s.start) as i32 * 10, 20)]
                })
                .collect()
        }

        fn shows(&self, _: &Snapshot, _: usize, _: Point) -> bool {
            true
        }
    }

    #[test]
    fn characters_that_match_nothing_are_dropped_together() {
        let shown = Snapshot::build(1, 0, std::iter::empty());
        let mut found = search(&["contact", "falcon"]);
        "con".chars().for_each(|c| found.push(c));
        find(&mut found, &shown, &mut Rows);
        assert_eq!(drop_unmatched(&mut found, &shown, &mut Rows), None);
        // Typed fast: t matches contact, then x and y match nothing.
        "txy".chars().for_each(|c| found.push(c));
        find(&mut found, &shown, &mut Rows);
        assert_eq!(
            drop_unmatched(&mut found, &shown, &mut Rows).as_deref(),
            Some("contxy")
        );
        assert_eq!(found.query(), "cont");
        assert_eq!(found.matches().len(), 1, "contact is still found");
        let mut nothing = search(&["contact"]);
        nothing.push('q');
        find(&mut nothing, &shown, &mut Rows);
        assert_eq!(
            drop_unmatched(&mut nothing, &shown, &mut Rows).as_deref(),
            Some("q")
        );
        assert_eq!(nothing.query(), "", "nothing left to match");
    }

    #[test]
    fn one_match_is_unique_once_the_query_is_long_enough() {
        let texts = ["contact", "falcon", "context"];
        let mut found = search(&texts);
        typed(&mut found, "fa");
        assert_eq!(found.matches().len(), 1);
        assert!(!unique(&found), "two characters match too much by chance");
        typed(&mut found, "l");
        assert!(unique(&found), "falcon, after three");
        let mut found = search(&texts);
        typed(&mut found, "con");
        assert!(!unique(&found), "three matches");
        typed(&mut found, "ta");
        assert!(unique(&found), "contact alone");
    }

    #[test]
    fn the_tags_go_and_the_pill_says_what_is_typed() {
        let loading = frame(None, false, None);
        assert!(loading.tags.is_empty(), "target tags are hidden");
        assert_eq!(loading.status.line().unwrap().0, "text ...");
        let mut found = search(&["contact", "falcon"]);
        let pill = |found: &Search, stale| {
            frame(Some(found), stale, Some("double"))
                .status
                .line()
                .unwrap()
        };
        assert_eq!(pill(&found, false).0, "double text");
        assert_eq!(
            pill(&found, false).1,
            "double text".len(),
            "the placeholder is dimmed"
        );
        typed(&mut found, "con");
        assert_eq!(pill(&found, false).0, "double con  1/2");
        assert_eq!(pill(&found, false).1, "double ".len(), "the verb is dimmed");
        assert_eq!(pill(&found, true).0, "double con  ...", "being found");
    }

    #[test]
    fn query_decorations_leave_matching_and_status_intact() {
        let mut found = search(&["άλλο", "Άλλο"]);
        typed(&mut found, "αλλ");
        let input = Input {
            prefix: "find [".into(),
            suffix: "]".into(),
            ..Input::new(&Style::default())
        };
        let display = |stale| {
            super::frame(Some(&found), stale, Some("double"), &input, None)
                .status
                .line()
                .unwrap()
        };
        assert_eq!(display(false).0, "double find [αλλ]  1/2");
        assert_eq!(display(true).0, "double find [αλλ]  ...");
        assert_eq!(display(false).1, 7, "only the armed verb is dimmed");
        assert_eq!(found.query(), "αλλ", "decorations are display only");
        let empty = Input {
            placeholder: "find…".into(),
            ..input
        };
        let waiting = super::frame(Some(&search(&["x"])), false, None, &empty, None);
        assert_eq!(waiting.status.line().unwrap().0, "find…");
        let reading = super::frame(None, false, None, &empty, None);
        assert_eq!(reading.status.line().unwrap().0, "find… ...");
    }

    #[test]
    fn every_match_is_highlighted_and_the_current_one_marked() {
        let mut found = search(&["contact", "falcon"]);
        typed(&mut found, "con");
        found.next();
        let shown = frame(Some(&found), false, None);
        let marks: Vec<(i32, bool)> = shown
            .highlights
            .iter()
            .map(|h| (h.rect.y, h.current))
            .collect();
        assert_eq!(marks, [(0, false), (20, true)]);
        assert!(
            frame(Some(&found), true, None).highlights.is_empty(),
            "stale: none"
        );
        while found.pop() {}
        found.update(&|_, _| true);
        let cleared = frame(Some(&found), false, None);
        assert!(
            cleared.highlights.is_empty(),
            "nothing typed, nothing marked"
        );
    }

    #[test]
    fn a_terminal_match_is_clicked_in_its_cells_and_read_again() {
        let cells = crate::text::Cells {
            pane: 3,
            window: Point::new(100, 0),
            first: Point::new(0, 0),
            cell: (10, 20),
            scale: (1, 1),
            at: (0..7).map(|col| (0, col, col + 1)).collect(),
        };
        let mut found = Search::new(vec![Text {
            rect: cells.whole(),
            place: Place::Cells(cells.clone()),
            window: 0,
            name: "fish".into(),
            area: Rect::new(0, 0, 1000, 1000),
            start: 0,
            content: "find me".into(),
        }]);
        "me".chars().for_each(|c| found.push(c));
        let wanted = found.wanted();
        let boxes = wanted.iter().map(|s| cells.boxes(s.start, s.end)).collect();
        found.located(&wanted, boxes);
        found.update(&|_, _| true);
        let shown = Snapshot::build(1, 0, std::iter::empty());
        let act = Act {
            action: Action::Left,
            mods: crate::action::Mods::NONE,
            nudge: Point::default(),
        };
        for (cursor, x) in [
            (TextCursor::Start, 150),
            (TextCursor::Center, 160),
            (TextCursor::End, 169),
        ] {
            let chosen = choose(&found, &shown, act, cursor).expect("a match");
            assert_eq!(chosen.target.object, None, "nothing to ask: a pixel target");
            assert_eq!(chosen.target.role, "terminal");
            assert_eq!(chosen.target.hit, Point::new(x, 10));
            assert!(chosen.target.visible.contains(chosen.target.hit));
            let check = chosen.check.expect("read again before acting");
            assert_eq!((check.start, check.end, check.text.as_str()), (5, 7, "me"));
            assert_eq!(check.place, Place::Cells(cells.clone()));
        }
    }

    #[test]
    fn text_cursor_uses_visible_line_edges_and_keeps_the_recheck() {
        use crate::{desktop::tree::ObjectId, snapshot::TextSource};

        let object = ObjectId {
            bus: ":1.42".into(),
            path: "/text".into(),
        };
        let area = Rect::new(-90, 0, 60, 40);
        let mut shown = Snapshot::build(1, 0, std::iter::empty());
        shown.texts.push(TextSource {
            object: object.clone(),
            window: 0,
            role: "text".into(),
            editable: true,
            named: false,
            area,
            clips: vec![object.clone()],
            button: None,
        });
        let mut found = Search::new(vec![Text {
            place: Place::Object(0),
            window: 0,
            name: "editor".into(),
            rect: Rect::new(-100, 0, 80, 40),
            area,
            start: 0,
            content: "save as".into(),
        }]);
        "save as".chars().for_each(|c| found.push(c));
        let wanted = found.wanted();
        found.located(
            &wanted,
            vec![vec![
                Rect::new(-100, 0, 40, 20),
                Rect::new(-100, 20, 80, 20),
            ]],
        );
        found.update(&|_, _| true);
        let act = Act {
            action: Action::Right,
            mods: crate::action::Mods {
                shift: true,
                ..crate::action::Mods::NONE
            },
            nudge: Point::new(3, -2),
        };
        for (cursor, hit, visible) in [
            (
                TextCursor::Start,
                Point::new(-90, 10),
                Rect::new(-90, 0, 30, 20),
            ),
            (
                TextCursor::Center,
                Point::new(-75, 10),
                Rect::new(-90, 0, 30, 20),
            ),
            (
                TextCursor::End,
                Point::new(-31, 30),
                Rect::new(-90, 20, 60, 20),
            ),
        ] {
            let chosen = choose(&found, &shown, act, cursor).expect("a wrapped match");
            assert_eq!(chosen.target.hit, hit);
            assert_eq!(chosen.target.visible, visible);
            assert!(visible.contains(hit));
            assert_eq!(chosen.target.object, Some(object.clone()));
            assert_eq!(chosen.target.clips, std::slice::from_ref(&object));
            assert!(chosen.target.editable);
            assert_eq!(chosen.act, act, "keep the verb, modifiers and nudge");
            let check = chosen.check.expect("read the whole match again");
            assert_eq!(
                (check.start, check.end, check.text.as_str()),
                (0, 7, "save as")
            );
            assert_eq!(check.place, Place::Object(0));
        }
    }
}
