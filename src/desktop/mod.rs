//! Everything about the world outside wayhint: the compositor and the
//! accessibility tree, behind a few calls on [`Desktop`] (scan the windows
//! a monitor shows, re-check a target) plus a few plain Hyprland queries
//! (which windows those are, and how they stack). The rest of the
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
use futures::future;

use self::{atspi::Bounds, hyprland::uncovered};
pub use self::{
    atspi::Stats,
    hyprland::{
        Monitor, Window, cursor, focused_window, monitors, notify, stack, window_by_address,
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

    /// Scans every window in `stack` (bottom to top, from [`stack`]) at
    /// once and keeps what deserves a tag: on `monitor`, and not under a
    /// window stacked above its own. A window that can't be read (not on
    /// the accessibility bus, or no frame clearly its own) gets no tags;
    /// why comes back next to the snapshot.
    pub fn scan(&mut self, stack: &[Window], monitor: &Monitor) -> (Snapshot, Vec<anyhow::Error>) {
        // `join_all` runs the scans side by side on the one thread: each
        // waits on its own app, so three windows take as long as the
        // slowest, not the sum.
        let reads = stack.iter().map(|w| self.scanner.scan(w, self.bounds));
        let reads = self.runtime.block_on(future::join_all(reads));
        let mut trees = Vec::new();
        let mut failed = Vec::new();
        for read in reads {
            match read {
                Ok(tree) => trees.push(tree),
                Err(e) => failed.push(e),
            }
        }
        let filtered: Vec<_> = trees.iter().map(|(tree, _)| filter::filter(tree)).collect();
        self.scans += 1;
        let scans = trees.iter().zip(&filtered).map(|((t, s), f)| (t, f, s));
        let mut snapshot = Snapshot::build(self.scans, monitor.id, scans);
        // Two fields of `snapshot`, borrowed apart: the closure reads
        // `windows` while `retain` changes `targets`.
        snapshot.targets.retain(|t| {
            let address = &snapshot.windows[t.window].window.address;
            monitor.rect.contains(t.hit) && uncovered(stack, address, t.hit).is_some()
        });
        (snapshot, failed)
    }

    /// Asks a target's object again, right before clicking it: still there,
    /// still the same thing, same place, still in view, and nothing over it.
    pub fn recheck(&self, snapshot: &Snapshot, target: &Target) -> Result<Verdict> {
        // Where the window is now and how big, unless it's gone or another
        // window came over the spot (a dialog opened).
        let address = &snapshot.windows[target.window].window.address;
        let window = uncovered(&stack(snapshot.monitor)?, address, target.hit);
        let read = self
            .scanner
            .read(&target.object, &target.clips, RECHECK_TIMEOUT);
        let fresh = self.runtime.block_on(read).ok();
        Ok(recheck::verdict(target, fresh.as_ref(), window))
    }
}
