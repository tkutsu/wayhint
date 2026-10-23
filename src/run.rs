//! The first release: one straight loop, in one thread.
//!
//! ```text
//! focused window -> scan -> tags -> keys -> re-check -> act -> settle --+
//!      ^                                                                |
//!      +----------------------------------------------------------------+
//! Esc, Ctrl+C, --once or a click on a text field -> clean up -> exit
//! ```
//!
//! The keys pick a target and a verb (`[verb] tag`), or a verb and the
//! spot under the pointer (`[verb] Space`), which skips the re-check.
//! Shift, Ctrl or Alt held on the last key are held through the click.
//! The repeat key does the last command on a target again: the target is
//! found in the new scan by its accessibility object, not by its tag or
//! where it was.
//!
//! Each part was built to run alone first (`dump`, `overlay --from`,
//! `click --at`), so a failure here is in the wiring, not in a part.

use std::{thread::sleep, time::Duration};

use anyhow::{Context as _, Result, bail};
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    action::{Action, Mods},
    config::{After, Config},
    desktop::{self, Desktop, Monitor, Verdict, tree::ObjectId},
    geometry::{Point, Rect},
    keyboard::Keyboard,
    overlay::Overlay,
    pick::{self, Pick},
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
    /// The last command on a target, for the repeat key.
    last: Option<Command>,
}

/// A command on a target, as the repeat key does it again. The target is
/// kept as its accessibility object, which stays the same across scans for
/// as long as the control exists; its tag and position may not.
#[derive(Debug, Clone)]
struct Command {
    object: ObjectId,
    action: Action,
    mods: Mods,
}

/// How a pick ended for the loop.
enum Done {
    /// Acted. `ends` when the run is over: a text field took focus.
    Acted { ends: bool },
    /// Didn't act: the target changed, or was gone. Scan again.
    Refused,
}

/// The run: `verb` (from `--verb`) arms the first pick.
pub fn run(config: &Config, once: bool, verb: Option<Action>) -> Result<()> {
    let window = desktop::focused_window()?.context("no window has focus")?;
    let monitors = desktop::monitors()?;
    let monitor = monitor_of(&monitors, window.monitor)?;
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
            let window = desktop::focused_window()?.context("no window has focus")?;
            if window.monitor != self.overlay.monitor().id {
                bail!("focus moved to another monitor; run wayhint again there");
            }
            let snapshot = self.desktop.scan(&window)?;
            if let Some(why) = &snapshot.stats.incomplete {
                eprintln!(
                    "wayhint: scan of {} incomplete ({why}); some targets may be missing",
                    window.class
                );
            }
            // An error, not a quiet exit: from a keybind, it's the only way
            // you learn why no tags came (`main` puts errors on screen).
            if snapshot.targets.is_empty() {
                bail!("nothing to click in {} ({})", window.class, window.title);
            }

            // `take` hands `--verb` to the first pick and leaves `None`, so
            // later rounds start with the default action again.
            let picked = pick::pick(&mut self.overlay, config, &snapshot.targets, verb.take())?;
            let done = match picked {
                Pick::Leave => return Ok(()),
                // `Space`: the cursor is already there, and there's no
                // target to re-check, or to repeat on.
                Pick::Here { action, mods } => {
                    self.last = None;
                    self.act(action, mods, desktop::cursor()?, false)?;
                    Done::Acted { ends: false }
                }
                Pick::Target {
                    index,
                    action,
                    mods,
                } => {
                    let object = snapshot.targets[index].object.clone();
                    let command = Command {
                        object,
                        action,
                        mods,
                    };
                    self.command(config, &snapshot, index, command)?
                }
                Pick::Repeat => {
                    // `clone`: `self.command` needs all of `self`, so
                    // `self.last` can't stay borrowed through the call.
                    let last = self.last.clone();
                    let found = last.as_ref().and_then(|last| {
                        let mut targets = snapshot.targets.iter();
                        targets.position(|t| t.object == last.object)
                    });
                    match (last, found) {
                        (Some(last), Some(index)) => {
                            self.command(config, &snapshot, index, last)?
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
                self.act(command.action, command.mods, target.hit, restore)?;
                // A click on a text field ends the mode, so typing goes to
                // it. A hover only looks.
                let ends = editable && command.action.clicks();
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

fn monitor_of(monitors: &[Monitor], id: i64) -> Result<&Monitor> {
    monitors
        .iter()
        .find(|m| m.id == id)
        .with_context(|| format!("the focused window's monitor ({id}) is gone"))
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
