//! Text mode, what `wayhint` runs (`--hints` runs tags alone): typing
//! finds text on screen, and Enter acts on the current match. The glue
//! between `overlay` (keys and pixels), `text` (what the keys find) and the
//! apps ([`Source`]), as `pick` is for tags.
//!
//! ```text
//! wayhint -> text read -> keys: query edits, Tab, arrows
//!                                |
//!                 a new current match: the pointer goes to it
//!                 an arrow: moves the pointer from there
//!                 nothing typed or armed: Enter acts at the pointer, the
//!                   help key lists the keys, and the windows are scanned
//!                   again every [scan] refresh_ms
//!                 Backspace with nothing typed: takes back a verb
//!                 close key: leave wayhint, or let go of a hold
//!                 a verb key before any text (a capital or a symbol), or
//!                   a named one any time: armed
//!                 Enter: the armed verb (or the default) on the current match,
//!                        where the pointer is if the arrows moved it
//!                 one match left (with [words] auto_accept): as Enter
//!                 a whole tag typed (with [words] fallback_hints): its target, as a tag
//!                 text that matches nothing: kept to edit, and named
//! ```
//!
//! With `[words] fallback_hints`, the targets typing can't find keep a tag: those
//! whose text wasn't read (an icon, a window read from pixels) or has no
//! word. Their labels are ones no word on screen starts with, so a whole
//! label is never a query, and what's typed is both until one of them
//! wins ([`Hints`]).
//!
//! The text is read as text mode starts, and again whenever a refresh
//! finds the windows changed while nothing is typed: the first key freezes
//! what was read. Where a match is, the app is asked only for matches it
//! wasn't asked about before, and only once the keys typed so far are all
//! in. The match is checked again before acting on it (see `run.rs`).

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action, Mods},
    config::{Config, TextCursor},
    desktop,
    geometry::{Point, Rect},
    keys::KeyCode,
    overlay::{Event, Frame, Highlight, Overlay, Tag},
    pick::{self, Mouse, Pick, Source},
    selection::{self, Selector, Step, relabel_free},
    snapshot::{Snapshot, Target},
    status::{Message, Status},
    style::{Input, Notice, StartMark},
    text::{Check, Match, Place, Search, Span},
};

/// How many characters `[words] auto_accept` waits for before acting on
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
    /// The arrows moved the pointer off the match: the click goes where
    /// the pointer is, the match still re-checked.
    pub aimed: bool,
    /// What the match must still read. `None` for a text that is a name:
    /// the re-check reads the name anyway.
    pub check: Option<Check>,
}

/// Text mode's tags: each tagged target, by its index in the snapshot, and
/// its label, as indices into the alphabet. They're typed as the tags are,
/// by the keys' positions, aliases too; what those keys type is the query.
struct Hints(Vec<(usize, Vec<usize>)>);

/// A label as drawn, lower case.
fn spell(label: &[usize], config: &Config) -> String {
    let letters = config.alphabet.letters();
    label
        .iter()
        .flat_map(|&i| letters[i].label.to_lowercase())
        .collect()
}

/// Whether `label` goes on from `pressed`: the tag letter each key typed
/// so far is, if any.
fn opens(label: &[usize], pressed: &[Option<usize>]) -> bool {
    pressed.len() <= label.len() && label.iter().zip(pressed).all(|(&l, &p)| p == Some(l))
}

impl Hints {
    /// Tags for `shown`'s targets that no text of `search` is on, in
    /// labels no word of it starts with. None without `[words] fallback_hints`.
    fn new(search: &Search, shown: &Snapshot, config: &Config) -> Self {
        Self::keeping(search, shown, config, &|_| None)
    }

    /// The same, where a target `kept` names a label for keeps it if it
    /// can (see `selection::relabel_free`): a refresh moves no tag.
    fn keeping(
        search: &Search,
        shown: &Snapshot,
        config: &Config,
        kept: &dyn Fn(usize) -> Option<Vec<usize>>,
    ) -> Self {
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
                Place::Cells(_) => None,
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
        let free = |label: &[usize]| !search.starts_a_word(&spell(label, config));
        let before: Vec<Option<Vec<usize>>> = untyped.iter().map(|&i| kept(i)).collect();
        let before: Vec<Option<&[usize]>> = before.iter().map(Option::as_deref).collect();
        let labels = relabel_free(&before, config.alphabet.len(), &free);
        Self(untyped.into_iter().zip(labels).collect())
    }

    /// The target whose whole label `pressed` is.
    fn picked(&self, pressed: &[Option<usize>]) -> Option<usize> {
        self.0
            .iter()
            .find(|(_, label)| label.len() == pressed.len() && opens(label, pressed))
            .map(|t| t.0)
    }

    /// The target whose whole label `pressed` is, typed into `search`. A
    /// label starts no word, but what an alias or another layout types
    /// for it may: then it's text.
    fn typed(&self, pressed: &[Option<usize>], search: &Search) -> Option<usize> {
        self.picked(pressed)
            .filter(|_| !search.starts_a_word(search.query()))
    }

    /// A completed hint acts on Enter, or while typing with autoaccept.
    fn choice(
        &self,
        pressed: &[Option<usize>],
        search: &Search,
        config: &Config,
        enter: bool,
    ) -> Option<usize> {
        self.typed(pressed, search)
            .filter(|_| enter || config.hint_auto_accept)
    }

    /// Whether a label goes on from `pressed`.
    fn open(&self, pressed: &[Option<usize>]) -> bool {
        self.0.iter().any(|(_, label)| opens(label, pressed))
    }

    /// The tags still open after `pressed`, drawn as `pick` draws its own:
    /// with `[input] show_typed_hints`, only the letters left to type; without, the
    /// whole label with what's typed dimmed.
    fn tags(&self, pressed: &[Option<usize>], shown: &Snapshot, config: &Config) -> Vec<Tag> {
        let done = pressed.len();
        let (skip, dimmed) = if config.input.hints {
            (done, 0)
        } else {
            (0, done)
        };
        self.0
            .iter()
            .filter(|(_, label)| opens(label, pressed))
            .map(|(index, label)| {
                let (skip, dimmed) = if done == label.len() {
                    (0, done)
                } else {
                    (skip, dimmed)
                };
                let text = spell(label, config);
                let text = text.chars().skip(skip).map(|c| config.style.case.of(c));
                pick::tag(&shown.targets[*index], text.collect(), dimmed, config)
            })
            .collect()
    }
}

/// Text mode as it stands: what was read, typed and armed, and what the
/// info line says.
#[allow(clippy::struct_excessive_bools)] // independent states of one text mode
struct Typing {
    search: Search,
    hints: Hints,
    /// The verb typed so far, on a copy: a block opened here starts its
    /// lines' text mode from the pick's selector again.
    command: Selector,
    /// For each character of the query, the tag letter its key is, if any.
    pressed: Vec<Option<usize>>,
    /// The query changed since the matches were found.
    stale: bool,
    /// What was typed that matched nothing, and until when it's named.
    missed: Option<(String, Instant)>,
    /// The help key has every key listed under the input box.
    help: bool,
    /// A hold keeps a button down: the input box says `drop`.
    holding: bool,
    /// The match the pointer last went to (its text and where it starts)
    /// and where, and whether the arrows moved it since.
    jumped: Option<((usize, u32), Point)>,
    aimed: bool,
}

impl Typing {
    /// Whether nothing is typed or armed: Enter acts at the pointer, the
    /// help key lists the keys, and the windows are scanned again.
    fn idle(&self) -> bool {
        self.search.query().is_empty() && self.command.armed().is_none()
    }

    /// Another match is current: the pointer goes to it, and the arrows
    /// start over from there. The same one, typed longer or shorter,
    /// takes the pointer along only if the arrows haven't moved it.
    fn follow(&mut self, mouse: &mut dyn Mouse, cursor: TextCursor) -> Result<()> {
        if self.stale {
            return Ok(());
        }
        let Some(found) = self.search.current() else {
            return Ok(());
        };
        let Some((_, at)) = spot(found, cursor) else {
            return Ok(());
        };
        let id = (found.span.text, found.span.start);
        let aimed = self.aimed;
        let keep = self
            .jumped
            .is_some_and(|(was, point)| was == id && (aimed || point == at));
        if !keep {
            mouse.move_to(at)?;
            (self.jumped, self.aimed) = (Some((id, at)), false);
        }
        Ok(())
    }

    /// What text mode shows: [`frame`], with the tags on the targets with
    /// no text.
    fn draw(&self, shown: &Snapshot, config: &Config) -> Frame {
        let info = if self.help {
            Some(Message::new(Notice::Help, config.verbs.text_legend()))
        } else {
            let missed = self.missed.as_ref();
            missed.map(|m| Message::new(Notice::NoMatch, m.0.clone()))
        };
        let tags = self
            .hints
            .tags(&self.pressed, shown, config)
            .into_iter()
            .filter(|tag| {
                tag.kind != crate::overlay::Kind::Window
                    || self.command.armed().is_none()
                        && self.command.act(Mods::NONE).action == Action::Left
                        && !self.holding
            })
            .collect();
        let verb = self.command.armed().map(Action::name);
        let (search, input) = (Some(&self.search), &config.input);
        Frame {
            tags,
            ..frame(search, self.stale, verb, self.holding, input, info)
        }
    }

    /// Scans the windows again, and if they changed, reads their text
    /// again and draws it, a target found again keeping its tag. Whether
    /// they changed; `None` from a source that never does.
    fn refresh(
        &mut self,
        overlay: &mut Overlay,
        shown: &mut Snapshot,
        source: &mut dyn Source,
        config: &Config,
    ) -> Result<Option<bool>> {
        // What shows while a slow scan goes on: `scanning` under the box.
        let busy = config.info.scanning.then(|| {
            let mut busy = self.draw(shown, config);
            busy.status.info = Some(pick::scanning());
            busy
        });
        let mut slowed = false;
        let fresh = source.rescan(&mut || {
            if let Some(busy) = &busy {
                slowed = overlay.update(busy).is_ok();
            }
        })?;
        let Some(mut fresh) = fresh else {
            return Ok(None);
        };
        fresh.merge_nearby(config.merge_distance);
        let changed = fresh.targets != shown.targets
            || fresh.obstacles != shown.obstacles
            || fresh.texts != shown.texts;
        if !changed {
            if slowed {
                overlay.update(&self.draw(shown, config))?;
            }
            return Ok(Some(false));
        }
        let survivors = fresh.survivors(shown);
        let before: HashMap<usize, Vec<usize>> = self.hints.0.iter().cloned().collect();
        *shown = fresh;
        overlay.set_layout(shown);
        self.search = read(shown, source, config);
        let kept = |i: usize| survivors[i].and_then(|was| before.get(&was).cloned());
        let hints = Hints::keeping(&self.search, shown, config, &kept);
        // A label now on another target: a key typed for the old one
        // during the scan must not land there.
        let moved = hints.0.iter().any(|(now, label)| {
            before
                .iter()
                .any(|(was, old)| old == label && survivors[*now] != Some(*was))
        });
        self.hints = hints;
        let frame = self.draw(shown, config);
        if moved {
            overlay.show(&frame)?;
        } else {
            overlay.update(&frame)?;
        }
        Ok(Some(true))
    }
}

/// Text mode, until a match or a tag is chosen or the close key leaves.
/// `selector` holds the verb from `--verb`, if any. A verb typed here
/// comes first, before the text: a capital or a symbol, or a named key at
/// any point. Lower-case letters and digits always go into the query.
/// While a hold keeps a button down (`holding`), the close key lets go
/// where it is. A refresh may change `shown`, which ends as the snapshot
/// the pick was made in.
///
/// The pointer goes to the current match each time another one becomes
/// current, a typed character, a label or Tab away, and the arrows move it
/// from there, held for faster. Enter then clicks where they took it; a
/// new current match takes the pointer again and starts their count over.
pub fn search(
    overlay: &mut Overlay,
    shown: &mut Snapshot,
    selector: &Selector,
    holding: bool,
    mouse: &mut dyn Mouse,
    source: &mut dyn Source,
    config: &Config,
) -> Result<Pick> {
    if let Err(e) = overlay.start_typing() {
        desktop::warn(&format!("wayhint: no text mode: {e:#}"));
        return Ok(Pick::Leave);
    }
    let result = keys(overlay, shown, selector, holding, mouse, source, config);
    overlay.stop_typing();
    result
}

/// Text mode's keys, with the overlay typing.
#[allow(clippy::too_many_lines)] // one loop over the keys: its arms read best together
fn keys(
    overlay: &mut Overlay,
    shown: &mut Snapshot,
    selector: &Selector,
    holding: bool,
    mouse: &mut dyn Mouse,
    source: &mut dyn Source,
    config: &Config,
) -> Result<Pick> {
    let cursor = config.text_cursor;
    // Nothing shows while the text is read: no tag to type yet.
    let verb = selector.armed().map(Action::name);
    overlay.update(&frame(None, false, verb, holding, &config.input, None))?;
    let search = read(shown, source, config);
    let mut now = Typing {
        hints: Hints::new(&search, shown, config),
        search,
        command: selector.clone(),
        pressed: Vec::new(),
        stale: false,
        missed: None,
        help: false,
        holding,
        jumped: None,
        aimed: false,
    };
    overlay.update(&now.draw(shown, config))?;
    // When the windows are next scanned again, as the tags' are, and
    // further apart while nothing changes (see `pick::slower`).
    let mut every = config.refresh;
    let next = |every: Option<Duration>| every.map(|every| Instant::now() + every);
    let mut due = next(every);
    loop {
        // Only while nothing is typed: a key freezes what was read.
        let refresh = due.filter(|_| now.idle());
        let wake = refresh
            .into_iter()
            .chain(now.missed.as_ref().map(|m| m.1))
            .min();
        let Some(event) = overlay.next_event(wake)? else {
            let at = Instant::now();
            if now.missed.as_ref().is_some_and(|m| m.1 <= at) {
                now.missed = None;
                overlay.update(&now.draw(shown, config))?;
            }
            if refresh.is_some_and(|due| due <= at) {
                // A scan holds up the keys, so none while an arrow is
                // held: the pointer would freeze mid-move.
                if !overlay.holding() {
                    every = match now.refresh(overlay, shown, source, config)? {
                        None => None,
                        Some(false) => every.map(pick::slower),
                        Some(true) => config.refresh,
                    };
                }
                due = next(every);
            }
            continue;
        };
        let (key, mods, typed, ticks) = match event {
            Event::Escape => (
                crate::keys::Key {
                    code: KeyCode::ESC,
                    shift: false,
                },
                Mods::NONE,
                None,
                0,
            ),
            Event::Typed(key, mods, typed) => (key, mods, typed, 0),
            // Enter, on its release.
            Event::Key(key, mods) => (key, mods, None, 0),
            Event::Held(key, mods, ticks) => (key, mods, None, ticks),
            Event::Interrupted | Event::Closed | Event::KeyboardLost => {
                return Ok(Pick::Leave);
            }
            Event::Monitors => bail!("the monitors changed; run wayhint again"),
            Event::Workspace => return Ok(Pick::Rescan),
        };
        if config.verbs.is_close(key) {
            return Ok(if holding { Pick::Release } else { Pick::Leave });
        }
        if let Some(by) = pick::arrow(key.code, ticks, config) {
            mouse.move_by(by)?;
            now.aimed = true;
            continue;
        }
        let letter = config.alphabet.index_of(key.code);
        let action = config.verbs.action_of(key);
        let help = config.verbs.is_help(key);
        // A key that types nothing and means nothing here (a modifier on
        // its own) leaves the info line as it is.
        let built_in = matches!(key.code, KeyCode::BACKSPACE | KeyCode::ENTER | KeyCode::TAB);
        if typed.is_none() && letter.is_none() && action.is_none() && !help && !built_in {
            continue;
        }
        // The help key, with nothing typed, lists every key until the next.
        if help && now.idle() {
            now.help = !now.help;
            overlay.update(&now.draw(shown, config))?;
            continue;
        }
        now.help = false;
        // A verb: a named key (PageDown) any time; a character only before
        // any text, and only one that's no lower-case letter or digit (`,`,
        // `A`), which costs just the searches that start with it. Typed
        // twice, it's done again where the last command was.
        if let Some(action) = action
            && (crate::keys::name_of(key.code).is_some()
                || now.search.query().is_empty()
                    && typed.is_some_and(|c| c.is_uppercase() || !c.is_alphanumeric()))
        {
            if action.scrolls() {
                return Ok(Pick::Scroll {
                    act: Act { action, mods },
                });
            }
            if let Step::Again(act) = now.command.press(selection::Input::Verb(action, mods)) {
                return Ok(Pick::Again { act });
            }
            overlay.update(&now.draw(shown, config))?;
            continue;
        }
        let edit = match key.code {
            // Backspace takes back the last character; with nothing
            // typed, the verb.
            KeyCode::BACKSPACE => {
                if now.search.pop() {
                    now.pressed.pop();
                    true
                } else {
                    now.command.press(selection::Input::Backspace);
                    false
                }
            }
            KeyCode::ENTER | KeyCode::TAB => false,
            // Ctrl and Alt type nothing, AltGr isn't Alt; but they end a
            // tag, as a Ctrl- or Alt-click, as with the tags.
            _ => match typed {
                Some(c) if !mods.ctrl && !mods.alt => {
                    now.search.push(c);
                    now.pressed.push(letter);
                    true
                }
                _ if mods.ctrl || mods.alt => {
                    let last = [now.pressed.as_slice(), &[letter]].concat();
                    let Some(index) = now.hints.picked(&last) else {
                        continue;
                    };
                    if !pick::available(&shown.targets[index], &now.command, holding) {
                        continue;
                    }
                    if config.hint_auto_accept {
                        return Ok(Pick::Target {
                            index,
                            act: now.command.act(mods),
                        });
                    }
                    // Ctrl/Alt may type no character. Keep the tag's
                    // letter so it can be edited or confirmed later.
                    let Some(letter) = letter else { continue };
                    now.search.push(config.alphabet.letters()[letter].label);
                    now.pressed.push(Some(letter));
                    true
                }
                _ => continue,
            },
        };
        now.stale |= edit;
        if edit
            && key.code != KeyCode::BACKSPACE
            && let Some(index) = now
                .hints
                .choice(&now.pressed, &now.search, config, false)
                .filter(|&i| pick::available(&shown.targets[i], &now.command, holding))
        {
            let act = now.command.act(mods);
            return Ok(Pick::Target { index, act });
        }
        // Every key typed so far first: fast typing asks the apps once.
        // A key that moves through the matches needs them found.
        if now.stale && (!edit || !overlay.has_events()?) {
            find(&mut now.search, shown, source);
            now.stale = false;
            // A character that leaves nothing to match stays typed, to
            // edit or take back; the info line names the query.
            if config.info.no_match
                && key.code != KeyCode::BACKSPACE
                && !now.search.query().is_empty()
                && now.search.matches().is_empty()
                && !now.hints.open(&now.pressed)
            {
                let until = Instant::now() + config.info.duration;
                let said = format!("no text has \"{}\"", now.search.query());
                now.missed = Some((said, until));
            }
            // Typed into one match, not backed into it. An Enter that
            // comes after the typing acts below, with its own Ctrl and
            // Alt.
            if config.text_auto_accept
                && edit
                && key.code != KeyCode::BACKSPACE
                && unique(&now.search)
                && !now.hints.open(&now.pressed)
                && let Some(chosen) = choose(
                    &now.search,
                    shown,
                    now.command.act(Mods::NONE),
                    cursor,
                    false,
                )
            {
                return Ok(Pick::Text(Box::new(chosen)));
            }
        }
        match key.code {
            KeyCode::ENTER => {
                if let Some(index) = now
                    .hints
                    .choice(&now.pressed, &now.search, config, true)
                    .filter(|&i| pick::available(&shown.targets[i], &now.command, holding))
                {
                    return Ok(Pick::Target {
                        index,
                        act: now.command.act(mods),
                    });
                }
                // Nothing typed: at the pointer, as `Space` is with the tags.
                if now.search.query().is_empty() {
                    let act = now.command.act(mods);
                    return Ok(Pick::Here { act });
                }
                let act = now.command.act(mods);
                if let Some(chosen) = choose(&now.search, shown, act, cursor, now.aimed) {
                    return Ok(Pick::Text(Box::new(chosen)));
                }
            }
            KeyCode::TAB if !key.shift => now.search.next(),
            KeyCode::TAB => now.search.previous(),
            _ => {}
        }
        now.follow(mouse, cursor)?;
        overlay.update(&now.draw(shown, config))?;
    }
}

/// Whether the query is long enough, `UNIQUE` characters, and has just one
/// match.
fn unique(search: &Search) -> bool {
    search.query().chars().count() >= UNIQUE && search.matches().len() == 1
}

/// The windows' texts, and with `[words.first_character]`, where each word's first
/// letter shows, asked of the apps now: the first letter typed then asks
/// nothing. A text read from a name has no letters to ask about.
fn read(shown: &Snapshot, source: &mut dyn Source, config: &Config) -> Search {
    let mut search = Search::new(source.read_text(shown));
    if config.marks.starts.mark == StartMark::Off {
        return search;
    }
    let spans: Vec<Span> = search
        .word_starts()
        .into_iter()
        .filter(|span| match search.texts()[span.text].place {
            Place::Object(index) => !shown.texts[index].named,
            Place::Cells(_) => true,
        })
        .collect();
    let boxes = source.locate(shown, search.texts(), &spans);
    search.located(&spans, boxes);
    search.mark_starts(&spans, &|window, at| source.shows(shown, window, at));
    search
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

/// Where a click on `found` goes, at `cursor` on its first line (its last
/// for `End`), and that line's box.
fn spot(found: &Match, cursor: TextCursor) -> Option<(Rect, Point)> {
    let part = match cursor {
        TextCursor::Start | TextCursor::Center => found.parts[0],
        TextCursor::End => *found.parts.last()?,
    };
    let x = match cursor {
        TextCursor::Start => part.x,
        TextCursor::Center => part.center().x,
        TextCursor::End => part.right() - 1,
    };
    Some((part, Point::new(x, part.center().y)))
}

/// The current match as a command: the text's object as the target, and
/// the text to read again. `aimed` when the arrows moved the pointer off it.
fn choose(
    search: &Search,
    shown: &Snapshot,
    act: Act,
    cursor: TextCursor,
    aimed: bool,
) -> Option<Chosen> {
    let found = search.current()?;
    let text = &search.texts()[found.span.text];
    let (part, hit) = spot(found, cursor)?;
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
        aimed,
        check,
    })
}

/// What text mode shows: every match's lines, the current one's marked,
/// and the input box; with no query, each word's first letter marked if
/// `[words.first_character]` says. `search` is `None` while the text is being read, and
/// `stale` while the query's matches are being found: the box shows `...`
/// for the count then, and nothing is marked. The box names the armed
/// `verb`, or `drop` while `holding`, dimmed, before the query; with no
/// query yet, it shows `[input] placeholder`, dimmed too. Under it, `info`
/// says what was typed that matched nothing, or lists the keys.
fn frame(
    search: Option<&Search>,
    stale: bool,
    verb: Option<&'static str>,
    holding: bool,
    input: &Input,
    info: Option<Message>,
) -> Frame {
    let status = Status {
        info,
        ..Status::text(
            verb,
            holding,
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
    // The words' first letters, until a query's matches take over.
    let starts = search
        .filter(|search| search.query().is_empty())
        .map(|search| search.starts().to_vec())
        .unwrap_or_default();
    Frame {
        highlights,
        starts,
        status,
        ..Frame::default()
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

    fn frame(search: Option<&Search>, stale: bool, verb: Option<&'static str>) -> Frame {
        super::frame(
            search,
            stale,
            verb,
            false,
            &Input::new(&Style::default()),
            None,
        )
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
        let mut found = search(&["contact", "control"]);
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
            super::frame(Some(&found), stale, Some("double"), false, &input, None)
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
        let waiting = super::frame(Some(&search(&["x"])), false, None, false, &empty, None);
        assert_eq!(waiting.status.line().unwrap().0, "find…");
        let reading = super::frame(None, false, None, false, &empty, None);
        assert_eq!(reading.status.line().unwrap().0, "find… ...");
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
        };
        for (cursor, x) in [
            (TextCursor::Start, 150),
            (TextCursor::Center, 160),
            (TextCursor::End, 169),
        ] {
            let chosen = choose(&found, &shown, act, cursor, false).expect("a match");
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
            menu: None,
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
                ctrl: true,
                ..crate::action::Mods::NONE
            },
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
            let chosen = choose(&found, &shown, act, cursor, true).expect("a wrapped match");
            assert_eq!(chosen.target.hit, hit);
            assert_eq!(chosen.target.visible, visible);
            assert!(visible.contains(hit));
            assert_eq!(chosen.target.object, Some(object.clone()));
            assert_eq!(chosen.target.clips, std::slice::from_ref(&object));
            assert!(chosen.target.editable);
            assert_eq!(chosen.act, act, "keep the verb and modifiers");
            assert!(chosen.aimed, "the arrows moved the pointer off it");
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
            menu: None,
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
    fn text_mode_window_hints_hide_when_any_verb_is_armed() {
        let (mut shown, found) = buttons("Save");
        shown.targets[2].role = "window".into();
        let config = config("");
        let mut now = Typing {
            hints: Hints::new(&found, &shown, &config),
            search: found,
            command: Selector::new(shown.targets.len(), config.alphabet.len(), Action::Left),
            pressed: Vec::new(),
            stale: false,
            missed: None,
            help: false,
            holding: false,
            jumped: None,
            aimed: false,
        };
        assert!(
            now.draw(&shown, &config)
                .tags
                .iter()
                .any(|tag| tag.kind == crate::overlay::Kind::Window)
        );
        for action in Action::ALL {
            now.command
                .press(selection::Input::Verb(action, Mods::NONE));
            assert!(
                !now.draw(&shown, &config)
                    .tags
                    .iter()
                    .any(|tag| tag.kind == crate::overlay::Kind::Window)
            );
        }
        now.command.press(selection::Input::Backspace);
        assert!(
            now.draw(&shown, &config)
                .tags
                .iter()
                .any(|tag| tag.kind == crate::overlay::Kind::Window)
        );
    }

    #[test]
    fn hints_go_on_targets_with_no_text_in_letters_no_word_starts_with() {
        let (shown, found) = buttons("Save add");
        let hints = Hints::new(&found, &shown, &config(""));
        // s and a start Save and add: the icon and the pixel tag get d and f.
        assert_eq!(hints.0, [(1, vec![2]), (2, vec![3])]);
        assert_eq!(hints.picked(&[Some(2)]), Some(1));
        assert_eq!(hints.picked(&[Some(1)]), None);
        assert!(hints.open(&[]) && hints.open(&[Some(3)]) && !hints.open(&[Some(1)]));
        assert!(!hints.open(&[None]), "a key that's no tag letter");
        let tags = hints.tags(&[], &shown, &config(""));
        let texts: Vec<&str> = tags.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["D", "F"]);
        assert_eq!(tags[0].target, Some(shown.targets[1].hit));
        let off = Hints::new(&found, &shown, &config("[words]\nfallback_hints = false"));
        assert_eq!(off.0, Vec::<(usize, Vec<usize>)>::new());
    }

    #[test]
    fn a_tag_is_typed_by_its_keys_aliases_too_unless_their_text_is_found() {
        let config = config("[keys.aliases]\n\";\" = \"d\"");
        let (shown, mut found) = buttons("Save add");
        let hints = Hints::new(&found, &shown, &config);
        let semicolon = crate::keys::KeyCode::from_evdev(39);
        let pressed = [config.alphabet.index_of(semicolon)];
        found.push(';');
        assert_eq!(hints.typed(&pressed, &found), Some(1), "; is d's alias");
        // Another layout: the d key types δ, which starts a word here.
        let (_, mut greek) = buttons("Save δέντρο");
        greek.push('δ');
        assert_eq!(hints.typed(&[Some(2)], &greek), None, "δ is text");
        let (_, mut latin) = buttons("Save add");
        latin.push('δ');
        assert_eq!(hints.typed(&[Some(2)], &latin), Some(1), "no word: the tag");
    }

    #[test]
    fn text_modes_hints_wait_for_enter_independently_of_text_autoaccept() {
        let (shown, mut found) = buttons("Save add");
        typed(&mut found, "d");
        let pressed = [Some(2)];
        for text in [false, true] {
            let config = config(&format!(
                "[hints]\nauto_accept = false\n[words]\nauto_accept = {text}"
            ));
            let hints = Hints::new(&found, &shown, &config);
            assert_eq!(hints.choice(&pressed, &found, &config, false), None);
            assert_eq!(hints.choice(&pressed, &found, &config, true), Some(1));
            assert_eq!(hints.choice(&[], &found, &config, true), None);
            let tags = hints.tags(&pressed, &shown, &config);
            assert_eq!((tags[0].text.as_str(), tags[0].typed), ("D", 1));
        }
        let config = config("");
        let hints = Hints::new(&found, &shown, &config);
        assert_eq!(hints.choice(&pressed, &found, &config, false), Some(1));
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
        // A tag `xa`, with x and a the alphabet's letters 0 and 1.
        let hints = Hints(vec![(0, vec![0, 1])]);
        let mut found = search(&["contact"]);
        let mut pressed = vec![Some(0)];
        typed(&mut found, "x");
        assert!(hints.open(&pressed));
        typed(&mut found, "q");
        pressed.push(None);
        assert_eq!(found.matches(), []);
        assert!(!hints.open(&pressed));
        assert_eq!(found.query(), "xq", "unmatched text stays typed");
        assert!(found.pop());
        pressed.pop();
        assert!(hints.open(&pressed), "Backspace restores the tag prefix");
    }

    #[test]
    fn a_refresh_leaves_each_tag_on_its_target() {
        let (shown, found) = buttons("Save add");
        let config = config("");
        // The icon had f and the pixel tag d: they keep them.
        let kept = |i: usize| [None, Some(vec![3]), Some(vec![2])][i].clone();
        let hints = Hints::keeping(&found, &shown, &config, &kept);
        assert_eq!(hints.0, [(1, vec![3]), (2, vec![2])]);
    }

    #[test]
    fn the_key_list_says_how_text_mode_works() {
        let legend = config("").verbs.text_legend();
        assert!(legend.starts_with("text Enter\tleft click\n"), "{legend}");
        assert!(legend.contains(", text Enter\tright click"), "{legend}");
        assert!(legend.ends_with("Esc\tleave"), "{legend}");
    }
}
