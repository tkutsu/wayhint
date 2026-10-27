//! Choosing one target: tags on screen, keys in, until a tag is typed or
//! the user leaves. The glue between `overlay` (pixels and keys) and
//! `selection` (what the keys mean), so neither has to know the other.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action, Mods},
    config::Config,
    geometry::{Point, Rect},
    keys::{Key, KeyCode},
    overlay::{Event, Frame, Kind, Overlay, Tag},
    search::{self, Chosen},
    selection::{Input, Selector, Step},
    snapshot::{Snapshot, Target},
    status::{Message, Status},
    style::{Notice, Position},
    text::{Span, Text},
};

/// How far inside its monitor's edge the arrows stop the pointer, in px.
const EDGE: i32 = 5;
/// A held arrow speeds up by one `[pointer] nudge` a tick every this many
/// ticks (25 ms each, `overlay/hold.rs`), up to `TOP_SPEED` a tick.
const SPEED_UP: u16 = 8;
const TOP_SPEED: u16 = 4;
/// The longest a screen that stays the same waits between refreshes: each
/// one that finds nothing changed doubles `[scan] refresh`, up to this.
const IDLE_REFRESH: Duration = Duration::from_secs(2);

/// How a pick ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Do `act` at the target at `index`.
    Target { index: usize, act: Act },
    /// Do `act` where the pointer is (`Space`).
    Here { act: Act },
    /// Do `act` where the last command was done (a verb typed twice).
    Again { act: Act },
    /// Do the last command again, on its target.
    Repeat,
    /// `Esc` while a hold keeps a button down: let go where it is.
    Release,
    /// A match chosen in text mode (boxed: it carries its target).
    Text(Box<Chosen>),
    /// `Esc`, Ctrl+C, or the overlay went away: nothing chosen.
    Leave,
    /// The monitor shows another workspace: nothing chosen, and the tags
    /// are for windows that left.
    Rescan,
}

/// What the input box's second line says, besides `no targets` while
/// nothing has a tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Said {
    #[default]
    Nothing,
    /// A letter that no tag goes on with, named until the time given.
    Missed(String, Instant),
    /// Every key and what it does, until the next key that means something.
    Help,
}

impl Said {
    /// When a passing message goes.
    const fn until(&self) -> Option<Instant> {
        match self {
            Self::Missed(_, until) => Some(*until),
            _ => None,
        }
    }
}

/// The message the second line shows while a scan is slow.
pub fn scanning() -> Message {
    Message::new(Notice::Scanning, "scanning")
}

/// A refresh can move tags without changing which targets their keys name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refresh {
    Unchanged,
    Redraw,
    Retag,
}

/// How a pick starts: with a verb armed (`--verb`), in text mode (unless
/// `--hints`), and whether a hold keeps a button down, so the next
/// command lets go of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Start {
    pub verb: Option<Action>,
    pub text: bool,
    pub holding: bool,
}

/// What the screen holds while a pick runs: the targets, and whether a
/// button is held.
struct Scene<'a> {
    targets: &'a [Target],
    holding: bool,
}

/// Where a pick's targets come from, asked again while the tags are up.
pub trait Source {
    /// The targets as they are now, or `None` when they can't change (a
    /// saved snapshot). A run scans its windows again, and calls `slow` if
    /// that takes long enough to say so.
    fn rescan(&mut self, slow: &mut dyn FnMut()) -> Result<Option<Snapshot>>;
    /// Drops the lines of an opened block that another window covers or
    /// the monitor cuts off.
    fn keep(&self, opened: &mut Snapshot);
    /// The text the windows of `shown` show, for text mode. None from a
    /// saved snapshot: there's no app to ask.
    fn read_text(&mut self, shown: &Snapshot) -> Vec<Text>;
    /// Where each of `spans` of `texts` is on screen: a global box per
    /// line, none if the app can't say.
    fn locate(&mut self, shown: &Snapshot, texts: &[Text], spans: &[Span]) -> Vec<Vec<Rect>>;
    /// Whether the point `at` of window `window` of `shown` is in view: on
    /// the monitor, with nothing over it.
    fn shows(&self, shown: &Snapshot, window: usize, at: Point) -> bool;
}

/// The pointer, as the arrows move it and text mode's matches take it. A
/// button a hold keeps down stays down through both: a drag.
pub trait Mouse {
    /// Moves it `by` px, stopping inside its monitor's edge (see `moved`).
    fn move_by(&mut self, by: Point) -> Result<()>;
    /// Moves it to `at`.
    fn move_to(&mut self, at: Point) -> Result<()>;
}

/// Shows tags for `shown`'s targets and reads keys until one is picked.
/// `start.verb` (from `--verb`) is armed as if its key were the first one
/// typed, and `start.text` (no `--hints`) makes it a text-mode pick
/// ([`text`]). The arrows move the pointer through `mouse`, held for
/// faster, and `Space` or a verb typed twice acts where it is.
///
/// Every `[scan] refresh` with no pick, `source` scans again and the tags
/// follow: a target found again keeps its tag, and what was typed for it
/// stays typed (see `Selector::relabel`). While nothing changes, the scans
/// come further apart (see `IDLE_REFRESH`).
///
/// A block's tag (a pixel target with lines in `opens`) opens it instead
/// of picking: `shown` becomes the block opened, its lines filtered by
/// `source` (none under another window or off the monitor), and the
/// command goes on there, with what was typed before the block's tag (a
/// verb, arrows). The lines stay as they were opened: a rescan
/// has no blocks to open. `shown` ends as the snapshot the pick was made
/// in.
///
/// While a hold keeps a button down (`start.holding`), whatever is picked
/// is where it's let go, and the close key lets go where it is.
///
/// The input box's second line says `no targets` while nothing has a tag,
/// names a letter no tag goes on with for `[info] duration`, and shows
/// every key while the help key has it up (see [`Said`]).
#[allow(clippy::too_many_lines)] // one loop over the keys: its arms read best together
pub fn pick(
    overlay: &mut Overlay,
    config: &Config,
    shown: &mut Snapshot,
    start: Start,
    mouse: &mut dyn Mouse,
    source: &mut dyn Source,
) -> Result<Pick> {
    shown.merge_nearby(config.merge_distance);
    overlay.set_layout(shown);
    let mut selector = Selector::new(
        shown.targets.len(),
        config.alphabet.len(),
        config.verbs.default_action(),
    )
    .with_auto_accept(config.hint_auto_accept);
    if let Some(action) = start.verb {
        selector.press(Input::Verb(action, Mods::NONE));
    }
    let mut said = Said::Nothing;
    // A scene per drawing: `shown` changes when a block opens, so nothing
    // may keep borrowing it in between.
    let tags = |selector: &Selector, shown: &Snapshot, said: &Said| {
        let scene = Scene {
            targets: &shown.targets,
            holding: start.holding,
        };
        frame(selector, config, &scene, said)
    };
    if start.text {
        // An empty frame opens the keys, with no tag to flash first.
        overlay.show(&Frame::default())?;
        return text(
            overlay,
            config,
            shown,
            selector,
            start.holding,
            mouse,
            source,
        );
    }
    overlay.show(&tags(&selector, shown, &said))?;
    // When the tags are next brought up to date. The wait starts once a
    // scan is done, so a slow scan never runs back to back with the next.
    let mut every = config.refresh;
    let next = |every: Option<Duration>| every.map(|every| Instant::now() + every);
    let mut due = next(every);
    loop {
        let wake = due.into_iter().chain(said.until()).min();
        let Some(event) = overlay.next_event(wake)? else {
            let now = Instant::now();
            if said.until().is_some_and(|until| until <= now) {
                said = Said::Nothing;
                overlay.update(&tags(&selector, shown, &said))?;
            }
            if due.is_none_or(|due| due > now) {
                continue;
            }
            // A scan holds up the keys for as long as it takes, so none
            // while an arrow is held: the pointer would freeze mid-move.
            if overlay.holding() {
                due = next(every);
                continue;
            }
            // A source that can't change is asked once.
            due = None;
            // What shows while a slow scan goes on: the tags as they are,
            // and `scanning` under what's typed.
            let busy = config.info.scanning.then(|| {
                let mut busy = tags(&selector, shown, &said);
                busy.status.info = Some(scanning());
                busy
            });
            let mut slowed = false;
            let mut slow = || {
                if let Some(busy) = &busy {
                    slowed = overlay.update(busy).is_ok();
                }
            };
            let refresh = rescan(config, shown, &mut selector, source, &mut slow)?;
            let Some(refresh) = refresh else {
                continue;
            };
            if refresh != Refresh::Unchanged || slowed {
                overlay.set_layout(shown);
                let frame = tags(&selector, shown, &said);
                if refresh == Refresh::Retag {
                    overlay.show(&frame)?;
                } else {
                    overlay.update(&frame)?;
                }
            }
            every = match refresh {
                Refresh::Unchanged => every.map(slower),
                Refresh::Redraw | Refresh::Retag => config.refresh,
            };
            due = next(every);
            continue;
        };
        let (key, mods, ticks) = match event {
            Event::Escape => (
                Key {
                    code: KeyCode::ESC,
                    shift: false,
                },
                Mods::NONE,
                0,
            ),
            Event::Key(key, mods) => (key, mods, 0),
            Event::Held(key, mods, ticks) => (key, mods, ticks),
            // Only text mode types, and it reads its own keys.
            Event::Typed(..) => continue,
            Event::Interrupted | Event::Closed | Event::KeyboardLost => return Ok(Pick::Leave),
            Event::Monitors => bail!("the monitors changed; run wayhint again"),
            Event::Workspace => return Ok(Pick::Rescan),
        };
        if config.verbs.is_help(key) {
            said = if said == Said::Help {
                Said::Nothing
            } else {
                Said::Help
            };
            overlay.update(&tags(&selector, shown, &said))?;
            continue;
        }
        if !config.verbs.is_close(key)
            && let Some(by) = arrow(key.code, ticks, config)
        {
            mouse.move_by(by)?;
            continue;
        }
        // A key that means nothing (a modifier on its own) leaves the
        // message up; any other key takes it down.
        let Some(input) = input_of(key, mods, config) else {
            continue;
        };
        said = Said::Nothing;
        // The command as it stood before this key: what a block opens with.
        let before = selector.clone();
        match selector.press(input) {
            Step::Typing => {
                overlay.update(&tags(&selector, shown, &said))?;
            }
            Step::Picked { index, .. } if !shown.targets[index].opens.is_empty() => {
                selector = before;
                // A block with no line left to show stays shut, as if its
                // last letter missed.
                if !open(shown, index, &*source, config) {
                    continue;
                }
                selector.reopen(shown.targets.len(), config.alphabet.len());
                due = None;
                // The lines have new labels: discard taps queued for the
                // block before accepting keys for those lines.
                overlay.show(&tags(&selector, shown, &said))?;
            }
            Step::Picked { index, act } => return Ok(Pick::Target { index, act }),
            Step::Here(act) => return Ok(Pick::Here { act }),
            Step::Again(act) => return Ok(Pick::Again { act }),
            Step::Repeat => return Ok(Pick::Repeat),
            // The letter is dropped, and named for `[info] duration`.
            Step::Ignored => {
                if let Input::Letter(letter, _) = input
                    && config.info.no_match
                {
                    let letters = config.alphabet.letters();
                    let case = config.style.case;
                    let typed: String = before
                        .typed()
                        .iter()
                        .chain([&letter])
                        .map(|&i| case.of(letters[i].label))
                        .collect();
                    let until = Instant::now() + config.info.duration;
                    said = Said::Missed(format!("no tag starts with {typed}"), until);
                }
                overlay.update(&tags(&selector, shown, &said))?;
            }
            Step::Exit if start.holding => return Ok(Pick::Release),
            Step::Exit => return Ok(Pick::Leave),
        }
    }
}

/// Opens the block at `index` of `shown` into its lines, those `source`
/// keeps. `false`, with `shown` as it was, if no line is left to show.
fn open(shown: &mut Snapshot, index: usize, source: &dyn Source, config: &Config) -> bool {
    let mut opened = shown.open(index);
    source.keep(&mut opened);
    opened.merge_nearby(config.merge_distance);
    if opened.targets.is_empty() {
        return false;
    }
    *shown = opened;
    true
}

/// The wait after a refresh that found nothing changed: twice `every`, up
/// to `IDLE_REFRESH`, and never shorter than `every`.
pub fn slower(every: Duration) -> Duration {
    (every * 2).min(IDLE_REFRESH).max(every)
}

/// Brings `shown` up to date with a fresh scan from `source`: a target
/// found again keeps its tag, and what was typed for it stays typed (see
/// `selection::relabel`). Says whether tags need drawing or new input;
/// `None` if targets never change (a saved snapshot).
fn rescan(
    config: &Config,
    shown: &mut Snapshot,
    selector: &mut Selector,
    source: &mut dyn Source,
    slow: &mut dyn FnMut(),
) -> Result<Option<Refresh>> {
    let Some(mut fresh) = source.rescan(slow)? else {
        return Ok(None);
    };
    fresh.merge_nearby(config.merge_distance);
    let changed = fresh.obstacles != shown.obstacles || fresh.targets != shown.targets;
    let survivors = fresh.survivors(shown);
    let misleads = selector.relabel(&survivors, config.alphabet.len());
    // Movement, captured ink and tags that came or went need a redraw,
    // not a fresh input gate: that drops every key pressed during the
    // scan, and a page that changes all the time would lose keys at every
    // refresh. Keys that could now pick another target must go, and so
    // must keys for an object reused for a different action, or a
    // replacement window with identical pixel boxes.
    let replaced = fresh.targets.iter().zip(&survivors).any(|(now, was)| {
        was.is_some_and(|i| {
            let old = &shown.targets[i];
            fresh.windows[now.window].window.address != shown.windows[old.window].window.address
                || now.role != old.role
                || now.name != old.name
                || now.editable != old.editable
                || now.opens != old.opens
        })
    });
    *shown = fresh;
    Ok(Some(if misleads || replaced {
        Refresh::Retag
    } else if changed {
        Refresh::Redraw
    } else {
        Refresh::Unchanged
    }))
}

/// A text run's pick: text mode (`search::search`) until a match, a tag
/// or the close key. A block's tag opens it, as in [`pick`], and text mode
/// goes on in its lines. While a hold keeps a button down (`holding`),
/// the close key lets go where it is.
fn text(
    overlay: &mut Overlay,
    config: &Config,
    shown: &mut Snapshot,
    mut selector: Selector,
    holding: bool,
    mouse: &mut dyn Mouse,
    source: &mut dyn Source,
) -> Result<Pick> {
    loop {
        match search::search(overlay, shown, &selector, holding, mouse, source, config)? {
            Pick::Target { index, .. } if !shown.targets[index].opens.is_empty() => {
                if open(shown, index, &*source, config) {
                    selector.reopen(shown.targets.len(), config.alphabet.len());
                }
            }
            pick => return Ok(pick),
        }
    }
}

/// What a key means, an arrow aside: a built-in key, else a verb or
/// repeat, else a tag letter, else nothing. The config keeps command keys
/// out of the alphabet and apart from each other, so the order never
/// decides anything. A tag letter matches with or without Shift, unless
/// Shift on it is a verb's key (`A`); Shift is never a click's.
fn input_of(key: Key, mods: Mods, config: &Config) -> Option<Input> {
    if config.verbs.is_close(key) {
        return Some(Input::Escape);
    }
    match key.code {
        KeyCode::BACKSPACE => Some(Input::Backspace),
        KeyCode::ENTER => Some(Input::Confirm(mods)),
        KeyCode::SPACE => Some(Input::Here(mods)),
        _ if config.verbs.is_repeat(key) => Some(Input::Repeat),
        code => config
            .verbs
            .action_of(key)
            .map(|action| Input::Verb(action, mods))
            .or_else(|| {
                let letter = config.alphabet.index_of(code)?;
                Some(Input::Letter(letter, mods))
            }),
    }
}

/// The tags still possible after what's been typed, in `[style] case`,
/// and the input box.
///
/// The input box names the armed verb, dimmed, or `drop` while a hold
/// keeps a button down: a verb with no key (`--verb`) and `Space`, which
/// uses no tag, show nowhere else. With `[input] hints`, the letters typed
/// follow it there, and the tags keep only the letters still to type.
/// Without, each tag reads as the whole command: everything typed so far,
/// dimmed, then the letters still to type. So an armed verb's key goes in
/// front, wherever it was typed (`kEA` after `k`, with `k` and nothing
/// else dimmed; `kEA` again after `e k`, with `kE` dimmed), where the eyes
/// already are.
///
/// Under what's typed, the box says what `said` holds, or `no targets`
/// while nothing has a tag.
///
/// A tag sits at `[style] position` on its target's visible box, moved by
/// `[style] offset`.
///
fn frame(selector: &Selector, config: &Config, scene: &Scene, said: &Said) -> Frame {
    let letters = config.alphabet.letters();
    let style = &config.style;
    let hints = config.input.hints;
    let prefix = selector
        .armed()
        .and_then(|action| config.verbs.label(action))
        .filter(|_| !hints)
        .unwrap_or("");
    // With hints, what's typed leaves the tags for the input box.
    let (skip, dimmed) = if hints {
        (selector.typed().len(), 0)
    } else {
        (0, prefix.chars().count() + selector.typed().len())
    };
    let open: Vec<(&Vec<usize>, &Target)> = selector
        .labels()
        .iter()
        .zip(scene.targets)
        .filter(|(label, _)| selector.matches(label))
        .collect();
    let letter = |&i: &usize| style.case.of(letters[i].label);
    let tags: Vec<Tag> = open
        .iter()
        .map(|(label, target)| {
            // Keep a completed tag visible while Enter is still needed.
            let (skip, dimmed) = if selector.typed() == label.as_slice() {
                (0, prefix.chars().count() + label.len())
            } else {
                (skip, dimmed)
            };
            let text = prefix.chars().chain(label[skip..].iter().map(letter));
            Tag {
                verb: prefix.chars().count(),
                ..tag(target, text.collect(), dimmed, config)
            }
        })
        .collect();
    let info = match said {
        Said::Help => Some(Message::new(Notice::Help, config.verbs.legend())),
        Said::Missed(message, _) => Some(Message::new(Notice::NoMatch, message.clone())),
        Said::Nothing if scene.targets.is_empty() && config.info.no_targets => {
            Some(Message::new(Notice::NoTargets, "no targets"))
        }
        Said::Nothing => None,
    };
    Frame {
        tags,
        status: Status {
            info,
            ..status(selector, config, scene.holding)
        },
        ..Frame::default()
    }
}

/// `target`'s tag, reading `text` with its first `typed` characters dimmed.
pub fn tag(target: &Target, text: String, typed: usize, config: &Config) -> Tag {
    Tag {
        at: spot(target, config),
        target: Some(target.hit),
        position: if target.role == "terminal" {
            Position::Center
        } else {
            config.style.position
        },
        text,
        typed,
        verb: 0,
        kind: if target.in_pixels() {
            Kind::Pixels
        } else {
            Kind::Plain
        },
    }
}

/// A label on the box `shown`, for the point `hit`, placed as a target's
/// tag is: text mode's labels on its matches, in `[style.labels]`.
pub fn tag_on(shown: Rect, hit: Point, text: String, typed: usize, config: &Config) -> Tag {
    Tag {
        at: spot_on(shown, config),
        target: Some(hit),
        position: config.style.position,
        text,
        typed,
        verb: 0,
        kind: Kind::Label,
    }
}

/// Where `target`'s tag goes: at `[style] position` on its visible box,
/// moved by `[style] offset`; a terminal's on its cursor.
fn spot(target: &Target, config: &Config) -> Point {
    if target.role == "terminal" {
        return target.hit;
    }
    spot_on(target.shown(), config)
}

/// The same for a box.
fn spot_on(shown: Rect, config: &Config) -> Point {
    let [x, y] = config.style.offset;
    config.style.position.on(shown).offset(Point::new(x, y))
}

/// The input box with the tags up: the armed verb's name, dimmed, or
/// `drop` while a hold keeps a button down (whatever comes next lets go),
/// then, with `[input] hints`, the letters typed so far.
fn status(selector: &Selector, config: &Config, holding: bool) -> Status {
    let letters = config.alphabet.letters();
    let typed = selector
        .typed()
        .iter()
        .map(|&i| config.style.case.of(letters[i].label))
        .collect();
    Status::tags(
        typed,
        selector.armed().map(Action::name),
        holding,
        config.input.hints,
    )
}

/// How far the arrow `code` goes: `[pointer] nudge` px its way, and held
/// down, on at each of its `ticks`, faster (see `speed`). `None` for a key
/// that is no arrow.
pub fn arrow(code: KeyCode, ticks: u16, config: &Config) -> Option<Point> {
    let (x, y) = match code {
        KeyCode::UP => (0, -1),
        KeyCode::DOWN => (0, 1),
        KeyCode::LEFT => (-1, 0),
        KeyCode::RIGHT => (1, 0),
        _ => return None,
    };
    let step = config.nudge * speed(ticks);
    Some(Point::new(x * step, y * step))
}

/// How many `[pointer] nudge` steps an arrow moves at its `ticks`th tick
/// held down, 0 being its press: one, and one more every `SPEED_UP` ticks,
/// up to `TOP_SPEED`.
fn speed(ticks: u16) -> i32 {
    i32::from((1 + ticks / SPEED_UP).min(TOP_SPEED))
}

/// Where the pointer at `at` goes, moved `by`: kept `EDGE` px inside the
/// one of `screens` (the monitors) that `at` is on, so an arrow too many
/// stops by its edge instead of jumping to another monitor or a point no
/// monitor has. Not moved, that's `at` itself, wherever it is.
pub fn moved(at: Point, by: Point, screens: &[Rect]) -> Point {
    if by == Point::default() {
        return at;
    }
    let moved = at.offset(by);
    match screens.iter().find(|s| s.contains(at)) {
        Some(screen) => screen.inset(EDGE).clamp(moved),
        None => moved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Source for Snapshot {
        fn rescan(&mut self, _: &mut dyn FnMut()) -> Result<Option<Snapshot>> {
            Ok(Some(self.clone()))
        }

        fn keep(&self, _: &mut Snapshot) {
            unreachable!("refresh tests do not open blocks")
        }

        fn read_text(&mut self, _: &Snapshot) -> Vec<Text> {
            unreachable!("refresh tests do not enter text mode")
        }

        fn locate(&mut self, _: &Snapshot, _: &[Text], _: &[Span]) -> Vec<Vec<Rect>> {
            unreachable!("refresh tests do not locate text")
        }

        fn shows(&self, _: &Snapshot, _: usize, _: Point) -> bool {
            unreachable!("refresh tests do not locate text")
        }
    }

    fn refresh_scene() -> (Config, Snapshot, Selector) {
        let config = Config::parse(
            "[keys]\nalphabet = \"ab\"\nleft = \"\"",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let window = crate::desktop::Window {
            address: "0x1".into(),
            pid: 1,
            class: "test".into(),
            title: String::new(),
            rect: Rect::new(0, 0, 800, 600),
            monitor: 0,
            focused: true,
            kind: crate::desktop::Kind::Client,
            frames: Vec::new(),
        };
        let found = crate::pixels::Found {
            direct: vec![[10, 10, 30, 30], [50, 10, 70, 30], [90, 10, 110, 30]],
            ..crate::pixels::Found::default()
        };
        let mut snapshot = Snapshot::build(
            1,
            0,
            [crate::snapshot::Read::Pixels {
                window: &window,
                found: &found,
                scale: 1.0,
                stats: &crate::desktop::Stats::default(),
            }],
        );
        for (i, target) in snapshot.targets.iter_mut().enumerate() {
            target.object = Some(crate::desktop::tree::ObjectId {
                bus: ":1.7".into(),
                path: format!("/{i}"),
            });
        }
        let selector = Selector::new(3, config.alphabet.len(), Action::Left);
        (config, snapshot, selector)
    }

    #[test]
    fn an_unchanged_screen_is_refreshed_less_often_up_to_two_seconds() {
        let ms = Duration::from_millis;
        assert_eq!(slower(ms(500)), ms(1000));
        assert_eq!(slower(ms(1500)), ms(2000));
        assert_eq!(slower(ms(2000)), ms(2000));
        // A setting slower than that stays as it is.
        assert_eq!(slower(ms(5000)), ms(5000));
    }

    #[test]
    fn a_refresh_of_the_same_controls_keeps_hint_input_live() {
        let (config, mut shown, mut selector) = refresh_scene();
        let mut fresh = shown.clone();
        // An animated control moves while the user presses the first a
        // of aa/ab. Resetting the gate here loses its release entirely.
        fresh.targets[1].rect.x += 1;
        fresh.targets[1].visible.x += 1;
        fresh.targets[1].hit.x += 1;
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Redraw),
            "a redraw must keep queued keys and presses awaiting release"
        );
        assert_eq!(selector.press(Input::Letter(0, Mods::NONE)), Step::Typing);
        assert_eq!(selector.typed(), [0]);
    }

    #[test]
    fn obstacle_changes_and_target_reordering_keep_partial_hints() {
        let (config, mut shown, mut selector) = refresh_scene();
        assert_eq!(selector.press(Input::Letter(0, Mods::NONE)), Step::Typing);
        let mut fresh = shown.clone();
        fresh.obstacles.push(Rect::new(200, 200, 20, 20));
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Redraw)
        );
        fresh.targets.reverse();
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Redraw)
        );
        assert_eq!(selector.typed(), [0]);
        assert!(matches!(
            selector.press(Input::Letter(0, Mods::NONE)),
            Step::Picked { index: 1, .. }
        ));
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Unchanged)
        );
    }

    #[test]
    fn a_tag_that_comes_in_a_refresh_keeps_hint_input_live() {
        // a s d over asdf, and a fourth control loads under the free `f`:
        // no key typed for a, s or d can reach it.
        let (_, mut shown, _) = refresh_scene();
        let config = Config::parse(
            "[keys]\nalphabet = \"asdf\"\nleft = \"\"",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let mut selector = Selector::new(3, config.alphabet.len(), Action::Left);
        let mut fresh = shown.clone();
        let mut late = fresh.targets[2].clone();
        late.rect.x += 200;
        late.visible.x += 200;
        late.hit.x += 200;
        late.object.as_mut().unwrap().path = "/late".into();
        fresh.targets.push(late);
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Redraw),
            "a gate reset here drops every key pressed during the scan"
        );
    }

    #[test]
    fn changed_tag_meanings_still_reset_hint_input() {
        let check = |change: fn(&mut Snapshot)| {
            let (config, mut shown, mut selector) = refresh_scene();
            let mut fresh = shown.clone();
            change(&mut fresh);
            assert_eq!(
                rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
                Some(Refresh::Retag)
            );
        };
        check(|s| s.targets[1].name = "Replacement action".into());
        check(|s| s.targets[1].role = "entry".into());
        check(|s| s.targets[1].editable = true);
        check(|s| s.targets[1].opens.push(Rect::new(50, 10, 20, 5)));
        check(|s| s.targets[1].object.as_mut().unwrap().path = "/new".into());
        check(|s| s.windows[0].window.address = "0x2".into());
        check(|s| s.targets.truncate(2));
    }

    #[test]
    fn pixel_hints_stay_live_when_their_window_moves_but_not_when_replaced() {
        let (config, mut shown, mut selector) = refresh_scene();
        for target in &mut shown.targets {
            target.object = None;
        }
        let mut fresh = shown.clone();
        let by = Point::new(10, 20);
        fresh.windows[0].window.rect = fresh.windows[0].window.rect.offset(by);
        for target in &mut fresh.targets {
            target.rect = target.rect.offset(by);
            target.visible = target.visible.offset(by);
            target.hit = target.hit.offset(by);
        }
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Redraw)
        );
        fresh.windows[0].window.address = "0x2".into();
        assert_eq!(
            rescan(&config, &mut shown, &mut selector, &mut fresh, &mut || {}).unwrap(),
            Some(Refresh::Retag)
        );
    }

    #[test]
    fn keys_decode_built_ins_then_verbs_then_letters() {
        let config = Config::parse(
            "[keys]\nhover = \"_\"",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        // A key, with Shift or without; no click modifier held.
        let input = |code, shift| {
            let key = Key {
                code: KeyCode::from_evdev(code),
                shift,
            };
            input_of(key, Mods::NONE, &config)
        };
        assert_eq!(input(57, false), Some(Input::Here(Mods::NONE)));
        assert_eq!(input(28, true), Some(Input::Confirm(Mods::NONE)));
        assert_eq!(
            input(51, false),
            Some(Input::Verb(Action::Right, Mods::NONE))
        );
        assert_eq!(
            input(12, true),
            Some(Input::Verb(Action::Hover, Mods::NONE))
        );
        assert_eq!(input(12, false), None, "- alone is nothing");
        assert_eq!(
            input(30, true),
            Some(Input::Letter(0, Mods::NONE)),
            "Shift on a is still a, and no Shift-click"
        );
        assert_eq!(input(105, false), None, "an arrow is no input");
        assert_eq!(arrow(KeyCode::LEFT, 0, &config), Some(Point::new(-10, 0)));
        assert_eq!(arrow(KeyCode::UP, 0, &config), Some(Point::new(0, -10)));
        assert_eq!(arrow(KeyCode::RIGHT, 7, &config), Some(Point::new(10, 0)));
        assert_eq!(
            arrow(KeyCode::RIGHT, 8, &config),
            Some(Point::new(20, 0)),
            "held, faster"
        );
        assert_eq!(arrow(KeyCode::from_evdev(30), 0, &config), None);
    }

    #[test]
    fn the_configured_close_key_replaces_escape_and_overrides_built_ins() {
        let layout = crate::keys::Layout::named("us", "");
        for name in ["q", "Space", "Up", "Enter"] {
            let config = Config::parse(&format!("[keys]\nclose = {name:?}"), &layout).unwrap();
            let code = crate::keys::named(name).unwrap_or(KeyCode::from_evdev(16));
            assert_eq!(
                input_of(Key { code, shift: false }, Mods::NONE, &config),
                Some(Input::Escape)
            );
            assert_eq!(
                input_of(
                    Key {
                        code: KeyCode::ESC,
                        shift: false
                    },
                    Mods::NONE,
                    &config
                ),
                None
            );
        }
    }

    #[test]
    fn a_held_arrow_speeds_up_to_a_top_speed() {
        let speeds: Vec<i32> = [0, 7, 8, 16, 24, 32, 1000].map(speed).into();
        assert_eq!(speeds, [1, 1, 2, 3, 4, 4, 4]);
    }

    #[test]
    fn the_arrows_stop_inside_the_edge_of_the_pointers_monitor() {
        let screens = [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1920, 1080)];
        let at = Point::new(10, 500);
        assert_eq!(
            moved(at, Point::new(28, -14), &screens),
            Point::new(38, 486)
        );
        assert_eq!(
            moved(at, Point::new(-42, 0), &screens),
            Point::new(EDGE, 500)
        );
        let corner = Point::new(1, 1079);
        assert_eq!(
            moved(corner, Point::new(10, 0), &screens),
            Point::new(11, 1079 - EDGE),
            "from the edge: it comes inside it too"
        );
        assert_eq!(
            moved(corner, Point::default(), &screens),
            corner,
            "not moved: it stays where it is"
        );
        let right = Point::new(2100, 500);
        assert_eq!(
            moved(right, Point::default(), &screens[..1]),
            right,
            "not moved: it stays where it is, on any monitor"
        );
        assert_eq!(
            moved(right, Point::new(-196, 0), &screens),
            Point::new(1920 + EDGE, 500),
            "from the right monitor it stops inside its left edge"
        );
    }

    #[test]
    fn tags_take_the_case_and_say_which_came_in_pixels() {
        let tree = Target {
            object: Some(crate::desktop::tree::ObjectId {
                bus: ":1.7".into(),
                path: "/ok".into(),
            }),
            window: 0,
            rect: Rect::new(80, 90, 40, 20),
            visible: Rect::new(80, 90, 40, 20),
            hit: Point::new(100, 100),
            role: "button".into(),
            name: String::new(),
            editable: false,
            clips: Vec::new(),
            opens: Vec::new(),
        };
        let guessed = Target {
            object: None,
            role: "pixels".into(),
            ..tree.clone()
        };
        let targets = [tree, guessed];
        let scene = Scene {
            targets: &targets,
            holding: false,
        };
        let selector = Selector::new(2, 2, Action::Left);
        let tags = |extra: &str| {
            let config = Config::parse(
                &format!("[keys]\nalphabet = \"ab\"\nleft = \"\"\n{extra}"),
                &crate::keys::Layout::named("us", ""),
            )
            .unwrap();
            frame(&selector, &config, &scene, &Said::Nothing)
                .tags
                .into_iter()
                .map(|t| (t.text, t.kind == Kind::Pixels))
                .collect::<Vec<_>>()
        };
        let pair = |a: &str, b: &str| [(a.to_owned(), false), (b.to_owned(), true)];
        assert_eq!(tags(""), pair("A", "B"));
        assert_eq!(tags("[style]\ncase = \"lower\""), pair("a", "b"));
    }

    #[test]
    fn terminal_hint_is_centered_on_the_cursor_despite_general_style_offsets() {
        let config = Config::parse(
            "[style]\nposition = \"top-left\"\noffset = [5, 2]",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let mut targets = three_buttons();
        let cell = Rect::new(153, 237, 8, 18);
        targets[0].rect = cell;
        targets[0].visible = cell;
        targets[0].hit = cell.center();
        targets[0].role = "terminal".into();
        let scene = Scene {
            targets: &targets,
            holding: false,
        };
        let selector = Selector::new(targets.len(), config.alphabet.len(), Action::Left);
        let frame = frame(&selector, &config, &scene, &Said::Nothing);
        assert_eq!(frame.tags[0].position, Position::Center);
        assert_eq!(frame.tags[0].at, Point::new(157, 246));
        assert_eq!(frame.tags[0].target, Some(Point::new(157, 246)));
        assert_eq!(
            frame.tags[0].kind,
            Kind::Pixels,
            "keep the existing terminal hint style"
        );
        assert_eq!(frame.tags[1].position, Position::TopLeft);
        assert_eq!(frame.tags[1].at, Point::new(185, 92));
    }

    /// Three buttons in a row.
    fn three_buttons() -> Vec<Target> {
        (1..=3)
            .map(|i| Target {
                object: None,
                window: 0,
                rect: Rect::new(i * 100 - 20, 90, 40, 20),
                visible: Rect::new(i * 100 - 20, 90, 40, 20),
                hit: Point::new(i * 100, 100),
                role: "button".into(),
                name: String::new(),
                editable: false,
                clips: Vec::new(),
                opens: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn with_hints_what_is_typed_leaves_the_tags_for_the_input_box() {
        let targets = three_buttons();
        let scene = |holding| Scene {
            targets: &targets,
            holding,
        };
        let shown = |extra: &str, holding| {
            let config = Config::parse(
                &format!("[keys]\nalphabet = \"ab\"\nleft = \"\"\nright = \",\"\n{extra}"),
                &crate::keys::Layout::named("us", ""),
            )
            .unwrap();
            let mut selector = Selector::new(3, 2, Action::Left);
            selector.press(Input::Letter(0, Mods::NONE));
            selector.press(Input::Verb(Action::Right, Mods::NONE));
            let frame = frame(&selector, &config, &scene(holding), &Said::Nothing);
            let tags: Vec<(String, usize)> =
                frame.tags.into_iter().map(|t| (t.text, t.typed)).collect();
            assert_eq!(frame.status.input, "A", "what's typed, wherever it shows");
            (tags, frame.status.line())
        };
        let owned = |text: &str, typed| (text.to_owned(), typed);
        let (tags, input) = shown("", false);
        assert_eq!(tags, [owned("A", 0), owned("B", 0)], "only what's left");
        assert_eq!(input, Some(owned("right A", 6)), "the verb dimmed");
        let (tags, input) = shown("[input]\nhints = false", false);
        assert_eq!(
            tags,
            [owned(",AA", 2), owned(",AB", 2)],
            "the whole command, typed keys dimmed"
        );
        assert_eq!(input, Some(owned("right", 5)));
        let (_, input) = shown("[input]\nvisible = false", false);
        assert_eq!(input, Some(owned("right A", 6)), "drawn or not");
        let (_, input) = shown("", true);
        assert_eq!(
            input,
            Some(owned("drop A", 5)),
            "a hold's drop beats a verb"
        );
    }

    #[test]
    fn a_completed_hint_stays_visible_while_waiting_for_enter() {
        let config = Config::parse(
            "[hints]\nauto_accept = false",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let targets = three_buttons();
        let mut selector = Selector::new(3, config.alphabet.len(), Action::Left)
            .with_auto_accept(config.hint_auto_accept);
        assert_eq!(selector.press(Input::Letter(0, Mods::NONE)), Step::Typing);
        let scene = Scene {
            targets: &targets,
            holding: false,
        };
        let frame = frame(&selector, &config, &scene, &Said::Nothing);
        assert_eq!(frame.tags.len(), 1);
        assert_eq!((frame.tags[0].text.as_str(), frame.tags[0].typed), ("A", 1));
        assert_eq!(frame.tags[0].target, Some(targets[0].hit));
    }

    #[test]
    fn the_second_line_says_no_targets_a_miss_or_every_key() {
        let config = Config::parse("", &crate::keys::Layout::named("us", "")).unwrap();
        let selector = Selector::new(0, config.alphabet.len(), Action::Left);
        let targets = three_buttons();
        let scene = |targets| Scene {
            targets,
            holding: false,
        };
        let info =
            |targets, said: &Said| frame(&selector, &config, &scene(targets), said).status.info;
        assert_eq!(
            info(&[], &Said::Nothing),
            Some(Message::new(Notice::NoTargets, "no targets"))
        );
        assert_eq!(info(&targets, &Said::Nothing), None);
        let missed = Said::Missed("no tag starts with Q".into(), Instant::now());
        assert_eq!(
            info(&[], &missed),
            Some(Message::new(Notice::NoMatch, "no tag starts with Q")),
            "a miss beats no targets"
        );
        let help = info(&targets, &Said::Help).unwrap();
        assert_eq!(help.kind, Notice::Help);
        assert!(help.text.contains("Esc\tleave"), "{}", help.text);
        let quiet = Config::parse(
            "[info]\nno_targets = false",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let frame = frame(&selector, &quiet, &scene(&[]), &Said::Nothing);
        assert_eq!(frame.status.info, None, "turned off");
    }
}
