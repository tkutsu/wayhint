//! Everything about the world outside wayhint: the compositor and the
//! accessibility tree, behind a few calls on [`Desktop`] (what a monitor
//! shows and how it stacks, scan it, re-check a target) plus a few plain
//! Hyprland queries. The rest of the program sees plain data (`Window`,
//! `Monitor`, `Snapshot`, `Verdict`), never a socket, a D-Bus proxy or a
//! future.
//!
//! The async scan runs on a Tokio runtime that [`Desktop`] owns. Each call
//! blocks until its answer is in (`block_on`), so callers stay ordinary
//! synchronous code: the first release is one straight loop, and nothing
//! else needs to run while a scan does.

mod atspi;
pub mod filter;
mod hyprland;
pub mod recheck;
pub mod surfaces;
pub mod tree;

use std::{collections::BTreeSet, time::Duration};

use anyhow::{Result, bail};
use futures::future;

use self::{
    atspi::Bounds,
    surfaces::{place, popup, uncovered, unknown},
};
pub use self::{
    atspi::Stats,
    hyprland::{
        Kind, Monitor, Window, cursor, focused_window, monitors, notify, window_by_address,
    },
    recheck::Verdict,
    surfaces::Stack,
};
use crate::snapshot::{Snapshot, Target};

/// How long the re-check may take: one object, four small requests.
const RECHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// How long reading the layer apps' frames may take: a few requests per
/// frame, and a shell has a handful.
const FRAMES_TIMEOUT: Duration = Duration::from_millis(300);

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

    /// What `monitor` shows, bottom to top: Hyprland's clients and layers
    /// (see `hyprland::shown`), each layer with the frames that may be its
    /// own, then the popups. Frames are read only for the apps that have a
    /// layer and no client: an app's own menus are left out, as before.
    /// An app whose popups can't be placed, or whose frames didn't answer,
    /// covers the whole monitor (`surfaces::unknown`), whichever monitor its
    /// layers are on: a popup can reach past its layer's output.
    pub fn stack(&self, monitor: i64) -> Result<Stack> {
        let shown = hyprland::shown(monitor)?;
        let pids: BTreeSet<u32> = shown
            .layers
            .iter()
            .map(|l| l.pid)
            .filter(|p| !shown.client_pids.contains(p))
            .collect();
        let frames = self
            .runtime
            .block_on(self.scanner.frames(&pids, FRAMES_TIMEOUT));
        let mut stack = shown.stack;
        for (pid, frames) in frames {
            let frames = match frames {
                Ok(frames) => frames,
                Err(e) => {
                    stack.unplaced.push(format!(
                        "pid {pid}: its frames didn't answer, so its popups may be anywhere: {e:#}"
                    ));
                    stack.windows.push(unknown(pid, monitor, shown.screen));
                    continue;
                }
            };
            let layers: Vec<Window> = shown
                .layers
                .iter()
                .filter(|l| l.pid == pid)
                .cloned()
                .collect();
            let placed = place(&layers, &frames, &shown.corners);
            for (address, frames) in placed.layers {
                if let Some(layer) = stack.windows.iter_mut().find(|w| w.address == address) {
                    layer.frames = frames;
                }
            }
            let on_screen = placed
                .popups
                .iter()
                .filter(|p| p.rect.intersect(shown.screen).is_some());
            stack
                .windows
                .extend(on_screen.map(|p| popup(p, pid, monitor)));
            if placed.unplaced > 0 {
                stack.unplaced.push(format!(
                    "pid {pid}: {} surfaces it can't place, so they may be anywhere (popups are \
                     placed only while all its layers sit in the corner of the monitor at 0,0)",
                    placed.unplaced
                ));
                stack.windows.push(unknown(pid, monitor, shown.screen));
            }
        }
        Ok(stack)
    }

    /// Scans every surface in `stack` (from [`Desktop::stack`]) at once and
    /// keeps what deserves a tag: on `monitor`, and not under a surface
    /// that may be above its own. A surface that can't be read (not on the
    /// accessibility bus, or no frame clearly its own) gets no tags; why
    /// comes back next to the snapshot, with any frames `stack` couldn't
    /// place.
    pub fn scan(&mut self, stack: &Stack, monitor: &Monitor) -> (Snapshot, Vec<anyhow::Error>) {
        // A layer with no frame only covers.
        let readable = stack
            .windows
            .iter()
            .filter(|w| w.kind == Kind::Client || !w.frames.is_empty());
        // `join_all` runs the scans side by side on the one thread: each
        // waits on its own app, so three windows take as long as the
        // slowest, not the sum.
        let reads = readable.map(|w| self.read(w));
        let reads = self.runtime.block_on(future::join_all(reads));
        let mut trees = Vec::new();
        // `anyhow!` copies the text into the error: `Error::msg` takes it
        // as it is, and an error can't hold a borrowed `&String`.
        let mut failed: Vec<_> = stack
            .unplaced
            .iter()
            .map(|u| anyhow::anyhow!("{u}"))
            .collect();
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

    /// One surface's tree: a client's from the frame with its title, a
    /// layer's or popup's from its frame. Of several frames that may be a
    /// layer's (two layers with one box, as a backdrop and a launcher
    /// over it), the one with controls, if just one has and every one was
    /// read whole: which of the two is on top can't be told, so two with
    /// controls get no tags. The tree keeps the one frame it came from, for
    /// [`Desktop::recheck`].
    async fn read(&self, window: &Window) -> Result<(tree::Tree, Stats)> {
        match window.frames.as_slice() {
            [] => self.scanner.scan(window, self.bounds).await,
            [frame] => Ok(self.scanner.scan_frame(window, frame, self.bounds).await),
            frames => {
                let reads = frames
                    .iter()
                    .map(|f| self.scanner.scan_frame(window, f, self.bounds));
                let mut reads = future::join_all(reads).await;
                // A scan cut short may have missed the controls that would
                // make two frames with them.
                if let Some(why) = reads.iter().find_map(|(_, s)| s.incomplete.as_ref()) {
                    bail!(
                        "{}: {} frames its size, and one couldn't be read whole: {why}",
                        window.class,
                        frames.len()
                    );
                }
                let with: Vec<usize> = (0..reads.len())
                    .filter(|&i| !filter::filter(&reads[i].0).kept.is_empty())
                    .collect();
                let pick = match with.as_slice() {
                    // None has controls: any of them gives no tags.
                    [] => 0,
                    &[one] => one,
                    many => bail!(
                        "{}: {} frames its size have controls, and which is on top can't be told",
                        window.class,
                        many.len()
                    ),
                };
                let mut read = reads.swap_remove(pick);
                read.0.window.frames = vec![frames[pick].clone()];
                Ok(read)
            }
        }
    }

    /// Asks a target's object again, right before clicking it: still there,
    /// still the same thing, same place, still in view, and nothing over it.
    pub fn recheck(&self, snapshot: &Snapshot, target: &Target) -> Result<Verdict> {
        // Where the surface is now and how big, unless it's gone, something
        // came over the spot (a dialog opened, a popup closed), or the frame
        // its tags came from isn't clearly its own any more (a layer that
        // reports the same box was mapped).
        let then = &snapshot.windows[target.window].window;
        let stack = self.stack(snapshot.monitor)?;
        let same_frames = stack
            .windows
            .iter()
            .find(|w| w.address == then.address)
            .is_some_and(|now| then.frames.iter().all(|f| now.frames.contains(f)));
        let window = same_frames
            .then(|| uncovered(&stack, &then.address, target.hit))
            .flatten();
        let read = self
            .scanner
            .read(&target.object, &target.clips, RECHECK_TIMEOUT);
        let fresh = self.runtime.block_on(read).ok();
        Ok(recheck::verdict(target, fresh.as_ref(), window))
    }
}
