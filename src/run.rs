//! The first release: one straight loop, in one thread.
//!
//! ```text
//! windows shown -> scan -> tags -> keys -> re-check -> act -> settle --+
//!      ^                                                               |
//!      +---------------------------------------------------------------+
//! Esc, Ctrl+C, --once or a click that may take typing -> clean up -> exit
//! ```
//!
//! Every window the focused monitor shows gets tags, not just the focused
//! one, except where another window covers them. A window with no
//! accessibility tree gets tags from its pixels; a block of text there
//! gets one tag, which opens into tags on its lines.
//!
//! The keys pick a target and a verb (`[verb] tag`), or a verb and the
//! spot under the pointer (`[verb] Space`), which skips the re-check.
//! Shift, Ctrl or Alt held on the last key are held through the click.
//! Arrows typed before the last key move where that one command lands,
//! so a tag near the spot can click anywhere.
//! The repeat key does the last command on a target again: the target is
//! found in the new scan by its accessibility object, not by its tag or
//! where it was. A pixel target has none, so it can't be repeated.
//!
//! Each part was built to run alone first (`dump`, `overlay --from`,
//! `click --at`), so a failure here is in the wiring, not in a part.

use std::{thread::sleep, time::Duration};

use anyhow::{Context as _, Result, bail};
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    action::{Act, Action, Mods},
    config::{After, Config},
    desktop::{self, Desktop, Verdict, tree::ObjectId},
    geometry::{Point, Rect},
    keyboard::Keyboard,
    overlay::Overlay,
    pick::{self, Pick, lands},
    pointer::Pointer,
    snapshot::Snapshot,
};

/// How long the app gets to react to a click before the rescan. A fixed
/// pause for now; event-driven settling arrives with live refresh.
const SETTLE: Duration = Duration::from_millis(150);

/// Everything one run holds on to. Owning it all in one struct makes the
/// cleanup one function, whichever way the run ends.
struct Session {
    conn: Connection,
    overlay: Overlay,
    desktop: Desktop,
    pointer: Pointer,
    /// Made at the first modifier click: most runs never need one.
    keyboard: Option<Keyboard>,
    /// Every monitor's box, for where a nudge stops (`pick::lands`).
    screens: Vec<Rect>,
    /// The last command on a target, for the repeat key.
    last: Option<Command>,
}

/// A command on a target, as the repeat key does it again, nudge and all.
/// The target is kept as its accessibility object, which stays the same
/// across scans for as long as the control exists; its tag and position
/// may not. `None` for a target found in pixels.
#[derive(Debug, Clone)]
struct Command {
    object: Option<ObjectId>,
    act: Act,
}

/// How a pick ended for the loop.
enum Done {
    /// Acted. `ends` when the run is over: a text field may have taken
    /// focus.
    Acted { ends: bool },
    /// Didn't act: the target changed, or was gone. Scan again.
    Refused,
}

/// The run: `verb` (from `--verb`) arms the first pick.
pub fn run(config: &Config, once: bool, verb: Option<Action>) -> Result<()> {
    let monitors = desktop::monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| m.focused)
        .context("no monitor has focus")?;
    let outputs = Rect::bounding(monitors.iter().map(|m| m.rect)).context("no monitors")?;

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    // The overlay comes first: it blocks SIGINT and SIGTERM for this thread
    // (they become events), and the threads started after it inherit that.
    let overlay = Overlay::new(&conn, &config.style, monitor)?;
    let mut session = Session {
        overlay,
        desktop: Desktop::connect()?,
        pointer: Pointer::new(&conn, outputs)?,
        keyboard: None,
        screens: monitors.iter().map(|m| m.rect).collect(),
        last: None,
        conn,
    };

    let result = session.rounds(config, once, verb);
    session.clean_up();
    result
}

impl Session {
    /// One round per action, until something ends the run.
    fn rounds(&mut self, config: &Config, once: bool, mut verb: Option<Action>) -> Result<()> {
        loop {
            // No tags while scanning: nothing stale can be typed.
            self.overlay.clear()?;
            // A copy: the overlay is lent to the pick while blocks open.
            let monitor = &self.overlay.monitor().clone();
            let stack = desktop::stack(monitor.id)?;
            let (scanned, unread) = self.desktop.scan(&stack, monitor, config.pixels);
            report(&scanned, &unread)?;

            // `take` hands `--verb` to the first pick and leaves `None`, so
            // later rounds start with the default action again. `snapshot`
            // is what the pick ends in: the scan, or a block opened in it.
            let mut snapshot = scanned.clone();
            let picked = pick::pick_opening(
                &mut self.overlay,
                config,
                &mut snapshot,
                verb.take(),
                &desktop::cursor,
                &|opened| desktop::keep_visible(opened, &stack, monitor),
            )?;
            let done = match picked {
                Pick::Leave => return Ok(()),
                // `Space`: the cursor is already there, give or take the
                // nudge, and there's no target to re-check, or to repeat on.
                // Nor any telling what's under it, so a click there ends
                // the run in case it was a text field (see `command`).
                Pick::Here { act, from } => {
                    self.last = None;
                    let at = lands(from, act.nudge, &self.screens);
                    self.act(act.action, act.mods, at, false)?;
                    Done::Acted {
                        ends: act.action.clicks(),
                    }
                }
                Pick::Target { index, act } => {
                    let object = snapshot.targets[index].object.clone();
                    self.command(config, &snapshot, index, Command { object, act })?
                }
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
                        (Some(last), Some(index)) => self.command(config, &scanned, index, last)?,
                        (Some(last), None) if last.object.is_none() => {
                            eprintln!(
                                "wayhint: not repeating: the target was found in pixels, which can't be found again"
                            );
                            Done::Refused
                        }
                        (Some(_), None) => {
                            eprintln!("wayhint: not repeating: the target is gone");
                            Done::Refused
                        }
                        (None, _) => {
                            eprintln!("wayhint: nothing to repeat yet");
                            Done::Refused
                        }
                    }
                }
            };
            match done {
                // Loop around: a fresh scan, fresh tags. A refusal never let
                // go of the keyboard.
                Done::Refused if !once => continue,
                Done::Acted { ends: false } if !once => {}
                _ => return Ok(()),
            }
            // Takes the keyboard back if `act` let go of it; a no-op with
            // the grab still held.
            self.overlay.grab_keyboard()?;
            sleep(SETTLE);
        }
    }

    /// Re-checks the target at `index` and, if it's still the same, does
    /// `command` on it and keeps it for the repeat key.
    ///
    /// The re-check is of the target, not of where a nudge takes the click:
    /// that spot is anywhere you chose, and has nothing to check it against.
    fn command(
        &mut self,
        config: &Config,
        snapshot: &Snapshot,
        index: usize,
        command: Command,
    ) -> Result<Done> {
        let target = &snapshot.targets[index];
        match self.desktop.recheck(snapshot, target)? {
            Verdict::Click { editable } => {
                let restore = config.after == After::Restore;
                let Act {
                    action,
                    mods,
                    nudge,
                } = command.act;
                let at = lands(target.hit, nudge, &self.screens);
                self.act(action, mods, at, restore)?;
                // A click on a text field ends the mode, so typing goes to
                // it. So does one nudged out of the target's box: wayhint
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
                eprintln!("wayhint: not clicking {:?}: {why}", target.name);
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
    fn act(&mut self, action: Action, mods: Mods, at: Point, restore: bool) -> Result<()> {
        let mods = if action.clicks() { mods } else { Mods::NONE };
        self.overlay.clear()?;
        if !mods.is_empty() || !self.overlay.grabs_without_focus() {
            self.overlay.release_keyboard()?;
        }
        let back = if restore && action.clicks() {
            desktop::cursor().ok()
        } else {
            None
        };
        // Without a virtual keyboard, or a keymap to give it, the click
        // goes ahead without the modifiers: a plain click beats none.
        if !mods.is_empty()
            && let Err(e) = self.hold(mods)
        {
            eprintln!("wayhint: clicking without the modifiers: {e:#}");
        }
        let clicked = self.pointer.act(action, at);
        // Modifiers go before the cursor moves back: a Shift still held
        // there would be a Shift-hover.
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.release_all();
        }
        clicked?;
        match back {
            Some(back) => self.pointer.put_back(back),
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

/// Says what a scan missed: windows it couldn't read (`unread`) and scans
/// cut short. With nothing to click it's an error, not a quiet exit: from
/// a keybind, that's the only way you learn why no tags came (`main` puts
/// errors on screen), and a lone terminal says why there: it isn't on the
/// bus.
fn report(snapshot: &Snapshot, unread: &[anyhow::Error]) -> Result<()> {
    if snapshot.targets.is_empty() {
        let mut why = vec!["nothing to click on this workspace".to_string()];
        why.extend(unread.iter().map(|e| format!("{e:#}")));
        bail!("{}", why.join("; "));
    }
    for e in unread {
        eprintln!("wayhint: no tags in {e:#}");
    }
    for scanned in snapshot.windows.iter().filter(|s| s.pixels) {
        eprintln!(
            "wayhint: tags in {} come from its pixels ({:.0} ms): it has no accessibility tree",
            scanned.window.class, scanned.stats.ms
        );
    }
    for scanned in &snapshot.windows {
        if let Some(why) = &scanned.stats.incomplete {
            eprintln!(
                "wayhint: scan of {} incomplete ({why}); some targets may be missing",
                scanned.window.class
            );
        }
    }
    Ok(())
}

/// `wayhint click --at X,Y`: the pointer alone, one action.
pub fn click_at(at: Point, action: Action) -> Result<()> {
    let monitors = desktop::monitors()?;
    let outputs = Rect::bounding(monitors.iter().map(|m| m.rect)).context("no monitors")?;
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let mut pointer = Pointer::new(&conn, outputs)?;
    let back = desktop::cursor().ok().filter(|_| action.clicks());
    let result = pointer.act(action, at).and_then(|()| match back {
        Some(back) => pointer.put_back(back),
        None => Ok(()),
    });
    pointer.release_all();
    result
}
