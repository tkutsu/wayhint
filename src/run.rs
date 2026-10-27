//! The first release: one straight loop, in one thread.
//!
//! ```text
//!                  +-- refresh --+
//!                  v             |
//! windows shown -> scan -> tags -> keys -> re-check -> act -> settle --+
//!      ^                         |                                     |
//!      +---- another workspace --+                                     |
//!      +---------------------------------------------------------------+
//! Close key, Ctrl+C, --once or a click that may take typing -> clean up -> exit
//! ```
//!
//! Every window the focused monitor shows gets tags, not just the focused
//! one, except where another window covers them. A menu an app opens
//! during the run counts as a window right above its own. While the tags
//! are up, the windows are scanned again every `[scan] refresh`, so a page that
//! loaded or a window that opened gets tags without a key, and a target
//! found again keeps its tag. While nothing changes, the scans come further
//! apart, up to 2 s, and they reuse the round's first scan's pictures of
//! the windows and what each object is. When the monitor switches to another
//! workspace (a Super bind still reaches Hyprland while wayhint holds the
//! keys), the tags follow it at once. A window with no accessibility tree
//! gets tags from its pixels; a block of text there gets one tag, which
//! opens into tags on its lines.
//!
//! The keys pick a target and a verb (`[verb] tag`, the verb anywhere
//! before the tag's last letter), or a verb and the spot under the
//! pointer (`[verb] Space`), which skips the re-check, or a verb typed
//! twice, done again where the last command was.
//! Ctrl or Alt held on the last key are held through the click; Shift
//! never is, since it types the capitals that are verbs.
//! A hold presses a button and keeps it down through the next round: the
//! next command, whatever its verb, lets go where it points.
//! The arrows move the pointer itself: after a hold they drag, and
//! `Space` or a verb typed twice acts where they took it. In text mode the
//! pointer goes to the current match first, and Enter clicks where the
//! arrows took it from there.
//! The repeat key does the last command on a target again: the target is
//! found in the new scan by its accessibility object, not by its tag or
//! where it was. A pixel target has none, so it can't be repeated.
//!
//! A run is in one mode: text mode, unless `--hints` asks for tags alone.
//! In text mode, every round, the tags of targets whose text can be typed
//! go, and typing searches the windows' text instead (`search.rs`); the
//! other tags stay, typed as before. `Enter` acts on the current match as on a tag, after its text
//! is read again and found unchanged, and the close key ends the run. The
//! compositor's shortcuts switch modes: launching one stops a run in the
//! other (`toggle.rs`).
//!
//! Each part was built to run alone first (`dump`, `overlay --from`,
//! `click --at`), so a failure here is in the wiring, not in a part.

use std::{
    collections::HashSet,
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, sleep},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    action::{Act, Action, Mods},
    config::{After, Config},
    desktop::{self, Desktop, Monitor, Stack, Unread, Verdict, tree::ObjectId},
    geometry::{Point, Rect},
    keyboard::Keyboard,
    overlay::{Frame, Overlay},
    pick::{self, Pick, Start, moved},
    pointer::Pointer,
    search::Chosen,
    snapshot::{Snapshot, Target},
    status::Status,
    text::{Span, Text},
};

/// How long a scan goes on before the input box says `scanning`: a quick
/// one would only blink it.
const SLOW: Duration = Duration::from_millis(500);

/// Everything one run holds on to. Owning it all in one struct makes the
/// cleanup one function, whichever way the run ends.
struct Session {
    conn: Connection,
    overlay: Overlay,
    desktop: Desktop,
    pointer: Pointer,
    /// Made at the first modifier click: most runs never need one.
    keyboard: Option<Keyboard>,
    /// Every monitor's box, for where the arrows stop (`pick::moved`).
    screens: Vec<Rect>,
    /// The last command on a target, for the repeat key.
    last: Option<Command>,
    /// Where the last command was done, or wayhint moved the pointer
    /// since, for a verb typed twice.
    spot: Option<Point>,
    /// Where the pointer was before wayhint moved it, until the next
    /// command: where `[pointer] after = "restore"` puts it back.
    home: Option<Point>,
    /// The button a hold keeps down, until the next command lets go.
    held: Option<Held>,
    /// What the scans have warned about so far: each rescan finds the same
    /// unreadable window again, and once a run is enough to say so.
    warned: HashSet<String>,
}

/// A command on a target, as the repeat key does it again.
/// The target is kept as its accessibility object, which stays the same
/// across scans for as long as the control exists; its tag and position
/// may not. `None` for a target found in pixels.
#[derive(Debug, Clone)]
struct Command {
    object: Option<ObjectId>,
    act: Act,
}

/// A hold's button, down at `at`, and where the cursor goes back to once
/// it's let go, if `[pointer] after` puts it back.
#[derive(Debug, Clone, Copy)]
struct Held {
    at: Point,
    back: Option<Point>,
}

/// How a pick ended for the loop.
enum Done {
    /// Acted. `ends` when the run is over: a text field may have taken
    /// focus.
    Acted { ends: bool },
    /// Didn't act: the target changed, or was gone. Scan again.
    Refused,
}

/// The run: `start` says how its picks start (`--verb` the first, text
/// mode or `--hints` every one); static mode never scans again.
pub fn run(
    mut config: Config,
    once: bool,
    static_mode: bool,
    start: Start,
    toggle: std::os::unix::net::UnixDatagram,
) -> Result<()> {
    if static_mode {
        config.refresh = None;
    }
    let monitors = desktop::monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| m.focused)
        .context("no monitor has focus")?;
    let outputs = Rect::bounding(monitors.iter().map(|m| m.rect)).context("no monitors")?;

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    // The overlay comes first: it blocks SIGINT and SIGTERM for this thread
    // (they become events), and the threads started after it inherit that.
    let mut overlay = Overlay::new(
        &conn,
        &config.style,
        &config.marks,
        &config.input,
        &config.info,
        monitor,
    )?;
    overlay.listen_for_toggle(toggle, &config.verbs, start.text)?;
    overlay.start_feed();
    if let Err(e) = overlay.follow_workspaces() {
        desktop::warn(&format!(
            "wayhint: the tags won't follow workspace changes: {e:#}"
        ));
    }
    let mut desktop = Desktop::connect()?;
    if let Err(e) = desktop.watch_menus() {
        desktop::log(&format!("wayhint: menus won't get tags: {e:#}"));
    }
    let mut session = Session {
        overlay,
        desktop,
        pointer: Pointer::new(&conn, outputs, config.scroll)?,
        keyboard: None,
        screens: monitors.iter().map(|m| m.rect).collect(),
        last: None,
        spot: None,
        home: None,
        held: None,
        warned: HashSet::new(),
        conn,
    };

    let result = session.rounds(&config, once || static_mode, static_mode, start);
    session.clean_up();
    result
}

impl Session {
    /// One round per action, until something ends the run.
    fn rounds(
        &mut self,
        config: &Config,
        once: bool,
        static_mode: bool,
        mut start: Start,
    ) -> Result<()> {
        loop {
            // No tags while scanning: nothing stale can be typed.
            self.overlay.clear()?;
            // A copy: the overlay is lent to the pick while blocks open.
            let monitor = &self.overlay.monitor().clone();
            let overlay = &mut self.overlay;
            let mut slow = || say_scanning(overlay, config);
            let mut scans = Scans::new(
                &mut self.desktop,
                &mut self.warned,
                monitor,
                config,
                &mut slow,
            )?;

            // `take` hands `--verb` to the first pick only, so later
            // rounds start with the default action again; the mode is
            // every round's. `snapshot` is what the pick ends
            // in: the last scan, or a block opened in it.
            let mut snapshot = scans.latest.clone();
            let first = Start {
                verb: start.verb.take(),
                text: start.text,
                holding: self.held.is_some(),
            };
            let picked = pick::pick(
                &mut self.overlay,
                config,
                &mut snapshot,
                first,
                &mut Hand::new(
                    &mut self.pointer,
                    &self.screens,
                    &mut self.spot,
                    &mut self.home,
                ),
                &mut scans,
            )?;
            // Moved out, so `scans` lets go of the desktop for the click.
            let scanned = scans.latest;
            let done = match picked {
                Pick::Leave => return self.go_home(config),
                // Frozen hints belong only to the workspace first scanned.
                Pick::Rescan if static_mode => return Ok(()),
                // Nothing was done, so `--once` goes on too, and the letters
                // typed so far go with the old tags.
                Pick::Rescan => continue,
                // `Space`: the cursor is already there, and there's no
                // target to re-check, or to repeat on. Nor any telling
                // what's under it, so a click there ends the run in case it
                // was a text field (see `command`).
                Pick::Here { act } => {
                    self.last = None;
                    self.act(act.action, act.mods, desktop::cursor()?, false)?;
                    Done::Acted {
                        ends: act.action.clicks(),
                    }
                }
                // A verb typed twice: where the last command was, or at
                // the pointer before the first. As unknown as `Space`'s spot,
                // so a click there ends the run too.
                Pick::Again { act } => {
                    self.last = None;
                    let at = self.spot.map_or_else(desktop::cursor, Ok)?;
                    let restore = config.after == After::Restore;
                    self.act(act.action, act.mods, at, restore)?;
                    Done::Acted {
                        ends: act.action.clicks(),
                    }
                }
                Pick::Release => {
                    self.release()?;
                    Done::Acted { ends: false }
                }
                Pick::Target { index, act } => {
                    let target = &snapshot.targets[index];
                    let object = target.object.clone();
                    self.command(config, &snapshot, target, Command { object, act }, None)?
                }
                Pick::Text(chosen) => self.text(config, &snapshot, *chosen)?,
                Pick::Repeat => {
                    // `clone`: `self.command` needs all of `self`, so
                    // `self.last` can't stay borrowed through the call.
                    // The search is in the whole scan, even from inside an
                    // opened block.
                    let last = self.last.clone();
                    let found = last.as_ref().and_then(|last| {
                        let object = last.object.as_ref()?;
                        let mut targets = scanned.targets.iter();
                        targets.position(|t| t.object.as_ref() == Some(object))
                    });
                    match (last, found) {
                        (Some(last), Some(index)) => {
                            self.command(config, &scanned, &scanned.targets[index], last, None)?
                        }
                        (Some(last), None) if last.object.is_none() => {
                            desktop::log(
                                "wayhint: not repeating: the target was found in pixels, which can't be found again",
                            );
                            Done::Refused
                        }
                        (Some(_), None) => {
                            desktop::log("wayhint: not repeating: the target is gone");
                            Done::Refused
                        }
                        (None, _) => {
                            desktop::log("wayhint: nothing to repeat yet");
                            Done::Refused
                        }
                    }
                }
            };
            match done {
                // A held button waits for the next command to let go, so
                // the run goes on, even with `--once` or after a field.
                _ if self.held.is_some() => {}
                // Loop around: a fresh scan, fresh tags. A refusal never let
                // go of the keyboard.
                Done::Refused if !once => continue,
                Done::Acted { ends: false } if !once => {}
                _ => return Ok(()),
            }
            // Takes the keyboard back if `act` let go of it; a no-op with
            // the grab still held.
            self.overlay.grab_keyboard()?;
            // A fixed pause for the app to react to the click: what it
            // changes after that, the next refresh picks up.
            sleep(config.settle);
        }
    }

    /// Does a match chosen in text mode, if its text still reads the same
    /// (in kitty, in the same cells) and its object passes a target's
    /// re-check (in kitty, its window): on it, or where the pointer is if
    /// the arrows moved it off. Not kept for the repeat key: that finds
    /// targets by their object, and a text's object is rarely one.
    fn text(&mut self, config: &Config, snapshot: &Snapshot, chosen: Chosen) -> Result<Done> {
        let Chosen {
            target,
            act,
            aimed,
            check,
        } = chosen;
        if let Some(check) = &check
            && let Err(e) = self.desktop.still_reads(snapshot, &target, check)
        {
            desktop::log(&format!("wayhint: refreshing {:?}: {e:#}", check.text));
            return Ok(Done::Refused);
        }
        let at = aimed.then(desktop::cursor).transpose()?;
        let command = Command { object: None, act };
        let done = self.command(config, snapshot, &target, command, at);
        self.last = None;
        done
    }

    /// Re-checks `target` and, if it's still the same, does `command` at
    /// `at`, where the arrows aimed, or else at its hit point, and keeps it
    /// for the repeat key.
    ///
    /// The re-check is of the target, not of where the arrows aimed: that
    /// spot is anywhere you chose, and has nothing to check it against.
    fn command(
        &mut self,
        config: &Config,
        snapshot: &Snapshot,
        target: &Target,
        command: Command,
        at: Option<Point>,
    ) -> Result<Done> {
        match self.desktop.recheck(snapshot, target)? {
            Verdict::Click { editable } => {
                let restore = config.after == After::Restore;
                let Act { action, mods } = command.act;
                let at = at.unwrap_or(target.hit);
                self.act(action, mods, at, restore)?;
                // A click on a text field ends the mode, so typing goes to
                // it. So does one aimed out of the target's box: wayhint
                // doesn't know what's there, and a field next door would
                // take the focus while the tags took the typing. And one on
                // a target found in pixels, which could be a field for all
                // wayhint knows. Staying on is for clicks it can vouch for.
                // A hover only looks.
                let vouched = target.object.is_some() && target.rect.contains(at);
                let ends = action.clicks() && (editable || !vouched);
                self.last = Some(command);
                Ok(Done::Acted { ends })
            }
            Verdict::Refuse(why) => {
                // A changing screen is routine: the next round refreshes
                // the tags, without a desktop notification for the race.
                desktop::log(&format!("wayhint: refreshing {:?}: {why}", target.name));
                Ok(Done::Refused)
            }
        }
    }

    /// Does `action` at `at` with `mods` held, and puts the cursor back if
    /// `restore` (never after a hover: it's there to stay, and holds
    /// nothing).
    ///
    /// The input-method grab stays on through a plain click: spike test 7
    /// (2026-10-23) clicked a field and a menu item through it. Keys typed
    /// while the app reacts and wayhint rescans then come to wayhint, which
    /// drops them as stale, instead of reaching the app, where a fast next
    /// tag would be typed into the page. A modifier click lets go first, as
    /// spike test 9 did it, and so does the focus fallback, the path tests
    /// 2 and 3 proved; `rounds` takes the keyboard back.
    ///
    /// While a hold keeps a button down, any command lets go of it at `at`
    /// instead, whatever its verb: a drag there, from where the cursor is,
    /// on the hold or where the arrows took it. A hold itself keeps its
    /// button down and the cursor on it, and puts the cursor back only once
    /// it's let go.
    fn act(&mut self, action: Action, mods: Mods, at: Point, restore: bool) -> Result<()> {
        self.spot = Some(at);
        let home = self.home.take();
        if let Some(held) = self.held.take() {
            self.overlay.clear()?;
            let from = desktop::cursor().unwrap_or(held.at);
            self.pointer.drag(from, at)?;
            return match held.back {
                Some(back) => self.pointer.put_back(back),
                None => Ok(()),
            };
        }
        let mods = if action.takes_mods() {
            mods
        } else {
            Mods::NONE
        };
        self.overlay.clear()?;
        if !mods.is_empty() || !self.overlay.grabs_without_focus() {
            self.overlay.release_keyboard()?;
        }
        // Back where it was before wayhint moved it, if it did.
        let back = if restore && action.restores() {
            home.or_else(|| desktop::cursor().ok())
        } else {
            None
        };
        // Without a virtual keyboard, or a keymap to give it, the click
        // goes ahead without the modifiers: a plain click beats none.
        if !mods.is_empty()
            && let Err(e) = self.hold(mods)
        {
            desktop::warn(&format!("wayhint: clicking without the modifiers: {e:#}"));
        }
        let clicked = self.pointer.act(action, at);
        // Modifiers go before the cursor moves back: a Ctrl still held
        // there would be a Ctrl-hover.
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.release_all();
        }
        clicked?;
        if action == Action::Hold {
            self.held = Some(Held { at, back });
            return Ok(());
        }
        match back {
            Some(back) => self.pointer.put_back(back),
            None => Ok(()),
        }
    }

    /// Puts the pointer back where it was before wayhint moved it, as
    /// `[pointer] after = "restore"` does after a click: leaving undoes the
    /// arrows and the jumps to matches. Not while a hold keeps a button
    /// down: that would drag.
    fn go_home(&mut self, config: &Config) -> Result<()> {
        match self.home.take() {
            Some(home) if config.after == After::Restore && self.held.is_none() => {
                self.pointer.go(home)
            }
            _ => Ok(()),
        }
    }

    /// Lets go of a hold's button where the cursor is, `Esc` while it's
    /// down: a long press, or the end of a drag the arrows made.
    fn release(&mut self) -> Result<()> {
        match self.held {
            // With a button held, `act` lets go: here, in place.
            Some(held) => {
                let at = desktop::cursor().unwrap_or(held.at);
                self.act(Action::Left, Mods::NONE, at, false)
            }
            None => Ok(()),
        }
    }

    /// Holds `mods` on the virtual keyboard, making it the first time,
    /// with the keymap the real keyboard sent.
    fn hold(&mut self, mods: Mods) -> Result<()> {
        let keyboard = match &mut self.keyboard {
            Some(keyboard) => keyboard,
            // `insert` puts the new keyboard in the empty slot and hands
            // back a reference to it, so there's no unwrap after.
            empty => {
                let keymap = self
                    .overlay
                    .keymap()
                    .context("the keyboard sent no keymap yet")?;
                empty.insert(Keyboard::new(&self.conn, keymap)?)
            }
        };
        keyboard.hold(mods, self.overlay.locks())
    }

    /// The one cleanup path: nothing pressed, keyboard handed back. `Drop`
    /// on each part does the same as a backstop, so a panic is covered too.
    fn clean_up(&mut self) {
        self.pointer.release_all();
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.release_all();
        }
        let _ = self.overlay.clear();
        let _ = self.overlay.release_keyboard();
    }
}

/// A round's scans of the monitor, the first and each refresh: what a pick
/// asks for fresh targets. It keeps the latest whole scan, for the repeat
/// key, and the windows it was made from, for an opened block's lines.
struct Scans<'a> {
    desktop: &'a mut Desktop,
    warned: &'a mut HashSet<String>,
    monitor: &'a Monitor,
    config: &'a Config,
    stack: Stack,
    latest: Snapshot,
}

impl<'a> Scans<'a> {
    /// Scans the windows `monitor` shows, calling `slow` if that takes a
    /// while. The windows' pictures and objects are read afresh, and the
    /// refreshes reuse them.
    fn new(
        desktop: &'a mut Desktop,
        warned: &'a mut HashSet<String>,
        monitor: &'a Monitor,
        config: &'a Config,
        slow: &mut dyn FnMut(),
    ) -> Result<Self> {
        desktop.new_round();
        let (stack, latest) = scan(desktop, warned, monitor, config, slow)?;
        Ok(Self {
            desktop,
            warned,
            monitor,
            config,
            stack,
            latest,
        })
    }
}

impl pick::Source for Scans<'_> {
    fn rescan(&mut self, slow: &mut dyn FnMut()) -> Result<Option<Snapshot>> {
        (self.stack, self.latest) =
            scan(self.desktop, self.warned, self.monitor, self.config, slow)?;
        Ok(Some(self.latest.clone()))
    }

    fn keep(&self, opened: &mut Snapshot) {
        desktop::keep_visible(opened, &self.stack, self.monitor);
    }

    fn read_text(&mut self, shown: &Snapshot) -> Vec<Text> {
        let reading = self.desktop.read_text(shown);
        if let Some(why) = reading.incomplete {
            desktop::log(&format!(
                "wayhint: reading the text stopped early ({why}); some may be missing"
            ));
        }
        let messages = reading
            .unread
            .into_iter()
            .map(|e| (e.app, format!("no text from {:#}", e.error)));
        for message in group_warnings(messages, self.warned) {
            desktop::log(&message);
        }
        reading.texts
    }

    fn locate(&mut self, shown: &Snapshot, texts: &[Text], spans: &[Span]) -> Vec<Vec<Rect>> {
        self.desktop.locate(shown, texts, spans)
    }

    fn shows(&self, shown: &Snapshot, window: usize, at: Point) -> bool {
        let address = &shown.windows[window].window.address;
        desktop::shows(&self.stack, self.monitor, address, at)
    }
}

/// The pointer for a pick, moved by the virtual pointer. Where it goes is
/// the spot a verb typed twice acts on, and where it was before the first
/// move is `home`.
struct Hand<'a> {
    pointer: &'a mut Pointer,
    screens: &'a [Rect],
    spot: &'a mut Option<Point>,
    home: &'a mut Option<Point>,
}

impl<'a> Hand<'a> {
    const fn new(
        pointer: &'a mut Pointer,
        screens: &'a [Rect],
        spot: &'a mut Option<Point>,
        home: &'a mut Option<Point>,
    ) -> Self {
        Self {
            pointer,
            screens,
            spot,
            home,
        }
    }
}

impl pick::Mouse for Hand<'_> {
    fn move_by(&mut self, by: Point) -> Result<()> {
        let at = moved(desktop::cursor()?, by, self.screens);
        self.move_to(at)
    }

    fn move_to(&mut self, at: Point) -> Result<()> {
        if self.home.is_none() {
            *self.home = desktop::cursor().ok();
        }
        self.pointer.go(at)?;
        *self.spot = Some(at);
        Ok(())
    }
}

/// One scan of the windows `monitor` shows, and what they are, bottom to
/// top, with `slow` called once it has taken `SLOW`. A scan slower than
/// `[scan] refresh` is logged once a run: the tags then lag behind the
/// screen by more than the setting says.
fn scan(
    desktop: &mut Desktop,
    warned: &mut HashSet<String>,
    monitor: &Monitor,
    config: &Config,
    slow: &mut dyn FnMut(),
) -> Result<(Stack, Snapshot)> {
    let started = Instant::now();
    let (stack, (scanned, unread)) = noticed(
        || {
            let stack = desktop.stack(monitor.id)?;
            let scan = desktop.scan(
                &stack,
                monitor,
                config.pixels,
                config.terminal_hint,
                &config.terminal_processes,
            );
            anyhow::Ok((stack, scan))
        },
        slow,
    )?;
    for message in report(&scanned, &unread, warned) {
        desktop::log(&message);
    }
    let took = started.elapsed();
    if let Some(every) = config.refresh
        && took > every
        && warned.insert("slow scan".into())
    {
        desktop::log(&format!(
            "wayhint: a scan took {} ms, more than [scan] refresh ({} ms)",
            took.as_millis(),
            every.as_millis()
        ));
    }
    Ok((stack, scanned))
}

/// Shows `scanning` alone, in the input box, if `[info] scanning` says to:
/// a round's first scan is slow, with no tags up yet.
fn say_scanning(overlay: &mut Overlay, config: &Config) {
    if config.info.scanning {
        let busy = Frame {
            status: Status {
                info: Some(pick::scanning()),
                ..Status::default()
            },
            ..Frame::default()
        };
        let _ = overlay.update(&busy);
    }
}

/// Does `work` on a thread of its own and hands back what it made, calling
/// `slow` if it's still at it after `SLOW`: the thread that draws is free
/// to say so meanwhile. A panic in `work` goes on here.
fn noticed<T: Send>(work: impl FnOnce() -> T + Send, slow: &mut dyn FnMut()) -> T {
    thread::scope(|scope| {
        let (done, finished) = mpsc::channel();
        let worker = scope.spawn(move || {
            let made = work();
            let _ = done.send(());
            made
        });
        if finished.recv_timeout(SLOW) == Err(RecvTimeoutError::Timeout) {
            slow();
        }
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Says what a scan missed: windows it couldn't read (`unread`) and scans
/// cut short, each once a run (`warned` holds what was said), grouped by
/// app. An empty scan is valid: keep listening for fresh targets or the
/// toggle shortcut.
/// Pixel tags are logged once a run too: they're how a
/// window with no tree is meant to work.
fn report(snapshot: &Snapshot, unread: &[Unread], warned: &mut HashSet<String>) -> Vec<String> {
    let incomplete = snapshot.windows.iter().filter_map(|scanned| {
        let why = scanned.stats.incomplete.as_ref()?;
        let class = &scanned.window.class;
        Some((
            Some(class.clone()),
            format!("scan of {class} incomplete ({why}); some targets may be missing"),
        ))
    });
    let unread = unread
        .iter()
        .map(|e| (e.app.clone(), format!("no tags in {:#}", e.error)));
    let messages = group_warnings(unread.chain(incomplete), warned);
    for scanned in snapshot.windows.iter().filter(|s| s.pixels) {
        let class = &scanned.window.class;
        if warned.insert(format!("pixels {class}")) {
            desktop::log(&format!(
                "wayhint: tags in {class} come from its pixels ({:.0} ms)",
                scanned.stats.ms
            ));
        }
    }
    messages
}

/// Keeps repeated diagnostics quiet for this run and groups new ones by app.
fn group_warnings(
    messages: impl IntoIterator<Item = (Option<String>, String)>,
    warned: &mut HashSet<String>,
) -> Vec<String> {
    let mut groups: Vec<(Option<String>, String)> = Vec::new();
    for (app, message) in messages {
        if !warned.insert(format!("wayhint: {message}")) {
            continue;
        }
        let group = groups
            .iter_mut()
            .find(|(key, _)| app.is_some() && *key == app);
        if let Some((_, body)) = group {
            body.push('\n');
            body.push_str(&message);
        } else {
            groups.push((app, format!("wayhint: {message}")));
        }
    }
    groups.into_iter().map(|(_, body)| body).collect()
}

/// `wayhint click --at X,Y`: the pointer alone, one action. A hold lets go
/// as the command ends, and a scroll turns the default notches.
pub fn click_at(at: Point, action: Action) -> Result<()> {
    let monitors = desktop::monitors()?;
    let outputs = Rect::bounding(monitors.iter().map(|m| m.rect)).context("no monitors")?;
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let mut pointer = Pointer::new(&conn, outputs, crate::config::SCROLL)?;
    let back = desktop::cursor().ok().filter(|_| action.restores());
    let result = pointer.act(action, at).and_then(|()| match back {
        Some(back) => pointer.put_back(back),
        None => Ok(()),
    });
    pointer.release_all();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_workspace_is_a_valid_refresh() {
        let snapshot = Snapshot {
            scan: 1,
            monitor: 0,
            windows: Vec::new(),
            targets: Vec::new(),
            obstacles: Vec::new(),
            texts: Vec::new(),
        };
        let mut warned = HashSet::new();
        assert_eq!(report(&snapshot, &[], &mut warned), Vec::<String>::new());
        assert!(warned.is_empty());
    }

    #[test]
    fn warnings_from_several_windows_of_one_app_share_a_log_entry() {
        let warnings = || {
            [
                (
                    Some("kitty".into()),
                    "no text from kitty: pid 1 has no socket".into(),
                ),
                (
                    Some("firefox".into()),
                    "no tags in firefox: timed out".into(),
                ),
                (
                    Some("kitty".into()),
                    "no text from kitty: pid 2 has no socket".into(),
                ),
                (
                    Some("kitty".into()),
                    "no text from kitty: pid 1 has no socket".into(),
                ),
            ]
        };
        let mut warned = HashSet::new();
        let messages = group_warnings(warnings(), &mut warned);
        assert_eq!(
            messages,
            [
                "wayhint: no text from kitty: pid 1 has no socket\nno text from kitty: pid 2 has no socket",
                "wayhint: no tags in firefox: timed out",
            ]
        );
        assert_eq!(
            group_warnings(warnings(), &mut warned),
            Vec::<String>::new()
        );
        assert_eq!(
            group_warnings(
                [
                    (
                        Some("kitty".into()),
                        "no text from kitty: pid 1 has no socket".into()
                    ),
                    (
                        Some("kitty".into()),
                        "no text from kitty: pid 3 has no socket".into()
                    ),
                ],
                &mut warned
            ),
            ["wayhint: no text from kitty: pid 3 has no socket"]
        );
    }

    #[test]
    fn scan_failures_and_incomplete_reads_share_an_app_log_entry() {
        let snapshot = Snapshot {
            scan: 1,
            monitor: 0,
            windows: vec![crate::snapshot::Scanned {
                window: desktop::Window {
                    address: "0x1".into(),
                    pid: 1,
                    class: "quickshell:bar".into(),
                    title: String::new(),
                    rect: Rect::new(0, 0, 100, 40),
                    monitor: 0,
                    focused: false,
                    kind: desktop::Kind::Layer { level: 2 },
                    frames: Vec::new(),
                },
                stats: desktop::Stats {
                    incomplete: Some("node budget".into()),
                    ..desktop::Stats::default()
                },
                pixels: false,
            }],
            targets: Vec::new(),
            obstacles: Vec::new(),
            texts: Vec::new(),
        };
        let unread = [Unread {
            app: Some("quickshell:bar".into()),
            error: anyhow::anyhow!("quickshell:bar: frames didn't answer"),
        }];
        let mut warned = HashSet::new();
        assert_eq!(
            report(&snapshot, &unread, &mut warned),
            [
                "wayhint: no tags in quickshell:bar: frames didn't answer\nscan of quickshell:bar incomplete (node budget); some targets may be missing",
            ]
        );
        assert_eq!(
            report(&snapshot, &unread, &mut warned),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unrelated_warnings_without_a_known_app_stay_separate() {
        assert_eq!(
            group_warnings(
                [
                    (None, "pid 1: unplaced popup".into()),
                    (Some("kitty".into()), "no text from kitty: no socket".into()),
                    (None, "pid 2: frames didn't answer".into()),
                ],
                &mut HashSet::new()
            ),
            [
                "wayhint: pid 1: unplaced popup",
                "wayhint: no text from kitty: no socket",
                "wayhint: pid 2: frames didn't answer",
            ]
        );
    }
}
