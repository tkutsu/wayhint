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
//! ```
//!
//! The text is read once, as text mode starts. Where a match is, the app is
//! asked only for matches it wasn't asked about before, and only once the
//! keys typed so far are all in. Nothing is scanned again while text mode
//! is up; the match is checked again before acting on it (see `run.rs`).

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action},
    config::TextCursor,
    desktop,
    geometry::{Point, Rect},
    keys::KeyCode,
    overlay::{Event, Frame, Highlight, Overlay, Tag},
    pick::{Pick, Source},
    selection::Selector,
    snapshot::{Snapshot, Target},
    style::Input,
    text::{Check, Place, Search},
};

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
    input: &Input,
    cursor: TextCursor,
) -> Result<Option<Pick>> {
    if let Err(e) = overlay.start_typing() {
        desktop::warn(&format!("wayhint: no text mode: {e:#}"));
        return Ok(None);
    }
    let result = keys(overlay, shown, selector, source, input, cursor);
    overlay.stop_typing();
    result
}

/// Text mode's keys, with the overlay typing.
fn keys(
    overlay: &mut Overlay,
    shown: &Snapshot,
    selector: &Selector,
    source: &mut dyn Source,
    input: &Input,
    cursor: TextCursor,
) -> Result<Option<Pick>> {
    let screen = overlay.monitor().rect;
    let verb = selector.armed().map(Action::name);
    // The tags go before the text is read: none is left to be typed.
    overlay.update(&frame(None, false, verb, screen, input))?;
    let mut search = Search::new(source.read_text(shown));
    overlay.update(&frame(Some(&search), false, verb, screen, input))?;
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
        overlay.update(&frame(Some(&search), stale, verb, screen, input))?;
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
/// and the pill. `search` is `None` while the text is being read, and
/// `stale` while the query's matches are being found: the pill shows `...`
/// for the count then, and nothing is marked. The pill names the armed
/// `verb`, dimmed, before the query; with no query yet, it shows
/// `[text.input] placeholder`, dimmed too.
fn frame(
    search: Option<&Search>,
    stale: bool,
    verb: Option<&str>,
    screen: Rect,
    input: &Input,
) -> Frame {
    let prefix = verb.map(|v| format!("{v} ")).unwrap_or_default();
    let (status, placeholder) = match search {
        None => (format!("{} ...", input.placeholder), true),
        Some(search) if search.query().is_empty() => (input.placeholder.clone(), true),
        Some(search) if stale => (
            format!("{}{}{}  ...", input.prefix, search.query(), input.suffix),
            false,
        ),
        Some(search) => {
            let (at, count) = search.position();
            let status = format!(
                "{}{}{}  {at}/{count}",
                input.prefix,
                search.query(),
                input.suffix
            );
            (status, false)
        }
    };
    let dimmed = if placeholder { status.as_str() } else { "" };
    let typed = prefix.chars().count() + dimmed.chars().count();
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
        tags: Vec::new(),
        dots: Vec::new(),
        highlights,
        input: Some(Tag {
            at: input
                .style
                .position
                .on(screen)
                .offset(Point::new(input.style.offset[0], input.style.offset[1])),
            position: input.style.position,
            text: format!("{prefix}{status}"),
            typed,
            target: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{style::Style, text::Text};

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

    const SCREEN: Rect = Rect::new(0, 0, 1920, 1080);

    fn frame(search: Option<&Search>, stale: bool, verb: Option<&str>, screen: Rect) -> Frame {
        super::frame(search, stale, verb, screen, &Input::new(&Style::default()))
    }

    #[test]
    fn the_tags_go_and_the_pill_says_what_is_typed() {
        let loading = frame(None, false, None, SCREEN);
        assert!(loading.tags.is_empty(), "target tags are hidden");
        assert_eq!(loading.input.unwrap().text, "text ...");
        let mut found = search(&["contact", "falcon"]);
        let pill = |found: &Search, stale| {
            frame(Some(found), stale, Some("double"), SCREEN)
                .input
                .unwrap()
        };
        assert_eq!(pill(&found, false).text, "double text");
        assert_eq!(
            pill(&found, false).typed,
            "double text".len(),
            "the placeholder is dimmed"
        );
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
    fn query_decorations_leave_matching_and_status_intact() {
        let mut found = search(&["άλλο", "Άλλο"]);
        typed(&mut found, "αλλ");
        let input = Input {
            prefix: "find [".into(),
            suffix: "]".into(),
            ..Input::new(&Style::default())
        };
        let display = |stale| {
            super::frame(Some(&found), stale, Some("double"), SCREEN, &input)
                .input
                .unwrap()
        };
        assert_eq!(display(false).text, "double find [αλλ]  1/2");
        assert_eq!(display(true).text, "double find [αλλ]  ...");
        assert_eq!(display(false).typed, 7, "only the armed verb is dimmed");
        assert_eq!(found.query(), "αλλ", "decorations are display only");
        let empty = Input {
            placeholder: "find…".into(),
            ..input
        };
        let waiting = super::frame(Some(&search(&["x"])), false, None, SCREEN, &empty);
        assert_eq!(waiting.input.unwrap().text, "find…");
        let reading = super::frame(None, false, None, SCREEN, &empty);
        assert_eq!(reading.input.unwrap().text, "find… ...");
    }

    #[test]
    fn query_positions_use_the_monitor_and_signed_offsets() {
        use crate::style::Position;
        let screen = Rect::new(-1920, 100, 1920, 1080);
        for (position, at) in [
            (Position::TopLeft, Point::new(-1920, 100)),
            (Position::Top, Point::new(-960, 100)),
            (Position::TopRight, Point::new(0, 100)),
            (Position::Left, Point::new(-1920, 640)),
            (Position::Center, Point::new(-960, 640)),
            (Position::Right, Point::new(0, 640)),
            (Position::BottomLeft, Point::new(-1920, 1180)),
            (Position::Bottom, Point::new(-960, 1180)),
            (Position::BottomRight, Point::new(0, 1180)),
        ] {
            let mut input = Input::new(&Style::default());
            input.style.position = position;
            input.style.offset = [-17, 23];
            let tag = super::frame(None, false, None, screen, &input)
                .input
                .unwrap();
            assert_eq!(tag.at, at.offset(Point::new(-17, 23)));
            assert_eq!(tag.position, position);
        }
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
        while found.pop() {}
        found.update(&|_, _| true);
        let cleared = frame(Some(&found), false, None, SCREEN);
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
