//! Everything about the world outside wayhint: the compositor and the
//! accessibility tree, behind a few calls on [`Desktop`] (scan the windows
//! a monitor shows, re-check a target) plus a few plain Hyprland queries
//! (which windows those are, and how they stack). The rest of the
//! program sees plain data (`Window`, `Monitor`, `Snapshot`, `Verdict`),
//! never a socket, a D-Bus proxy or a future.
//!
//! A window with no accessibility tree is tagged from its pixels: a
//! picture through Hyprland's window export (`capture`), read by
//! [`crate::pixels`].
//!
//! The async scan runs on a Tokio runtime that [`Desktop`] owns. Each call
//! blocks until its answer is in (`block_on`), so callers stay ordinary
//! synchronous code: the first release is one straight loop, and nothing
//! else needs to run while a scan does.

mod atspi;
mod capture;
pub mod filter;
mod hyprland;
pub mod recheck;
pub mod tree;

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures::future;

use self::{atspi::Bounds, capture::Camera, hyprland::uncovered};
pub use self::{
    atspi::Stats,
    hyprland::{Monitor, Window, cursor, focused_window, monitors, stack, warn, window_by_address},
    recheck::Verdict,
};
use crate::{
    pixels::{self, Detector, Found},
    snapshot::{Read, Snapshot, Target},
};

/// How long the re-check may take: one object, four small requests.
const RECHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a window's picture may take, connecting included: the
/// spike's took 6 to 12 ms.
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(500);

/// A connection to the accessibility bus plus the runtime its calls run on.
pub struct Desktop {
    runtime: tokio::runtime::Runtime,
    scanner: atspi::Scanner,
    bounds: Bounds,
    /// Scan counter, written into each snapshot.
    scans: u32,
    /// Made the first time a window has no tree: most runs never need it.
    camera: Option<Camera>,
}

/// One window as the scan saw it, until the snapshot is built from it.
enum Seen {
    Tree(tree::Tree, filter::Filtered, Stats),
    Pixels(Window, Found, f64, Stats),
}

impl Seen {
    fn read(&self) -> Read<'_> {
        match self {
            Self::Tree(tree, filtered, stats) => Read::Tree(tree, filtered, stats),
            Self::Pixels(window, found, scale, stats) => Read::Pixels {
                window,
                found,
                scale: *scale,
                stats,
            },
        }
    }
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
            camera: None,
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
    /// the accessibility bus, or no frame clearly its own) is tagged from
    /// its pixels with `pixels`, or gets no tags when that's `None`; why
    /// comes back next to the snapshot.
    pub fn scan(
        &mut self,
        stack: &[Window],
        monitor: &Monitor,
        pixels: Option<Detector>,
    ) -> (Snapshot, Vec<anyhow::Error>) {
        // `join_all` runs the scans side by side on the one thread: each
        // waits on its own app, so three windows take as long as the
        // slowest, not the sum.
        let reads = stack.iter().map(|w| self.scanner.scan(w, self.bounds));
        let reads = self.runtime.block_on(future::join_all(reads));
        let mut seen = Vec::new();
        let mut failed = Vec::new();
        for (window, read) in stack.iter().zip(reads) {
            match (read, pixels) {
                (Ok((tree, stats)), _) => {
                    let filtered = filter::filter(&tree);
                    seen.push(Seen::Tree(tree, filtered, stats));
                }
                (Err(e), None) => failed.push(e),
                (Err(no_tree), Some(detector)) => match look(&mut self.camera, window, detector) {
                    Ok((found, scale, stats)) => {
                        seen.push(Seen::Pixels(window.clone(), found, scale, stats));
                    }
                    Err(e) => failed.push(anyhow!("{no_tree:#}, and {e:#}")),
                },
            }
        }
        self.scans += 1;
        let mut snapshot = Snapshot::build(self.scans, monitor.id, seen.iter().map(Seen::read));
        keep_visible(&mut snapshot, stack, monitor);
        (snapshot, failed)
    }

    /// Asks a target's object again, right before clicking it: still there,
    /// still the same thing, same place, still in view, and nothing over it.
    /// A pixel target has no object: its window must still be where it was,
    /// with nothing over the spot.
    pub fn recheck(&self, snapshot: &Snapshot, target: &Target) -> Result<Verdict> {
        // Where the window is now and how big, unless it's gone or another
        // window came over the spot (a dialog opened).
        let scanned = &snapshot.windows[target.window].window;
        let window = uncovered(&stack(snapshot.monitor)?, &scanned.address, target.hit);
        let Some(object) = &target.object else {
            return Ok(recheck::unread(scanned.rect, window));
        };
        let read = self.scanner.read(object, &target.clips, RECHECK_TIMEOUT);
        let fresh = self.runtime.block_on(read).ok();
        Ok(recheck::verdict(target, fresh.as_ref(), window))
    }
}

/// Keeps the targets worth a tag: on `monitor`, and not under a window
/// stacked above their own (`stack` is bottom to top). A block whose
/// middle is covered stays while one of its lines shows, with its tag on
/// the first of those: it's the only way to those lines.
pub fn keep_visible(snapshot: &mut Snapshot, stack: &[Window], monitor: &Monitor) {
    // Two fields of `snapshot`, borrowed apart: the closure reads
    // `windows` while `retain_mut` changes `targets`.
    snapshot.targets.retain_mut(|t| {
        let address = &snapshot.windows[t.window].window.address;
        let shows = |at| monitor.rect.contains(at) && uncovered(stack, address, at).is_some();
        if shows(t.hit) {
            return true;
        }
        match t
            .opens
            .iter()
            .map(|line| line.center())
            .find(|&at| shows(at))
        {
            Some(at) => {
                t.hit = at;
                true
            }
            None => false,
        }
    });
    // A tag that moved onto a line may be out of reading order now.
    snapshot.targets.sort_by_key(|t| (t.hit.y, t.hit.x));
}

/// `window` tagged from its pixels alone: what `dump --pixels` prints.
pub fn read_pixels(window: &Window, detector: Detector) -> Result<Snapshot> {
    let (found, scale, stats) = look(&mut None, window, detector)?;
    let read = Read::Pixels {
        window,
        found: &found,
        scale,
        stats: &stats,
    };
    Ok(Snapshot::build(1, window.monitor, [read]))
}

/// Takes a picture of `window` and finds its tags, with the `camera`
/// made on first use. Also returns the picture's pixels per logical pixel
/// and what it cost.
fn look(
    camera: &mut Option<Camera>,
    window: &Window,
    detector: Detector,
) -> Result<(Found, f64, Stats)> {
    let start = Instant::now();
    let until = start + CAPTURE_TIMEOUT;
    let camera = match camera {
        Some(camera) => camera,
        empty => empty.insert(Camera::connect(until)?),
    };
    let picture = camera.shoot(&window.address, until)?;
    let (frame, scale) = pixels::shrink(picture, window.rect.w);
    let found = pixels::find(&frame, detector);
    let stats = Stats {
        ms: start.elapsed().as_secs_f64() * 1000.0,
        ..Stats::default()
    };
    Ok((found, scale, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::{Point, Rect},
        pixels::Block,
    };

    fn window(address: &str, rect: Rect) -> Window {
        Window {
            address: address.into(),
            pid: 1,
            class: "kitty".into(),
            title: String::new(),
            rect,
            monitor: 0,
            focused: true,
        }
    }

    #[test]
    fn a_block_with_its_middle_covered_moves_its_tag_to_a_line_that_shows() {
        let below = window("0x1", Rect::new(0, 0, 800, 600));
        let found = Found {
            blocks: vec![Block {
                rect: [100, 100, 300, 200],
                phrases: vec![
                    [100, 100, 300, 120],
                    [100, 140, 300, 160],
                    [100, 180, 300, 200],
                ],
            }],
            ..Found::default()
        };
        let monitor = Monitor {
            id: 0,
            name: "test".into(),
            rect: below.rect,
            scale: 1.0,
            focused: true,
        };
        let stats = Stats::default();
        let read = || Read::Pixels {
            window: &below,
            found: &found,
            scale: 1.0,
            stats: &stats,
        };
        // A small window over the block's middle and its middle line only.
        let small = window("0x2", Rect::new(180, 135, 40, 30));
        let mut snapshot = Snapshot::build(1, 0, [read()]);
        keep_visible(&mut snapshot, &[below.clone(), small], &monitor);
        assert_eq!(snapshot.targets.len(), 1, "the block stays");
        assert_eq!(
            snapshot.targets[0].hit,
            Point::new(200, 110),
            "on its first line"
        );

        // One over the whole block: nothing of it shows.
        let big = window("0x3", Rect::new(50, 50, 400, 300));
        let mut snapshot = Snapshot::build(1, 0, [read()]);
        keep_visible(&mut snapshot, &[below.clone(), big], &monitor);
        assert_eq!(snapshot.targets, []);
    }
}
