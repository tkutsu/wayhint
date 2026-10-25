//! Everything about the world outside wayhint: the compositor and the
//! accessibility tree, behind a few calls on [`Desktop`] (what a monitor
//! shows and how it stacks, scan it, re-check a target) plus a few plain
//! Hyprland queries. The rest of the program sees plain data (`Window`,
//! `Monitor`, `Snapshot`, `Verdict`), never a socket, a D-Bus proxy or a
//! future.
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
pub mod surfaces;
pub mod tree;

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use futures::future;

use self::{
    atspi::Bounds,
    capture::Camera,
    surfaces::{place, popup, uncovered, unknown},
};
pub use self::{
    atspi::Stats,
    hyprland::{
        Kind, Monitor, Window, changes_workspace, cursor, events, focused_window, monitors, warn,
        window_by_address,
    },
    recheck::Verdict,
    surfaces::Stack,
};
use crate::{
    geometry::{Point, Rect},
    pixels::{self, Detector, Found},
    snapshot::{Read, Snapshot, Target},
    space::Space,
};

/// How long the re-check may take: one object, four small requests.
const RECHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// How long reading the layer apps' frames may take: a few requests per
/// frame, and a shell has a handful.
const FRAMES_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a window's picture may take, connecting included: the
/// spike's took 6 to 12 ms.
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(500);

/// The patch a document body's tag sits on, as a terminal's, in logical px.
const BODY_TAG: (i32, i32) = (36, 24);

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
    /// that may be above its own. A client that can't be read (not on the
    /// accessibility bus, or no frame clearly its own) is tagged from its
    /// pixels with `pixels`, or gets no tags when that's `None`; a layer or
    /// popup that can't be read gets none, since Hyprland's window export
    /// only takes pictures of clients. Why comes back next to the
    /// snapshot, with any frames `stack` couldn't place.
    pub fn scan(
        &mut self,
        stack: &Stack,
        monitor: &Monitor,
        pixels: Option<Detector>,
        terminals: &[String],
    ) -> (Snapshot, Vec<anyhow::Error>) {
        // A layer with no frame only covers.
        let readable: Vec<&Window> = stack
            .windows
            .iter()
            .filter(|w| w.kind == Kind::Client || !w.frames.is_empty())
            .collect();
        // `join_all` runs the scans side by side on the one thread: each
        // waits on its own app, so three windows take as long as the
        // slowest, not the sum.
        let reads = readable.iter().map(|w| async {
            if is_terminal(w, terminals) {
                bail!("{}: terminal focus is read from pixels", w.class);
            }
            self.read(w).await
        });
        let reads = self.runtime.block_on(future::join_all(reads));
        let mut seen = Vec::new();
        // `anyhow!` copies the text into the error: `Error::msg` takes it
        // as it is, and an error can't hold a borrowed `&String`.
        let mut failed: Vec<_> = stack.unplaced.iter().map(|u| anyhow!("{u}")).collect();
        for (window, read) in readable.into_iter().zip(reads) {
            let detector = pixels.filter(|_| window.kind == Kind::Client);
            match (read, detector) {
                (Ok((tree, stats)), _) => {
                    let filtered = filter::filter(&tree);
                    seen.push(Seen::Tree(tree, filtered, stats));
                }
                (Err(e), None) => failed.push(e),
                (Err(no_tree), Some(detector)) => {
                    match look(&mut self.camera, window, detector, terminals) {
                        Ok((found, scale, stats)) => {
                            seen.push(Seen::Pixels(window.clone(), found, scale, stats));
                        }
                        Err(e) => failed.push(anyhow!("{no_tree:#}, and {e:#}")),
                    }
                }
            }
        }
        self.scans += 1;
        let mut snapshot = Snapshot::build(self.scans, monitor.id, seen.iter().map(Seen::read));
        keep_visible(&mut snapshot, stack, monitor);
        self.layout(&mut snapshot, monitor);
        snapshot.obstacles.extend(
            stack
                .windows
                .iter()
                .filter(|w| {
                    !snapshot
                        .windows
                        .iter()
                        .any(|s| s.window.address == w.address)
                })
                .map(|w| w.rect),
        );
        (snapshot, failed)
    }

    /// Capture what labels may cover separately from target detection. An
    /// unreadable window stays fully protected instead of guessing it is blank.
    /// The same picture puts each document body's tag in its clearest spot.
    pub fn layout(&mut self, snapshot: &mut Snapshot, monitor: &Monitor) {
        let mut obstacles = Vec::new();
        for (index, scanned) in snapshot.windows.iter().enumerate() {
            let window = &scanned.window;
            if window.rect.intersect(monitor.rect).is_none() {
                continue;
            }
            let is_body = |t: &Target| t.window == index && filter::is_body(&t.role, t.editable);
            let bodies = snapshot.targets.iter().any(is_body);
            let captured = (|| -> Result<_> {
                if window.kind != Kind::Client {
                    bail!("only clients can be exported");
                }
                let until = Instant::now() + CAPTURE_TIMEOUT;
                let camera = match &mut self.camera {
                    Some(camera) => camera,
                    empty => empty.insert(Camera::connect(until)?),
                };
                let (frame, scale) =
                    pixels::shrink(camera.shoot(&window.address, until)?, window.rect.w);
                let cells = bodies.then(|| pixels::cell_space(&frame));
                Ok((Space::pixels(&frame), cells, scale))
            })();
            let origin = window.rect.origin();
            match &captured {
                #[allow(clippy::cast_possible_truncation)]
                Ok((space, _, scale)) => obstacles.extend(space.obstacles().into_iter().map(|r| {
                    let x = (f64::from(r.x) / scale).floor() as i32;
                    let y = (f64::from(r.y) / scale).floor() as i32;
                    let right = (f64::from(r.right()) / scale).ceil() as i32;
                    let bottom = (f64::from(r.bottom()) / scale).ceil() as i32;
                    Rect::new(x, y, right - x, bottom - y).offset(origin)
                })),
                Err(_) => obstacles.push(window.rect),
            }
            let cells = captured
                .as_ref()
                .ok()
                .and_then(|(_, cells, scale)| Some((cells.as_ref()?, *scale)));
            for target in snapshot.targets.iter_mut().filter(|t| is_body(t)) {
                focus_body(target, origin, cells);
            }
        }
        // A body's click moved off its middle.
        snapshot.targets.sort_by_key(|t| (t.hit.y, t.hit.x));
        // Complete small controls, including flat interiors. Large parent
        // containers (tab rows, panes) leave their blank body available.
        obstacles.extend(
            snapshot
                .targets
                .iter()
                .filter(|t| {
                    (t.object.is_some()
                        && (t.editable
                            || matches!(
                                t.role.as_str(),
                                "push button"
                                    | "button"
                                    | "check box"
                                    | "radio button"
                                    | "combo box"
                                    | "slider"
                            )))
                        || (!t.editable && t.shown().w <= 160 && t.shown().h <= 80)
                })
                .map(Target::shown),
        );
        snapshot.obstacles = obstacles;
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
    /// A pixel target has no object: its window must still be where it was,
    /// with nothing over the spot.
    pub fn recheck(&self, snapshot: &Snapshot, target: &Target) -> Result<Verdict> {
        // Where the surface is now and how big, unless it's gone, something
        // came over the spot (a dialog opened, a popup closed), or the frame
        // its tags came from isn't clearly its own any more (a layer that
        // reports the same box was mapped).
        let scanned = &snapshot.windows[target.window].window;
        let stack = self.stack(snapshot.monitor)?;
        let same_frames = stack
            .windows
            .iter()
            .find(|w| w.address == scanned.address)
            .is_some_and(|now| scanned.frames.iter().all(|f| now.frames.contains(f)));
        let window = same_frames
            .then(|| uncovered(&stack, &scanned.address, target.hit))
            .flatten();
        let Some(object) = &target.object else {
            return Ok(recheck::unread(scanned.rect, window));
        };
        let read = self.scanner.read(object, &target.clips, RECHECK_TIMEOUT);
        let fresh = self.runtime.block_on(read).ok();
        Ok(recheck::verdict(target, fresh.as_ref(), window))
    }
}

/// Keeps the targets worth a tag: on `monitor`, and not under a surface
/// that may be above their own (see [`uncovered`]). A block whose middle
/// is covered stays while one of its lines shows, with its tag on the
/// first of those: it's the only way to those lines.
pub fn keep_visible(snapshot: &mut Snapshot, stack: &Stack, monitor: &Monitor) {
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

/// Shrinks a document body's tag to the clearest tag-sized patch nearest
/// its middle, and clicks there: an empty cell of a sheet, a blank stretch
/// of a page. `picture` is the window's ink without grid lines
/// ([`pixels::cell_space`]), at some pixels per logical pixel, whose
/// corner is at `origin`; without one, the middle.
#[allow(clippy::cast_possible_truncation)]
fn focus_body(target: &mut Target, origin: Point, picture: Option<(&Space, f64)>) {
    let body = target.visible;
    let (w, h) = (BODY_TAG.0.min(body.w), BODY_TAG.1.min(body.h));
    let middle = body.center();
    let mut patch = Rect::new(middle.x - w / 2, middle.y - h / 2, w, h);
    if let Some((space, scale)) = picture {
        let pixel = |v: i32| (f64::from(v) * scale).round() as i32;
        let area = body.inset(4);
        let (x, y) = (pixel(area.x - origin.x), pixel(area.y - origin.y));
        let area = Rect::new(x, y, pixel(area.w), pixel(area.h));
        let aim = Point::new(pixel(middle.x - origin.x), pixel(middle.y - origin.y));
        if let Some(r) = space.spot(area, (pixel(w), pixel(h)), aim, 1) {
            let logical = |v: i32| (f64::from(v) / scale).round() as i32;
            patch = Rect::new(logical(r.x) + origin.x, logical(r.y) + origin.y, w, h);
        }
    }
    target.visible = patch;
    target.hit = patch.center();
}

/// Match executable basenames as well as classes; terminal profiles often
/// replace the class. No process-tree guessing: a child TUI is not an app ID.
fn is_terminal(window: &Window, terminals: &[String]) -> bool {
    let executable = std::fs::read_link(format!("/proc/{}/exe", window.pid)).ok();
    terminals.iter().any(|name| {
        name.eq_ignore_ascii_case(&window.class)
            || executable
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .is_some_and(|exe| name.eq_ignore_ascii_case(exe))
    })
}

/// `window` tagged from its pixels alone: what `dump --pixels` prints.
pub fn read_pixels(window: &Window, detector: Detector, terminals: &[String]) -> Result<Snapshot> {
    let (found, scale, stats) = look(&mut None, window, detector, terminals)?;
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
    terminals: &[String],
) -> Result<(Found, f64, Stats)> {
    let start = Instant::now();
    let until = start + CAPTURE_TIMEOUT;
    let camera = match camera {
        Some(camera) => camera,
        empty => empty.insert(Camera::connect(until)?),
    };
    let picture = camera.shoot(&window.address, until)?;
    let (frame, scale) = pixels::shrink(picture, window.rect.w);
    let terminal = is_terminal(window, terminals);
    let found = pixels::find(&frame, detector, terminal);
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
            kind: Kind::Client,
            frames: Vec::new(),
        }
    }

    fn stack(windows: Vec<Window>) -> Stack {
        Stack {
            popup_floor: windows.len(),
            windows,
            ..Stack::default()
        }
    }

    #[test]
    fn configured_terminals_match_class_or_executable() {
        let mut window = window("test", Rect::new(0, 0, 100, 100));
        window.class = "CustomProfile".into();
        assert!(is_terminal(&window, &["customprofile".into()]));
        assert!(!is_terminal(&window, &[]));
        window.pid = std::process::id();
        let executable = std::env::current_exe().unwrap();
        let name = executable.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(is_terminal(&window, &[name]));
        assert!(!is_terminal(&window, &["different-terminal".into()]));
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
        keep_visible(&mut snapshot, &stack(vec![below.clone(), small]), &monitor);
        assert_eq!(snapshot.targets.len(), 1, "the block stays");
        assert_eq!(
            snapshot.targets[0].hit,
            Point::new(200, 110),
            "on its first line"
        );

        // One over the whole block: nothing of it shows.
        let big = window("0x3", Rect::new(50, 50, 400, 300));
        let mut snapshot = Snapshot::build(1, 0, [read()]);
        keep_visible(&mut snapshot, &stack(vec![below.clone(), big]), &monitor);
        assert_eq!(snapshot.targets, []);
    }

    fn sheet(visible: Rect) -> Target {
        Target {
            object: None,
            window: 0,
            rect: visible,
            visible,
            hit: visible.center(),
            role: "document spreadsheet".into(),
            name: String::new(),
            editable: true,
            clips: Vec::new(),
            opens: Vec::new(),
        }
    }

    #[test]
    fn a_sheet_tag_goes_to_the_empty_cell_nearest_its_middle() {
        // A window at 100,50 whose sheet starts 80 px down: cells 80x20,
        // with text in the two around the middle (320,280 in the picture).
        let origin = Point::new(100, 50);
        let body = Rect::new(100, 130, 640, 400);
        let text = [Rect::new(244, 283, 70, 12), Rect::new(324, 283, 70, 12)];
        let rows = (80..=480).step_by(20).map(|y| Rect::new(0, y, 640, 1));
        let columns = (0..=640).step_by(80).map(|x| Rect::new(x, 80, 1, 400));
        let ink: Vec<Rect> = rows.chain(columns).chain(text).collect();
        let picture = (Space::rectangles(Rect::new(0, 0, 800, 600), &ink), 1.0);

        let mut target = sheet(body);
        focus_body(&mut target, origin, Some((&picture.0, picture.1)));
        let patch = target.visible;
        assert_eq!((patch.w, patch.h), BODY_TAG);
        assert_eq!(target.hit, patch.center());
        assert_eq!(patch.intersect(body), Some(patch), "inside the sheet");
        let in_picture = Rect::new(patch.x - origin.x, patch.y - origin.y, patch.w, patch.h);
        assert!(text.iter().all(|t| t.intersect(in_picture).is_none()));
        let middle = body.center();
        let off = (target.hit.x - middle.x).abs() + (target.hit.y - middle.y).abs();
        assert!(off <= 60, "an empty cell next to the text, {off} px off");
    }

    #[test]
    fn calc_tag_sits_on_an_empty_cell_or_the_least_busy_one() {
        // Calc at 0,38 (2026-10-23): data in A1:E19 at the top left, the
        // rest of the sheet empty.
        let frame = pixels::tests::load(std::path::Path::new("tests/fixtures/calc.png"));
        let cells = pixels::cell_space(&frame);
        let origin = Point::new(0, 38);
        let ink = |r: Rect| cells.ink(r.offset(Point::new(0, -38)));
        let middle_patch = |body: Rect| {
            let c = body.center();
            Rect::new(c.x - 18, c.y - 12, 36, 24)
        };
        let data = Rect::new(45, 208, 220, 320);
        // Grid lines don't count: the empty middle is clear as it is.
        for (body, reach) in [(Rect::new(45, 208, 2438, 1168), 8), (data, 400)] {
            let mut target = sheet(body);
            focus_body(&mut target, origin, Some((&cells, 1.0)));
            let patch = target.visible;
            assert_eq!(patch.intersect(body), Some(patch), "inside the sheet");
            assert!(
                ink(patch) <= ink(middle_patch(body)),
                "no busier than the middle"
            );
            let middle = body.center();
            let off = (target.hit.x - middle.x).abs() + (target.hit.y - middle.y).abs();
            assert!(off <= reach, "{off} px from the middle");
            if body != data {
                assert_eq!(ink(patch), 0, "an empty cell");
                assert!(patch.intersect(data).is_none(), "off the data");
            }
        }
    }

    #[test]
    fn a_body_without_a_picture_is_tagged_and_clicked_in_its_middle() {
        let mut target = sheet(Rect::new(0, 0, 400, 300));
        focus_body(&mut target, Point::new(0, 0), None);
        assert_eq!(target.visible, Rect::new(182, 138, 36, 24));
        assert_eq!(target.hit, Point::new(200, 150));
    }
}
