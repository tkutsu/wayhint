//! Text mode: the tags go, typing finds text on screen, and Enter acts on
//! the current match. The glue between `overlay` (keys and pixels), `text`
//! (what the keys find) and the apps ([`Source`]), as `pick` is for tags.
//!
//! ```text
//! text key -> tags hidden -> text read once -> keys: query edits, arrows
//!                                                |
//!                 Backspace with nothing typed: back to the tags
//!                 close key: leave wayhint
//!                 Enter: the armed verb (or the default) on the current match
//!                 one match left (with [text] pick_unique): as Enter
//!                 a whole tag typed (with [text] hints): its target, as a tag
//!                 text that matches nothing: kept to edit, and named
//! ```
//!
//! With `[text] hints`, the targets typing can't find keep a tag: those
//! whose text wasn't read (an icon, a window read from pixels) or has no
//! word. Their labels are ones no word on screen starts with, so a whole
//! label is never a query, and what's typed is both until one of them
//! wins ([`Hints`]).
//!
//! The text is read once, as text mode starts. Where a match is, the app is
//! asked only for matches it wasn't asked about before, and only once the
//! keys typed so far are all in. Nothing is scanned again while text mode
//! is up; the match is checked again before acting on it (see `run.rs`).

use std::{collections::HashSet, time::Instant};

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action, Mods},
    config::{Config, TextCursor},
    desktop,
    geometry::Point,
    keys::KeyCode,
    overlay::{Event, Frame, Highlight, Note, Overlay, Tag},
    pick::{self, Pick, Source},
    selection::{Selector, free_labels},
    snapshot::{Snapshot, Target},
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

/// Text mode's tags: each tagged target, by its index in the snapshot, and
/// its label as the letters that type it, lower case.
struct Hints(Vec<(usize, String)>);

impl Hints {
    /// Tags for `shown`'s targets that no text of `search` is on, in
    /// labels no word of it starts with. None without `[text] hints`.
    fn new(search: &Search, shown: &Snapshot, config: &Config) -> Self {
        if !config.text_hints {
            return Self(Vec::new());
        }
        // A text with no word (an icon's glyph) can't be typed.
        let found: HashSet<_> = search
            .texts()
            .iter()
            .filter(|text| text.content.chars().any(char::is_alphanumeric))
            .filter_map(|text| match text.place {
                Place::Object(index) => shown.texts[index].owner.as_ref(),
                Place::Cells(_) | Place::Glyphs(_) => None,
            })
            .collect();
        let untyped: Vec<usize> = (0..shown.targets.len())
            .filter(|&i| {
                shown.targets[i]
                    .object
                    .as_ref()
                    .is_none_or(|o| !found.contains(o))
            })
            .collect();
        let letters = config.alphabet.letters();
        let spell = |label: &[usize]| -> String {
            label
                .iter()
                .flat_map(|&i| letters[i].label.to_lowercase())
                .collect()
        };
        let free = |label: &[usize]| !search.starts_a_word(&spell(label));
        let labels = free_labels(untyped.len(), letters.len(), &free);
        Self(
            untyped
                .into_iter()
                .zip(labels.iter().map(|l| spell(l)))
                .collect(),
        )
    }

    /// The target whose whole label is `typed`.
    fn picked(&self, typed: &str) -> Option<usize> {
        let typed = typed.to_lowercase();
        self.0
            .iter()
            .find(|(_, label)| *label == typed)
            .map(|t| t.0)
    }

    /// Whether a label goes on from `typed`.
    fn open(&self, typed: &str) -> bool {
        let typed = typed.to_lowercase();
        self.0.iter().any(|(_, label)| label.starts_with(&typed))
    }

    /// The tags still open after `typed`, drawn as `pick` draws its own:
    /// with `[input] hints`, only the letters left to type; without, the
    /// whole label with what's typed dimmed.
    fn tags(&self, typed: &str, shown: &Snapshot, config: &Config) -> Vec<Tag> {
        let typed = typed.to_lowercase();
        let done = typed.chars().count();
        let (skip, dimmed) = if config.input.hints {
            (done, 0)
        } else {
            (0, done)
        };
        self.0
            .iter()
            .filter(|(_, label)| label.starts_with(&typed))
            .map(|(index, label)| {
                let text = label.chars().skip(skip).map(|c| config.style.case.of(c));
                pick::tag(&shown.targets[*index], text.collect(), dimmed, config)
            })
            .collect()
    }
}

/// Text mode, from the text key until a match is chosen or the close key leaves
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
#[allow(clippy::too_many_lines)] // one loop over the keys: its arms read best together
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
    let hints = Hints::new(&search, shown, config);
    let draw = |search: &Search, stale, missed: Option<&str>| Frame {
        tags: hints.tags(search.query(), shown, config),
        ..frame(Some(search), stale, verb, input, missed)
    };
    overlay.update(&draw(&search, false, None))?;
    // The query changed since the matches were found.
    let mut stale = false;
    // What was typed that matched nothing, and until when it's named.
    let mut missed: Option<(String, Instant)> = None;
    loop {
        let Some(event) = overlay.next_event(missed.as_ref().map(|m| m.1))? else {
            missed = None;
            overlay.update(&draw(&search, stale, None))?;
            continue;
        };
        let (key, mods, typed) = match event {
            Event::Escape => (
                crate::keys::Key {
                    code: KeyCode::ESC,
                    shift: false,
                },
                Mods::NONE,
                None,
            ),
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
        if config.verbs.is_close(key) {
            return Ok(Some(Pick::Leave));
        }
        let edit = match key.code {
            // Backspace takes back the last character; with nothing
            // typed, it goes back to the tags.
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
        if edit
            && key.code != KeyCode::BACKSPACE
            && let Some(index) = hints.picked(search.query())
        {
            let act = selector.act(mods);
            return Ok(Some(Pick::Target { index, act }));
        }
        // Every key typed so far first: fast typing asks the apps once.
        // A key that moves through the matches needs them found.
        if stale && (!edit || !overlay.has_events()?) {
            find(&mut search, shown, source);
            stale = false;
            // A character that leaves nothing to match stays typed, to
            // edit or take back; the info line names the query.
            if config.info.no_match
                && key.code != KeyCode::BACKSPACE
                && !search.query().is_empty()
                && search.matches().is_empty()
                && !hints.open(search.query())
            {
                let until = Instant::now() + config.info.duration;
                missed = Some((format!("no text has \"{}\"", search.query()), until));
            }
            // Typed into one match, not backed into it. An Enter that
            // comes after the typing acts below, with its own Ctrl, Shift
            // and Alt.
            if config.pick_unique
                && edit
                && key.code != KeyCode::BACKSPACE
                && unique(&search)
                && !hints.open(search.query())
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
        overlay.update(&draw(&search, stale, said))?;
    }
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
    // The character before the match too, where it was read: one typed
    // there since would leave the match inside a word.
    let (start, end) = (found.span.start, found.span.end);
    let from = if start > text.start { start - 1 } else { start };
    let check = Check {
        place: text.place.clone(),
        start: from,
        end,
        text: text.chars(from, end),
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
    verb: Option<&str>,
    input: &Input,
    missed: Option<&str>,
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
        highlights,
        input: Some(Note {
            text: format!("{prefix}{status}"),
            typed,
        }),
        info: missed.map(|m| (Notice::NoMatch, m.to_owned())),
        ..Frame::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{geometry::Rect, style::Style, text::Text};

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

    fn frame(search: Option<&Search>, stale: bool, verb: Option<&str>) -> Frame {
        super::frame(search, stale, verb, &Input::new(&Style::default()), None)
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
        assert_eq!(loading.input.unwrap().text, "text ...");
        let mut found = search(&["contact", "control"]);
        let pill = |found: &Search, stale| frame(Some(found), stale, Some("double")).input.unwrap();
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
            super::frame(Some(&found), stale, Some("double"), &input, None)
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
        let waiting = super::frame(Some(&search(&["x"])), false, None, &empty, None);
        assert_eq!(waiting.input.unwrap().text, "find…");
        let reading = super::frame(None, false, None, &empty, None);
        assert_eq!(reading.input.unwrap().text, "find… ...");
    }

    #[test]
    fn every_match_is_highlighted_and_the_current_one_marked() {
        let mut found = search(&["contact", "control"]);
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
            // The space before it too: a letter there would put it inside a word.
            assert_eq!((check.start, check.end, check.text.as_str()), (4, 7, " me"));
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
            owner: None,
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

    /// A Save button whose label was read, an icon button and a pixel tag,
    /// and the text read: `label`, the Save button's.
    fn buttons(label: &str) -> (Snapshot, Search) {
        use crate::{desktop::tree::ObjectId, snapshot::TextSource};

        let object = |path: &str| ObjectId {
            bus: ":1.1".into(),
            path: path.into(),
        };
        let target = |object, x| Target {
            object,
            window: 0,
            rect: Rect::new(x, 0, 20, 20),
            visible: Rect::new(x, 0, 20, 20),
            hit: Point::new(x + 10, 10),
            role: "push button".into(),
            name: String::new(),
            editable: false,
            clips: Vec::new(),
            opens: Vec::new(),
        };
        let mut shown = Snapshot::build(1, 0, std::iter::empty());
        shown.targets = vec![
            target(Some(object("/save")), 0),
            target(Some(object("/menu")), 30),
            target(None, 60),
        ];
        shown.texts.push(TextSource {
            object: object("/save/label"),
            window: 0,
            role: "label".into(),
            editable: false,
            named: true,
            area: Rect::new(0, 0, 1000, 1000),
            clips: Vec::new(),
            owner: Some(object("/save")),
            button: None,
        });
        let found = Search::new(vec![Text {
            place: Place::Object(0),
            window: 0,
            name: label.into(),
            rect: Rect::new(0, 0, 20, 20),
            area: Rect::new(0, 0, 1000, 1000),
            start: 0,
            content: label.into(),
        }]);
        (shown, found)
    }

    fn config(text: &str) -> Config {
        let layout = crate::keys::Layout::named("us", "");
        Config::parse(&format!("[keys]\nalphabet = \"asdf\"\n{text}"), &layout).unwrap()
    }

    #[test]
    fn hints_go_on_targets_with_no_text_in_letters_no_word_starts_with() {
        let (shown, found) = buttons("Save add");
        let hints = Hints::new(&found, &shown, &config(""));
        // s and a start Save and add: the icon and the pixel tag get d and f.
        assert_eq!(hints.0, [(1, "d".to_owned()), (2, "f".to_owned())]);
        assert_eq!(hints.picked("D"), Some(1), "Shift types the same tag");
        assert_eq!(hints.picked("s"), None);
        assert!(hints.open("") && hints.open("f") && !hints.open("s"));
        let tags = hints.tags("", &shown, &config(""));
        let texts: Vec<&str> = tags.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["D", "F"]);
        assert_eq!(tags[0].target, Some(shown.targets[1].hit));
        let off = Hints::new(&found, &shown, &config("[text]\nhints = false"));
        assert_eq!(off.0, Vec::<(usize, String)>::new());
    }

    #[test]
    fn a_text_with_no_word_leaves_its_target_a_hint() {
        let (shown, found) = buttons("×");
        let hints = Hints::new(&found, &shown, &config(""));
        let tagged: Vec<usize> = hints.0.iter().map(|h| h.0).collect();
        assert_eq!(tagged, [0, 1, 2]);
    }

    #[test]
    fn unmatched_text_stays_typed_and_can_be_edited_back_to_a_tag() {
        let hints = Hints(vec![(0, "xa".into())]);
        let mut found = search(&["contact"]);
        typed(&mut found, "x");
        assert!(hints.open(found.query()));
        typed(&mut found, "q");
        assert!(found.matches().is_empty());
        assert!(!hints.open(found.query()));
        assert_eq!(found.query(), "xq", "unmatched text stays typed");
        assert!(found.pop());
        assert!(
            hints.open(found.query()),
            "Backspace restores the tag prefix"
        );
    }
}
