//! Everything about the world outside wayhint: the compositor and the
//! accessibility tree, behind three calls on [`Desktop`] (scan a window,
//! re-check a target) plus a few plain Hyprland queries. The rest of the
//! program sees plain data (`Window`, `Monitor`, `Snapshot`, `Verdict`),
//! never a socket, a D-Bus proxy or a future.
//!
//! The async scan runs on a Tokio runtime that [`Desktop`] owns. Each call
//! blocks until its answer is in (`block_on`), so callers stay ordinary
//! synchronous code: the first release is one straight loop, and nothing
//! else needs to run while a scan does.

mod atspi;
pub mod filter;
mod hyprland;
pub mod recheck;
pub mod tree;

use std::time::Duration;

use anyhow::Result;

use self::atspi::Bounds;
pub use self::{
    atspi::Stats,
    hyprland::{
        Monitor, Window, cursor, focused_window, keyboard_layout, monitors, window_by_address,
    },
    recheck::Verdict,
};
use crate::snapshot::{Snapshot, Target};

/// How long the re-check may take: one object, four small requests.
const RECHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// A connection to the accessibility bus plus the runtime its calls run on.
pub struct Desktop {
    runtime: tokio::runtime::Runtime,
    scanner: atspi::Scanner,
    bounds: Bounds,
    /// Scan counter, written into each snapshot.
    scans: u32,
}

impl Desktop {
    pub fn connect() -> Result<Self> {
        // `current_thread`: no thread pool. Every request is a wait on the
        // bus, and one thread waits on eight of them as well as eight would.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let scanner = runtime.block_on(atspi::Scanner::connect())?;
        Ok(Self {
            runtime,
            scanner,
            bounds: Bounds::default(),
            scans: 0,
        })
    }

    /// Reads `window`'s tree, unfiltered: what `dump --raw` prints.
    pub fn read_tree(&self, window: &Window) -> Result<(tree::Tree, Stats)> {
        self.runtime
            .block_on(self.scanner.scan(window, self.bounds))
    }

    /// Scans `window` and keeps what deserves a tag.
    pub fn scan(&mut self, window: &Window) -> Result<Snapshot> {
        let (tree, stats) = self.read_tree(window)?;
        self.scans += 1;
        Ok(Snapshot::build(
            self.scans,
            &tree,
            &filter::filter(&tree),
            stats,
        ))
    }

    /// Asks a target's object again, right before clicking it: still there,
    /// still the same thing, same place, and its window still focused.
    pub fn recheck(&self, snapshot: &Snapshot, target: &Target) -> Result<Verdict> {
        // Where the window is now, if it still has focus; the box is
        // compared in global coordinates, so a moved window counts as moved.
        let origin = focused_window()?
            .filter(|w| w.address == snapshot.window.address)
            .map(|w| w.rect.origin());
        let fresh = self
            .runtime
            .block_on(self.scanner.read(&target.object, RECHECK_TIMEOUT))
            .ok();
        Ok(recheck::verdict(target, fresh.as_ref(), origin))
    }
}
