//! The first release: one straight loop, in one thread.
//!
//! ```text
//! focused window -> scan -> tags -> keys -> re-check -> click -> settle --+
//!      ^                                                                  |
//!      +------------------------------------------------------------------+
//! Esc, Ctrl+C, --once or an editable target -> clean up -> exit
//! ```
//!
//! Each part was built to run alone first (`dump`, `overlay --from`,
//! `click --at`), so a failure here is in the wiring, not in a part.

use std::{thread::sleep, time::Duration};

use anyhow::{Context as _, Result, bail};
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    config::Config,
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

pub fn run(config: &Config, once: bool) -> Result<()> {
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

    let result = session.rounds(config, once);
    session.clean_up();
    result
}

impl Session {
    /// One round per action, until something ends the run.
    fn rounds(&mut self, config: &Config, once: bool) -> Result<()> {
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

            let Pick::Target(index) =
                pick::pick(&mut self.overlay, &config.alphabet, &snapshot.targets)?
            else {
                return Ok(());
            };
            let target = &snapshot.targets[index];
            match self.desktop.recheck(&snapshot, target)? {
                Verdict::Click { editable } => {
                    self.click(target.hit)?;
                    // A text field ends the mode, so typing goes to it.
                    if editable || once {
                        return Ok(());
                    }
                    self.overlay.grab_keyboard()?;
                    sleep(SETTLE);
                }
                Verdict::Refuse(why) => {
                    eprintln!("wayhint: not clicking {:?}: {why}", target.name);
                    if once {
                        return Ok(());
                    }
                    // Loop around: a fresh scan, fresh tags.
                }
            }
        }
    }

    /// Hands the keyboard back, clicks, and puts the cursor back. The grab
    /// is released first: spike test 4c proved clicks land that way, and
    /// clicking with the grab held is still unproven (phase 1b test 1).
    fn click(&mut self, at: Point) -> Result<()> {
        self.overlay.clear()?;
        self.overlay.release_keyboard()?;
        let back = desktop::cursor().ok();
        self.pointer.click(at, back)
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

/// `wayhint click --at X,Y`: the pointer alone, one click.
pub fn click_at(at: Point) -> Result<()> {
    let monitors = desktop::monitors()?;
    let outputs = Rect::bounding(monitors.iter().map(|m| m.rect)).context("no monitors")?;
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let mut pointer = Pointer::new(&conn, outputs)?;
    let back = desktop::cursor().ok();
    let result = pointer.click(at, back);
    pointer.release_all();
    result
}
