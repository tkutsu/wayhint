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
//!
//! Each part was built to run alone first (`dump`, `overlay --from`,
//! `click --at`), so a failure here is in the wiring, not in a part.

use std::{thread::sleep, time::Duration};

use anyhow::{Context as _, Result, bail};
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    action::Action,
    config::{After, Config},
    desktop::{self, Desktop, Monitor, Verdict},
    geometry::{Point, Rect},
    overlay::Overlay,
    pick::{self, Pick},
    pointer::Pointer,
};

/// How long the app gets to react to a click before the rescan. A fixed
/// pause for now; event-driven settling arrives with live refresh.
const SETTLE: Duration = Duration::from_millis(150);

/// Everything one run holds on to. Owning it all in one struct makes the
/// cleanup one function, whichever way the run ends.
struct Session {
    overlay: Overlay,
    desktop: Desktop,
    pointer: Pointer,
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
            if snapshot.targets.is_empty() {
                eprintln!(
                    "wayhint: nothing to click in {} ({})",
                    window.class, window.title
                );
                return Ok(());
            }

            // `take` hands `--verb` to the first pick and leaves `None`, so
            // later rounds start with the default action again.
            let picked = pick::pick(&mut self.overlay, config, &snapshot.targets, verb.take())?;
            let ends = match picked {
                Pick::Leave => return Ok(()),
                // `Space`: the cursor is already there, and there's no
                // target to re-check.
                Pick::Here(action) => {
                    self.act(action, desktop::cursor()?, false)?;
                    false
                }
                Pick::Target { index, action } => {
                    let target = &snapshot.targets[index];
                    match self.desktop.recheck(&snapshot, target)? {
                        Verdict::Click { editable } => {
                            self.act(action, target.hit, config.after == After::Restore)?;
                            // A click on a text field ends the mode, so
                            // typing goes to it. A hover only looks.
                            editable && action.clicks()
                        }
                        Verdict::Refuse(why) => {
                            eprintln!("wayhint: not clicking {:?}: {why}", target.name);
                            if once {
                                return Ok(());
                            }
                            // Loop around: a fresh scan, fresh tags. The
                            // keyboard was never let go.
                            continue;
                        }
                    }
                }
            };
            if ends || once {
                return Ok(());
            }
            // Takes the keyboard back if `act` let go of it; a no-op with
            // the grab still held.
            self.overlay.grab_keyboard()?;
            sleep(SETTLE);
        }
    }

    /// Does `action` at `at`, and puts the cursor back if `restore` (never
    /// after a hover: it's there to stay).
    ///
    /// The input-method grab stays on through the click: spike test 7
    /// (2026-10-23) clicked a field and a menu item through it. Keys typed
    /// while the app reacts and wayhint rescans then come to wayhint, which
    /// drops them as stale, instead of reaching the app: a fast next tag
    /// was typed into the page. The focus fallback lets go first, the path
    /// tests 2 and 3 proved, and `rounds` takes it back.
    fn act(&mut self, action: Action, at: Point, restore: bool) -> Result<()> {
        self.overlay.clear()?;
        if !self.overlay.grabs_without_focus() {
            self.overlay.release_keyboard()?;
        }
        let back = if restore && action.clicks() {
            desktop::cursor().ok()
        } else {
            None
        };
        self.pointer.act(action, at, back)
    }

    /// The one cleanup path: nothing pressed, keyboard handed back. `Drop`
    /// on each part does the same as a backstop, so a panic is covered too.
    fn clean_up(&mut self) {
        self.pointer.release_all();
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
    let result = pointer.act(action, at, back);
    pointer.release_all();
    result
}
