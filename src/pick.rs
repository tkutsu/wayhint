//! Choosing one target: tags on screen, keys in, until a tag is typed or
//! the user leaves. The glue between `overlay` (pixels and keys) and
//! `selection` (what the keys mean), so neither has to know the other.

use std::time::Instant;

use anyhow::{Result, bail};

use crate::{
    action::{Act, Action, Mods},
    config::Config,
    geometry::{Point, Rect},
    keys::{Key, KeyCode},
    overlay::{Dot, Event, Frame, Overlay, Tag},
    search::{self, Chosen},
    selection::{Input, Selector, Step},
    snapshot::{Snapshot, Target},
    style::Position,
    text::{Span, Text},
};

/// How far above the monitor's bottom edge the status pill sits, in px.
const PILL_LIFT: i32 = 48;
/// How far inside its monitor's edge a nudge stops, in px: far enough for
/// the whole dot to show.
const EDGE: i32 = 5;
/// A held arrow speeds up by one `[pointer] nudge` a tick every this many
/// ticks (25 ms each, `overlay/hold.rs`), up to `TOP_SPEED` a tick.
const SPEED_UP: u16 = 8;
const TOP_SPEED: u16 = 4;

/// How a pick ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Do `act` at the target at `index`.
    Target { index: usize, act: Act },
    /// Do `act` at the pointer (`Space`): `from`, moved by the nudge. A
    /// bare `Space` reads the pointer as it's pressed; a nudged one keeps
    /// the pointer its dot was drawn from, so it lands on the dot.
    Here { act: Act, from: Point },
    /// Do the last command again, on its target.
    Repeat,
    /// A match chosen in text mode (boxed: it carries its target).
    Text(Box<Chosen>),
    /// `Esc`, Ctrl+C, or the overlay went away: nothing chosen.
    Leave,
    /// The monitor shows another workspace: nothing chosen, and the tags
    /// are for windows that left.
    Rescan,
}

/// What the screen holds while a pick runs: the targets, the monitor they
/// are on, and the pointer, if known, for where a nudged `Space` lands.
struct Scene<'a> {
    targets: &'a [Target],
    screen: Rect,
    pointer: Option<Point>,
}

impl<'a> Scene<'a> {
    const fn new(targets: &'a [Target], screen: Rect, pointer: Option<Point>) -> Self {
        Self {
            targets,
            screen,
            pointer,
        }
    }
}

/// Where a pick's targets come from, asked again while the tags are up.
pub trait Source {
    /// The targets as they are now, or `None` when they can't change (a
    /// saved snapshot). A run scans its windows again.
    fn rescan(&mut self) -> Result<Option<Snapshot>>;
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

/// Shows tags for `shown`'s targets and reads keys until one is picked.
/// `verb` (from `--verb`) is armed as if its key were the first one typed.
/// `cursor` reads where the pointer is. The overlay never sees the mouse
/// move, so it's read again at every key: a dot shows where a nudged
/// `Space` lands from the pointer as of the last key.
///
/// Every `[scan] refresh` with no pick, `source` scans again and the tags
/// follow: a target found again keeps its tag, and what was typed for it
/// stays typed (see `Selector::relabel`).
///
/// A block's tag (a pixel target with lines in `opens`) opens it instead
/// of picking: `shown` becomes the block opened, its lines filtered by
/// `source` (none under another window or off the monitor), and the
/// command goes on there, with what was typed before the block's tag (a
/// verb, its Shift, arrows). The lines stay as they were opened: a rescan
/// has no blocks to open. `shown` ends as the snapshot the pick was made
/// in.
///
/// The text key hands the keys to text mode (`search::search`), with the
/// verb and arrows typed before it; leaving text mode brings the same tags
/// back.
pub fn pick(
    overlay: &mut Overlay,
    config: &Config,
    shown: &mut Snapshot,
    verb: Option<Action>,
    cursor: &dyn Fn() -> Result<Point>,
    source: &mut dyn Source,
) -> Result<Pick> {
    shown.merge_nearby(config.merge_distance);
    overlay.set_layout(shown);
    let mut selector = Selector::new(
        shown.targets.len(),
        config.alphabet.len(),
        config.verbs.default_action(),
    );
    if let Some(action) = verb {
        selector.press(Input::Verb(action, Mods::NONE));
    }
    let (screen, mut pointer) = (overlay.monitor().rect, cursor().ok());
    // A scene per drawing: `shown` changes when a block opens, so nothing
    // may keep borrowing it in between.
    let tags = |selector: &Selector, shown: &Snapshot, pointer| {
        frame(
            selector,
            config,
            &Scene::new(&shown.targets, screen, pointer),
        )
    };
    overlay.show(&tags(&selector, shown, pointer))?;
    // When the tags are next brought up to date. The wait starts once a
    // scan is done, so a slow scan never runs back to back with the next.
    let next = || config.refresh.map(|every| Instant::now() + every);
    let mut due = next();
    loop {
        let Some(event) = overlay.next_event(due)? else {
            // A scan holds up the keys for as long as it takes, so none
            // while an arrow is held: the nudge would freeze mid-move.
            if overlay.holding() {
                due = next();
                continue;
            }
            // A source that can't change is asked once.
            due = None;
            let Some(changed) = rescan(config, shown, &mut selector, source)? else {
                continue;
            };
            if changed {
                overlay.set_layout(shown);
                pointer = cursor().ok();
                // A label may name a different target now. Keys aimed at
                // the previous scan must not pick from the new one.
                overlay.show(&tags(&selector, shown, pointer))?;
            }
            due = next();
            continue;
        };
        let (key, mods, ticks) = match event {
            Event::Key(key, mods) => (key, mods, 0),
            Event::Held(key, mods, ticks) => (key, mods, ticks),
            // Only text mode types, and it reads its own keys.
            Event::Typed(..) => continue,
            Event::Interrupted | Event::Closed | Event::KeyboardLost => return Ok(Pick::Leave),
            Event::Resized => bail!("the monitor changed size; run wayhint again"),
            Event::Workspace => return Ok(Pick::Rescan),
        };
        let Some(input) = input_of(key, mods, ticks, config) else {
            continue;
        };
        // The command as it stood before this key: what a block opens with.
        let before = selector.clone();
        match selector.press(input) {
            Step::Typing => {
                pointer = cursor().ok();
                overlay.update(&tags(&selector, shown, pointer))?;
            }
            Step::Picked { index, .. } if !shown.targets[index].opens.is_empty() => {
                let mut opened = shown.open(index);
                source.keep(&mut opened);
                opened.merge_nearby(config.merge_distance);
                selector = before;
                // A block with no line left to show stays shut, as if its
                // last letter missed.
                if opened.targets.is_empty() {
                    continue;
                }
                *shown = opened;
                selector.reopen(shown.targets.len(), config.alphabet.len());
                due = None;
                // The lines have new labels: discard taps queued for the
                // block before accepting keys for those lines.
                overlay.show(&tags(&selector, shown, pointer))?;
            }
            Step::Picked { index, act } => return Ok(Pick::Target { index, act }),
            Step::Here(act) => {
                let from = match pointer {
                    Some(drawn) if act.nudge != Point::default() => drawn,
                    _ => cursor()?,
                };
                return Ok(Pick::Here { act, from });
            }
            Step::Repeat => return Ok(Pick::Repeat),
            Step::Text => {
                if let Some(pick) = search::search(overlay, shown, &selector, source)? {
                    return Ok(pick);
                }
                // Back to the tags of the same scan, as they were left.
                pointer = cursor().ok();
                overlay.show(&tags(&selector, shown, pointer))?;
                due = next();
            }
            Step::Ignored => {}
            Step::Exit => return Ok(Pick::Leave),
        }
    }
}

/// Brings `shown` up to date with a fresh scan from `source`: a target
/// found again keeps its tag, and what was typed for it stays typed (see
/// `selection::relabel`). Whether the targets changed, and so must be
/// drawn again; `None` if they never can (a saved snapshot).
fn rescan(
    config: &Config,
    shown: &mut Snapshot,
    selector: &mut Selector,
    source: &mut dyn Source,
) -> Result<Option<bool>> {
    let Some(mut fresh) = source.rescan()? else {
        return Ok(None);
    };
    fresh.merge_nearby(config.merge_distance);
    let changed = targets_changed(shown, &fresh);
    selector.relabel(&fresh.survivors(shown), config.alphabet.len());
    *shown = fresh;
    Ok(Some(changed))
}

/// Pixel targets can have identical fields in a replacement window, so
/// compare their owners too before deciding whether tags must be redrawn.
fn targets_changed(before: &Snapshot, after: &Snapshot) -> bool {
    after.obstacles != before.obstacles
        || after.targets != before.targets
        || after.targets.iter().zip(&before.targets).any(|(now, old)| {
            after.windows[now.window].window.address != before.windows[old.window].window.address
        })
}

/// What a key means: a built-in key, else a verb or repeat, else a tag
/// letter, else nothing. The config keeps command keys out of the alphabet
/// and apart from each other, so the order never decides anything. A tag letter matches with or
/// without Shift: Shift on it is a Shift-click, carried in `mods`. An
/// arrow moves the command `[pointer] nudge` px its way, whatever is held,
/// and held down it goes on, faster at each of its `ticks` (see `speed`).
fn input_of(key: Key, mods: Mods, ticks: u16, config: &Config) -> Option<Input> {
    let step = config.nudge * speed(ticks);
    let nudge = |x, y| Some(Input::Nudge(Point::new(x * step, y * step)));
    match key.code {
        KeyCode::ESC => Some(Input::Escape),
        KeyCode::BACKSPACE => Some(Input::Backspace),
        KeyCode::SPACE => Some(Input::Here(mods)),
        KeyCode::UP => nudge(0, -1),
        KeyCode::DOWN => nudge(0, 1),
        KeyCode::LEFT => nudge(-1, 0),
        KeyCode::RIGHT => nudge(1, 0),
        _ if config.verbs.is_repeat(key) => Some(Input::Repeat),
        _ if config.verbs.is_text(key) => Some(Input::Text),
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

/// The tags still possible after what's been typed, drawn upper case, and
/// where each would land if nudged.
///
/// Each tag reads as the whole command: everything typed so far, dimmed,
/// then the letters still to type. So an armed verb's key goes in front,
/// wherever it was typed (`kEA` after `k`, with `k` and nothing else
/// dimmed; `kEA` again after `e k`, with `kE` dimmed), where the eyes
/// already are. Its name also shows as a pill at the bottom of the screen,
/// for a verb that has no key (`--verb`) and for `Space`, which uses no
/// tag. The pill is drawn as one more tag: same look, no new drawing code.
///
/// A tag sits at `[style] position` on its target's visible box, moved by
/// `[style] offset`. The pill stays centred.
///
/// A nudge leaves the tags where they are and puts a dot where each one
/// lands, one for `Space` too, at the pointer: a tag moved by the arrows
/// would cover the very spot being aimed at. A line joins each dot to its
/// tag. Only the tags still open get them, so they thin out with every
/// letter typed, nudged before it or after.
fn frame(selector: &Selector, config: &Config, scene: &Scene) -> Frame {
    let letters = config.alphabet.letters();
    let prefix = selector
        .armed()
        .and_then(|action| config.verbs.label(action))
        .unwrap_or("");
    let typed = prefix.chars().count() + selector.typed().len();
    let open: Vec<(&Vec<usize>, &Target)> = selector
        .labels()
        .iter()
        .zip(scene.targets)
        .filter(|(label, _)| selector.matches(label))
        .collect();
    let style = &config.style;
    let spot = |target: &Target| {
        let [x, y] = style.offset;
        style.position.on(target.shown()).offset(Point::new(x, y))
    };
    let mut tags: Vec<Tag> = open
        .iter()
        .map(|(label, target)| Tag {
            at: spot(target),
            target: Some(target.hit),
            position: style.position,
            text: prefix
                .chars()
                .chain(label.iter().map(|&i| upper(letters[i].label)))
                .collect(),
            typed,
        })
        .collect();
    if let Some(action) = selector.armed() {
        tags.push(pill(scene.screen, action.name().into(), 0));
    }
    let nudge = selector.nudge();
    let dots = if nudge == Point::default() {
        Vec::new()
    } else {
        // Each line runs from the tag to where its click lands: from its
        // hit point, nudged.
        let starts = open
            .iter()
            .enumerate()
            .map(|(i, (_, target))| (spot(target), target.hit, Some(i)));
        // A pointer on another monitor draws nothing here, as it should:
        // `lands` keeps its dot on that monitor, off this overlay.
        starts
            .chain(scene.pointer.map(|p| (p, p, None)))
            .map(|(from, hit, tag)| Dot {
                from,
                at: lands(hit, nudge, &[scene.screen]),
                tag,
            })
            .collect()
    };
    Frame {
        tags,
        dots,
        highlights: Vec::new(),
    }
}

/// The status pill: `text` centred near the bottom of `screen`, its first
/// `typed` characters dimmed. Drawn as one more tag: same look, no new
/// drawing code.
pub fn pill(screen: Rect, text: String, typed: usize) -> Tag {
    Tag {
        at: Point::new(screen.center().x, screen.bottom() - PILL_LIFT),
        position: Position::Center,
        text,
        target: None,
        typed,
    }
}

/// How many `[pointer] nudge` steps an arrow moves at its `ticks`th tick
/// held down, 0 being its press: one, and one more every `SPEED_UP` ticks,
/// up to `TOP_SPEED`.
fn speed(ticks: u16) -> i32 {
    i32::from((1 + ticks / SPEED_UP).min(TOP_SPEED))
}

/// Where a command nudged by `nudge` lands for the point `at`: kept `EDGE`
/// px inside the one of `screens` (the monitors) that `at` is on, so an
/// arrow too many stops by its edge instead of jumping to another monitor
/// or a point no monitor has, with its dot in sight. Without a nudge that's
/// `at` itself, wherever it is.
pub fn lands(at: Point, nudge: Point, screens: &[Rect]) -> Point {
    if nudge == Point::default() {
        return at;
    }
    let moved = at.offset(nudge);
    match screens.iter().find(|s| s.contains(at)) {
        Some(screen) => screen.inset(EDGE).clamp(moved),
        None => moved,
    }
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
        let config = Config::parse(
            "[keys]\nhover = \"_\"",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        // A key and the modifiers held on it, Shift alone or none.
        let input = |code, shift| {
            let key = Key {
                code: KeyCode::from_evdev(code),
                shift,
            };
            let mods = Mods {
                shift,
                ..Mods::NONE
            };
            input_of(key, mods, 0, &config)
        };
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        assert_eq!(input(57, false), Some(Input::Here(Mods::NONE)));
        assert_eq!(
            input(19, false),
            Some(Input::Verb(Action::Right, Mods::NONE))
        );
        assert_eq!(input(12, true), Some(Input::Verb(Action::Hover, shift)));
        assert_eq!(input(12, false), None, "- alone is nothing");
        assert_eq!(
            input(30, true),
            Some(Input::Letter(0, shift)),
            "a Shift-click on a"
        );
        assert_eq!(input(105, false), Some(Input::Nudge(Point::new(-10, 0))));
        assert_eq!(
            input(103, true),
            Some(Input::Nudge(Point::new(0, -10))),
            "Shift changes nothing on an arrow"
        );
        let right = Key {
            code: KeyCode::RIGHT,
            shift: false,
        };
        let held = |ticks| input_of(right, Mods::NONE, ticks, &config);
        assert_eq!(held(7), Some(Input::Nudge(Point::new(10, 0))));
        assert_eq!(
            held(8),
            Some(Input::Nudge(Point::new(20, 0))),
            "held, faster"
        );
    }

    #[test]
    fn a_held_arrow_speeds_up_to_a_top_speed() {
        let speeds: Vec<i32> = [0, 7, 8, 16, 24, 32, 1000].map(speed).into();
        assert_eq!(speeds, [1, 1, 2, 3, 4, 4, 4]);
    }

    #[test]
    fn a_nudge_stops_inside_the_edge_of_its_own_monitor() {
        let screens = [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1920, 1080)];
        let at = Point::new(10, 500);
        assert_eq!(
            lands(at, Point::new(28, -14), &screens),
            Point::new(38, 486)
        );
        assert_eq!(
            lands(at, Point::new(-42, 0), &screens),
            Point::new(EDGE, 500)
        );
        let corner = Point::new(1, 1079);
        assert_eq!(
            lands(corner, Point::new(10, 0), &screens),
            Point::new(11, 1079 - EDGE),
            "a hit point on the edge: its dot comes inside it too"
        );
        assert_eq!(
            lands(corner, Point::default(), &screens),
            corner,
            "no nudge: the click stays where it is"
        );
        let right = Point::new(2100, 500);
        assert_eq!(
            lands(right, Point::default(), &screens[..1]),
            right,
            "no nudge: the pointer stays where it is, on any monitor"
        );
        assert_eq!(
            lands(right, Point::new(-196, 0), &screens),
            Point::new(1920 + EDGE, 500),
            "a nudge from the right monitor stops inside its left edge"
        );
    }

    #[test]
    fn a_nudged_space_dot_follows_the_pointer() {
        let config = Config::parse("", &crate::keys::Layout::named("us", "")).unwrap();
        let mut selector = Selector::new(0, config.alphabet.len(), Action::Left);
        selector.press(Input::Nudge(Point::new(14, 0)));
        let scene = |x| Scene {
            targets: &[],
            screen: Rect::new(0, 0, 1920, 1080),
            pointer: Some(Point::new(x, 100)),
        };
        let dot = |x| Dot {
            from: Point::new(x, 100),
            at: Point::new(x + 14, 100),
            tag: None,
        };
        assert_eq!(frame(&selector, &config, &scene(100)).dots, [dot(100)]);
        assert_eq!(frame(&selector, &config, &scene(500)).dots, [dot(500)]);
    }

    #[test]
    fn dots_thin_out_as_letters_are_typed() {
        // Two letters, three targets: labels a, ba, bb.
        let config = Config::parse(
            "[keys]\nalphabet = \"ab\"\nleft = \"\"",
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let target = |x| Target {
            object: Some(crate::desktop::tree::ObjectId {
                bus: ":1.7".into(),
                path: format!("/{x}"),
            }),
            window: 0,
            rect: Rect::new(x - 20, 90, 40, 20),
            visible: Rect::new(x - 20, 90, 40, 20),
            hit: Point::new(x, 100),
            role: "button".into(),
            name: String::new(),
            editable: false,
            clips: Vec::new(),
            opens: Vec::new(),
        };
        let targets = [target(100), target(200), target(300)];
        let scene = Scene {
            targets: &targets,
            screen: Rect::new(0, 0, 1920, 1080),
            pointer: None,
        };
        let mut selector = Selector::new(3, 2, Action::Left);
        let froms = |selector: &Selector| -> Vec<i32> {
            frame(selector, &config, &scene)
                .dots
                .iter()
                .map(|d| d.from.x)
                .collect()
        };
        selector.press(Input::Nudge(Point::new(0, 14)));
        // Lines start at the tags, in the boxes' bottom-right corners.
        assert_eq!(froms(&selector), [120, 220, 320], "nudged before typing");
        selector.press(Input::Letter(1, Mods::NONE));
        assert_eq!(froms(&selector), [220, 320], "b leaves ba and bb");
        selector.press(Input::Nudge(Point::new(14, 0)));
        let dots = frame(&selector, &config, &scene).dots;
        assert_eq!(
            dots.iter().map(|d| d.tag).collect::<Vec<_>>(),
            [Some(0), Some(1)],
            "the remaining dots stay attached to their filtered tags"
        );
        assert_eq!(
            dots.iter().map(|d| d.at).collect::<Vec<_>>(),
            [Point::new(214, 114), Point::new(314, 114)],
            "nudged after a letter: both arrows count, on the open tags only"
        );
    }
}
