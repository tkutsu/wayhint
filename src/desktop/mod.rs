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
mod kitty;
mod menus;
mod notify;
pub mod recheck;
pub mod surfaces;
pub mod tree;

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    os::unix::fs::DirBuilderExt as _,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use futures::future;

use self::{
    atspi::Bounds,
    capture::Camera,
    menus::Menus,
    surfaces::{above, add_menus, place, popup, uncovered, unknown},
    tree::ObjectId,
};
pub use self::{
    atspi::Stats,
    hyprland::{
        CursorVisibility, Kind, Monitor, Window, changes_workspace, cursor, events, focus_window,
        focused_window, monitors, window_by_address,
    },
    kitty::{is_kitty, listen_on},
    notify::{log, warn},
    recheck::Verdict,
    surfaces::Stack,
};
use crate::{
    geometry::{Point, Rect},
    pixels::{self, Detector, Found, Frame},
    snapshot::{Read, Snapshot, Target},
    space::Space,
    text::{Check, Place, Span, Text},
};

/// How long the re-check may take: one object, four small requests.
const RECHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// How long reading the layer apps' frames may take: a few requests per
/// frame, and a shell has a handful.
const FRAMES_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a scan's pictures may take, all of them and connecting
/// included: the spike's took 6 to 12 ms each. Shared, so windows with no
/// tree can't each add their own wait before `Esc` is read.
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long reading text for text mode may take, every window together.
const TEXT_TIMEOUT: Duration = Duration::from_secs(1);

/// How long finding where a query's new matches are may take.
const LOCATE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long kitty may take to say what one window shows: one request,
/// answered in a few ms.
const KITTY_TIMEOUT: Duration = Duration::from_millis(500);

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
    /// Each client's picture for placing labels, by address, until
    /// [`Desktop::new_round`].
    pictures: HashMap<String, Picture>,
    /// The apps' open menus, once a run starts following them.
    menus: Option<Menus>,
}

/// A client's picture as `layout` reads it, and what it read there. A
/// refresh places labels around the ink it showed then instead of taking
/// another: one picture, its ink and its cells cost tens of ms a window.
struct Picture {
    /// The window's size when it was taken: a resized window takes another.
    size: (i32, i32),
    /// Shrunk, with its pixels per logical pixel.
    frame: Frame,
    scale: f64,
    /// Where it shows ink, in its own pixels.
    ink: Vec<Rect>,
    /// Its cells, read the first time a document body asks.
    cells: Option<Space>,
}

impl Picture {
    fn new((frame, scale): (Frame, f64), window: &Window) -> Self {
        Self {
            size: (window.rect.w, window.rect.h),
            ink: Space::pixels(&frame).obstacles(),
            frame,
            scale,
            cells: None,
        }
    }

    fn cells(&mut self) -> &Space {
        self.cells
            .get_or_insert_with(|| pixels::cell_space(&self.frame))
    }
}

/// One window as the scan saw it, until the snapshot is built from it.
enum Seen {
    Tree(tree::Tree, filter::Filtered, Stats),
    Terminal(Window, Option<Rect>, Stats),
    /// With its picture, shrunk, and the picture's pixels per logical
    /// pixel: `layout` reads the same one. A terminal cursor needs no picture.
    Pixels(Window, Found, Option<(Frame, f64)>, Stats),
    Untagged(Window),
}

impl Seen {
    fn address(&self) -> &str {
        match self {
            Self::Tree(tree, ..) => &tree.window.address,
            Self::Terminal(window, ..) | Self::Pixels(window, ..) | Self::Untagged(window) => {
                &window.address
            }
        }
    }

    fn read(&self) -> Read<'_> {
        match self {
            Self::Tree(tree, filtered, stats) => Read::Tree(tree, filtered, stats),
            Self::Terminal(window, cursor, stats) => Read::Terminal {
                window,
                cursor: *cursor,
                stats,
            },
            Self::Pixels(window, found, picture, stats) => Read::Pixels {
                window,
                found,
                scale: picture.as_ref().map_or(1.0, |(_, scale)| *scale),
                stats,
            },
            Self::Untagged(window) => Read::Untagged(window),
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
            pictures: HashMap::new(),
            menus: None,
        })
    }

    /// Follows the menus apps open from now on, so [`Desktop::stack`] has
    /// them: a run's right click opens one.
    pub fn watch_menus(&mut self) -> Result<()> {
        self.menus = Some(Menus::watch()?);
        Ok(())
    }

    /// Reads `window`'s tree, unfiltered: what `dump --raw` prints.
    pub fn read_tree(&self, window: &Window) -> Result<(tree::Tree, Stats)> {
        self.runtime
            .block_on(self.scanner.scan(window, self.bounds))
    }

    /// What `monitor` shows, bottom to top: Hyprland's clients and layers
    /// (see `hyprland::shown`), each layer with the frames that may be its
    /// own, then the popups, and each open menu right above its window
    /// (once [`Desktop::watch_menus`] is on). Frames are read only for the
    /// apps that have a layer and no client.
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
        let open = self.menus.as_ref().map(Menus::open).unwrap_or_default();
        if !open.is_empty() {
            let read = self.scanner.menus(&open, FRAMES_TIMEOUT);
            // A menu that didn't answer has just closed, most likely.
            let menus: Vec<_> = self.runtime.block_on(read).into_iter().flatten().collect();
            add_menus(&mut stack, &menus, &shown.xwayland, shown.screen, monitor);
        }
        Ok(stack)
    }

    /// Scans every surface in `stack` (from [`Desktop::stack`]) at once and
    /// keeps what deserves a tag: on `monitor`, and not under a surface
    /// that may be above its own. A client that can't be read (not on the
    /// accessibility bus, or no frame clearly its own) is tagged from its
    /// pixels with `pixels`, or gets no tags when that's `None`. Terminals
    /// get one focus tag when `terminal_hint` is on, independently. A layer or
    /// popup that can't be read gets none, since Hyprland's window export
    /// only takes pictures of clients. kitty's surfaces left without tags
    /// (its windows with no pixels, its dropdown layer) are kept untagged,
    /// for text mode to ask kitty what they show. Why comes back next to
    /// the snapshot, with any frames `stack` couldn't place.
    pub fn scan(
        &mut self,
        stack: &Stack,
        monitor: &Monitor,
        pixels: Option<Detector>,
        terminal_hint: bool,
        window_hint: bool,
        terminals: &[String],
    ) -> (Snapshot, Vec<Unread>) {
        // A layer with no frame only covers.
        let readable: Vec<&Window> = stack
            .windows
            .iter()
            .filter(|w| {
                w.kind == Kind::Client
                    || !w.frames.is_empty()
                    || (is_terminal(w, terminals) && is_kitty(w))
            })
            .collect();
        // `join_all` runs the scans side by side on the one thread: each
        // waits on its own app, so three windows take as long as the
        // slowest, not the sum.
        let reads = readable.iter().map(|w| async {
            if is_terminal(w, terminals) {
                bail!("{}: terminal focus is read separately", w.class);
            }
            self.read(w).await
        });
        let reads = self.runtime.block_on(future::join_all(reads));
        let until = Instant::now() + CAPTURE_TIMEOUT;
        let mut seen = Vec::new();
        // `anyhow!` copies the text into the error: `Error::msg` takes it
        // as it is, and an error can't hold a borrowed `&String`.
        let mut failed: Vec<_> = stack
            .unplaced
            .iter()
            .map(|u| Unread {
                app: None,
                error: anyhow!("{u}"),
            })
            .collect();
        for (window, read) in readable.into_iter().zip(reads) {
            if is_terminal(window, terminals) {
                if terminal_hint {
                    seen.push(read_terminal(window));
                }
                continue;
            }
            let detector = pixels.filter(|_| window.kind == Kind::Client);
            match (read, detector) {
                (Ok((tree, stats)), _) => {
                    let filtered = filter::filter(&tree);
                    seen.push(Seen::Tree(tree, filtered, stats));
                }
                (Err(e), None) => failed.push(Unread::new(window, e)),
                (Err(no_tree), Some(detector)) => {
                    match look(&mut self.camera, window, detector, false, until) {
                        Ok((found, picture, stats)) => {
                            seen.push(Seen::Pixels(window.clone(), found, Some(picture), stats));
                        }
                        Err(e) => {
                            failed.push(Unread::new(window, anyhow!("{no_tree:#}, and {e:#}")));
                        }
                    }
                }
            }
        }
        let untagged: Vec<Seen> = stack
            .windows
            .iter()
            .filter(|w| {
                w.kind != Kind::Popup && (is_kitty(w) || window_hint && w.kind == Kind::Client)
            })
            .filter(|w| !seen.iter().any(|s| s.address() == w.address))
            .map(|w| Seen::Untagged(w.clone()))
            .collect();
        seen.extend(untagged);
        // Back to bottom to top: a tag stays within the topmost window
        // around it (`Painter::arrange`).
        seen.sort_by_key(|s| stack.windows.iter().position(|w| w.address == s.address()));
        self.scans += 1;
        let mut snapshot = Snapshot::build(self.scans, monitor.id, seen.iter().map(Seen::read));
        keep_visible(&mut snapshot, stack, monitor);
        // A picture taken this scan is the one `layout` reads; a window
        // seen without one needs none.
        let mut unpictured = Vec::new();
        for seen in seen {
            match seen {
                Seen::Pixels(window, _, Some(picture), _) => {
                    let picture = Picture::new(picture, &window);
                    self.pictures.insert(window.address, picture);
                }
                Seen::Terminal(window, ..)
                | Seen::Pixels(window, _, None, _)
                | Seen::Untagged(window) => {
                    unpictured.push(window.address);
                }
                Seen::Tree(..) => {}
            }
        }
        self.layout(&mut snapshot, monitor, stack, &unpictured);
        let unscanned = stack.windows.iter().filter(|w| {
            !snapshot
                .windows
                .iter()
                .any(|s| s.window.address == w.address)
        });
        for window in unscanned {
            let covers: Vec<Rect> = above(stack, &window.address).map(|w| w.rect).collect();
            snapshot.obstacles.extend(in_view([window.rect], &covers));
        }
        if window_hint {
            Self::window_hints(&mut snapshot, stack, monitor);
        }
        (snapshot, failed)
    }

    /// Add one focus target for each visible client, independent of its contents.
    fn window_hints(snapshot: &mut Snapshot, stack: &Stack, monitor: &Monitor) {
        for window in stack.windows.iter().filter(|w| w.kind == Kind::Client) {
            let Some(area) = window.rect.intersect(monitor.rect) else {
                continue;
            };
            let covers: Vec<_> = above(stack, &window.address).map(|w| w.rect).collect();
            let parts = in_view([area], &covers);
            let Some(visible) = parts
                .iter()
                .copied()
                .max_by_key(|r| i64::from(r.w) * i64::from(r.h))
            else {
                continue;
            };
            let label_area = parts
                .iter()
                .copied()
                .filter(|r| r.w >= BODY_TAG.0 && r.h >= BODY_TAG.1)
                .min_by_key(|r| area.right() - r.right() + r.y - area.y)
                .unwrap_or(visible);
            let Some(index) = snapshot
                .windows
                .iter()
                .position(|s| s.window.address == window.address)
            else {
                continue;
            };
            let target = Target {
                object: None,
                window: index,
                rect: window.rect,
                visible: label_area,
                hit: visible.center(),
                role: "window".into(),
                name: window.title.clone(),
                editable: false,
                clips: Vec::new(),
                opens: Vec::new(),
            };
            snapshot.targets.push(target);
        }
        snapshot.targets.sort_by_key(|t| (t.hit.y, t.hit.x));
    }

    /// Aim the wheel at the visible part of the window focused right now.
    pub fn scroll_target(&self) -> Result<Option<Point>> {
        let Some(window) = focused_window()? else {
            return Ok(None);
        };
        let Some(monitor) = monitors()?.into_iter().find(|m| m.id == window.monitor) else {
            return Ok(None);
        };
        let stack = self.stack(window.monitor)?;
        Ok(visible_window(&stack, &window.address, monitor.rect).map(Rect::center))
    }

    /// Capture what labels may cover separately from target detection. An
    /// unreadable window stays fully protected instead of guessing it is blank.
    /// The same picture puts each document body's tag in its clearest spot
    /// that no surface in `stack` may cover. A client's picture is taken
    /// once a round ([`Desktop::new_round`]), or again if it resized;
    /// one in `unpictured` (by address) gets none, as a terminal cursor or
    /// fixed fallback hint needs none.
    pub fn layout(
        &mut self,
        snapshot: &mut Snapshot,
        monitor: &Monitor,
        stack: &Stack,
        unpictured: &[String],
    ) {
        let mut obstacles = Vec::new();
        // Text sources kept: a button only where its picture shows letters.
        let mut keep: Vec<bool> = snapshot.texts.iter().map(|t| t.button.is_none()).collect();
        for (index, scanned) in snapshot.windows.iter().enumerate() {
            let window = &scanned.window;
            if window.rect.intersect(monitor.rect).is_none() {
                continue;
            }
            let covers: Vec<Rect> = above(stack, &window.address).map(|w| w.rect).collect();
            if unpictured.contains(&window.address) {
                obstacles.extend(in_view([window.rect], &covers));
                continue;
            }
            let is_body = |t: &Target| t.window == index && filter::is_body(&t.role, t.editable);
            let bodies = snapshot.targets.iter().any(is_body);
            // Hyprland's window export only takes pictures of clients.
            let picture = match window.kind {
                Kind::Client => self.picture(window).ok(),
                _ => None,
            };
            if let Some(picture) = &picture {
                let scale = picture.scale;
                for (i, text) in snapshot.texts.iter().enumerate() {
                    if let Some(button) = text.button.filter(|_| text.window == index) {
                        let r = button.rect;
                        #[allow(clippy::cast_possible_truncation)]
                        let at = |v: i32, from: i32| (f64::from(v - from) * scale).round() as i32;
                        let (x, y) = (window.rect.x, window.rect.y);
                        let corners = [at(r.x, x), at(r.y, y), at(r.right(), x), at(r.bottom(), y)];
                        keep[i] = pixels::shows_letters(&picture.frame, corners, button.letters);
                    }
                }
            }
            let origin = window.rect.origin();
            // What a window shows only blocks tags where it's on top: an
            // unreadable background layer would otherwise block them all.
            match &picture {
                #[allow(clippy::cast_possible_truncation)]
                Some(picture) => {
                    let scale = picture.scale;
                    let ink = picture.ink.iter().map(|r| {
                        let x = (f64::from(r.x) / scale).floor() as i32;
                        let y = (f64::from(r.y) / scale).floor() as i32;
                        let right = (f64::from(r.right()) / scale).ceil() as i32;
                        let bottom = (f64::from(r.bottom()) / scale).ceil() as i32;
                        Rect::new(x, y, right - x, bottom - y).offset(origin)
                    });
                    obstacles.extend(in_view(ink, &covers));
                }
                None => obstacles.extend(in_view([window.rect], &covers)),
            }
            let cells = picture.filter(|_| bodies).map(|picture| {
                let scale = picture.scale;
                (picture.cells(), scale)
            });
            for target in snapshot.targets.iter_mut().filter(|t| is_body(t)) {
                focus_body(target, origin, cells, monitor.rect, &covers);
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
        let mut keep = keep.into_iter();
        snapshot.texts.retain(|_| keep.next().unwrap_or(true));
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
        // reports the same box was mapped, or one gained controls).
        let scanned = &snapshot.windows[target.window].window;
        let stack = self.stack(snapshot.monitor)?;
        let resolve = |now: &Window| {
            let read = self.runtime.block_on(self.read(now));
            read.ok().map(|(tree, _)| tree.window.frames)
        };
        let owned = stack
            .windows
            .iter()
            .find(|w| w.address == scanned.address)
            .is_some_and(|now| same_frame(scanned, now, resolve));
        let window = owned
            .then(|| uncovered(&stack, &scanned.address, target.hit))
            .flatten();
        let Some(object) = &target.object else {
            return Ok(recheck::unread(scanned.rect, window));
        };
        let menu = (scanned.kind == Kind::Menu)
            .then(|| scanned.frames.first())
            .flatten();
        let read = self
            .scanner
            .read(object, &target.clips, menu, RECHECK_TIMEOUT);
        let fresh = self.runtime.block_on(read).ok();
        Ok(recheck::verdict(target, fresh.as_ref(), window))
    }

    /// The text of every object in `snapshot.texts` that shows some, and
    /// of every kitty window's panes, from kitty, for text mode.
    pub fn read_text(&self, snapshot: &Snapshot) -> Reading {
        let mut others = Vec::new();
        let mut unread = Vec::new();
        let windows = snapshot.windows.iter().enumerate();
        for (index, scanned) in windows.filter(|(_, s)| is_kitty(&s.window)) {
            let window = &scanned.window;
            match kitty::read(window, KITTY_TIMEOUT) {
                Ok(panes) => others.extend(panes.into_iter().map(|pane| Text {
                    rect: pane.cells.whole(),
                    place: Place::Cells(pane.cells),
                    window: index,
                    name: pane.title,
                    area: window.rect,
                    start: 0,
                    content: pane.content,
                })),
                Err(e) => unread.push(Unread::new(window, e.context(window.class.clone()))),
            }
        }
        let (mut texts, incomplete) = self.read_objects(snapshot);
        texts.append(&mut others);
        Reading {
            texts,
            incomplete,
            unread,
        }
    }

    /// A picture of the client at `address`, connecting first the first
    /// time.
    fn shoot(&mut self, address: &str) -> Result<Frame> {
        let until = Instant::now() + CAPTURE_TIMEOUT;
        let camera = match &mut self.camera {
            Some(camera) => camera,
            empty => empty.insert(Camera::connect(until)?),
        };
        camera.shoot(address, until)
    }

    /// `window`'s picture for placing labels: the one kept, unless the
    /// window resized since, else a new one.
    fn picture(&mut self, window: &Window) -> Result<&mut Picture> {
        let picture = match self.pictures.remove(&window.address) {
            Some(kept) if kept.size == (window.rect.w, window.rect.h) => kept,
            _ => {
                let shot = pixels::shrink(self.shoot(&window.address)?, window.rect.w);
                Picture::new(shot, window)
            }
        };
        Ok(self
            .pictures
            .entry(window.address.clone())
            .or_insert(picture))
    }

    /// Lets go of what the last round's scans kept, the windows' pictures
    /// and what each object is, so the next scan reads them afresh: a
    /// round's first scan does, its refreshes don't.
    pub fn new_round(&mut self) {
        self.pictures.clear();
        self.scanner.forget();
    }

    /// The text of every object in `snapshot.texts` that shows some, and
    /// why the reading stopped early, if it did. A menu's items were read
    /// by the scan; the rest are asked now.
    fn read_objects(&self, snapshot: &Snapshot) -> (Vec<Text>, Option<String>) {
        let origin = |window: usize| snapshot.windows[window].window.rect.origin();
        let (items, asked): (Vec<_>, Vec<_>) = snapshot
            .texts
            .iter()
            .enumerate()
            .partition(|(_, source)| source.menu.is_some());
        let objects: Vec<(&ObjectId, Rect, bool)> = asked
            .iter()
            .map(|(_, source)| {
                let at = origin(source.window);
                let area = source.area.offset(Point::new(-at.x, -at.y));
                (&source.object, area, source.named)
            })
            .collect();
        let read = self.scanner.read_texts(&objects, TEXT_TIMEOUT);
        let (reads, incomplete) = self.runtime.block_on(read);
        let mut texts: Vec<Text> = reads
            .into_iter()
            .zip(&asked)
            .filter_map(|(read, &(index, source))| {
                let read = read?;
                Some(Text {
                    place: Place::Object(index),
                    window: source.window,
                    name: read.name,
                    rect: read.bounds.offset(origin(source.window)),
                    area: source.area,
                    start: read.start,
                    content: read.content,
                })
            })
            .collect();
        texts.extend(items.into_iter().filter_map(|(index, source)| {
            let item = source.menu.as_ref()?;
            item.rect.intersect(source.area)?;
            Some(Text {
                place: Place::Object(index),
                window: source.window,
                name: item.name.clone(),
                rect: item.rect,
                area: source.area,
                start: 0,
                content: item.name.clone(),
            })
        }));
        (texts, incomplete)
    }

    /// Where each of `spans` of `texts` (read from `snapshot`) is on
    /// screen: a global box per line, none if the app can't say. A text
    /// that is a name has no characters to ask about; a terminal's isn't
    /// asked, its cells say.
    pub fn locate(&self, snapshot: &Snapshot, texts: &[Text], spans: &[Span]) -> Vec<Vec<Rect>> {
        let asks: Vec<Option<(&ObjectId, u32, u32)>> = spans
            .iter()
            .map(|s| match texts[s.text].place {
                Place::Object(index) => {
                    let source = &snapshot.texts[index];
                    (!source.named).then_some((&source.object, s.start, s.end))
                }
                Place::Cells(_) => None,
            })
            .collect();
        let boxes = self
            .runtime
            .block_on(self.scanner.locate(&asks, LOCATE_TIMEOUT));
        boxes
            .into_iter()
            .zip(spans)
            .map(|(parts, span)| placed(snapshot, &texts[span.text], span, parts))
            .collect()
    }

    /// Whether the text `target` was made from still reads what `check`
    /// says, and if not, why: the last look before acting on a match.
    /// An object's characters must also still be under `target`'s hit,
    /// since scrolling moves them inside a box that stays put; a
    /// terminal's must still be in the same cells.
    pub fn still_reads(&self, snapshot: &Snapshot, target: &Target, check: &Check) -> Result<()> {
        let Check {
            place,
            start,
            end,
            text,
        } = check;
        let (start, end) = (*start, *end);
        let window = target.window;
        let now: String = match place {
            Place::Object(index) => {
                let object = &snapshot.texts[*index].object;
                let span = [Some((object, start, end))];
                let read = self.scanner.read_range(object, start, end, RECHECK_TIMEOUT);
                let locate = self.scanner.locate(&span, RECHECK_TIMEOUT);
                let (now, parts) = self.runtime.block_on(future::join(read, locate));
                let now = now?;
                ensure!(now == *text, "the text changed");
                let origin = snapshot.windows[window].window.rect.origin();
                let under = parts
                    .iter()
                    .flatten()
                    .any(|part| part.offset(origin).contains(target.hit));
                ensure!(under, "the text moved");
                return Ok(());
            }
            Place::Cells(cells) => {
                let panes = kitty::read(&snapshot.windows[window].window, KITTY_TIMEOUT)?;
                let pane = panes
                    .into_iter()
                    .find(|p| p.cells.pane == cells.pane)
                    .context("the pane closed")?;
                ensure!(pane.cells.same_place(cells, start, end), "the text moved");
                let take = end.saturating_sub(start) as usize;
                pane.content
                    .chars()
                    .skip(start as usize)
                    .take(take)
                    .collect()
            }
        };
        ensure!(now == *text, "the text changed");
        Ok(())
    }
}

/// Where `span` of `text` is, global: in an object, the `parts` its app
/// gave, window-relative; in a terminal, worked out. A name
/// has no characters to ask about, long or short: the match is its
/// object, as the object's tag would be.
fn placed(snapshot: &Snapshot, text: &Text, span: &Span, parts: Vec<Rect>) -> Vec<Rect> {
    match &text.place {
        Place::Cells(cells) => cells.boxes(span.start, span.end),
        Place::Object(index) if snapshot.texts[*index].named => vec![text.rect],
        Place::Object(_) => {
            let origin = snapshot.windows[text.window].window.rect.origin();
            parts.into_iter().map(|r| r.offset(origin)).collect()
        }
    }
}

/// wayhint's own directory in the user's runtime directory, made if it
/// isn't there: only this user can read or change what's in it, and it's
/// in memory.
fn runtime_dir() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR isn't set")?;
    let dir = PathBuf::from(runtime).join("wayhint");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    Ok(dir)
}

/// What text mode reads: the texts, and what got in the way.
pub struct Reading {
    pub texts: Vec<Text>,
    /// Why reading the accessible objects stopped early, if it did.
    pub incomplete: Option<String>,
    /// The windows whose text couldn't be read, and why.
    pub unread: Vec<Unread>,
}

/// A failed read keeps its app separately from the error's wording, so
/// warnings from several windows can share one notification.
pub struct Unread {
    /// Unknown for compositor placement failures that have no window.
    pub app: Option<String>,
    pub error: anyhow::Error,
}

impl Unread {
    fn new(window: &Window, error: anyhow::Error) -> Self {
        Self {
            app: Some(window.class.clone()),
            error,
        }
    }
}

/// Whether the frame `scanned`'s tags came from is still clearly the
/// surface's own, with `now` the surface as it stands. Of several frames
/// that may be, `resolve` picks one again by the scan's rule
/// ([`Desktop::read`]): one mapped since, or one that gained controls, can
/// leave it ambiguous (`None`) or pick another.
fn same_frame(
    scanned: &Window,
    now: &Window,
    resolve: impl FnOnce(&Window) -> Option<Vec<tree::ObjectId>>,
) -> bool {
    match now.frames.len() {
        0 | 1 => now.frames == scanned.frames,
        _ => resolve(now).is_some_and(|picked| picked == scanned.frames),
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
        let shows = |at| shows(stack, monitor, address, at);
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

/// Whether the point `at` of the surface at `address` is in view: on
/// `monitor`, and under no surface that may be above it.
pub fn shows(stack: &Stack, monitor: &Monitor, address: &str, at: Point) -> bool {
    monitor.rect.contains(at) && uncovered(stack, address, at).is_some()
}

/// The parts of `rects` that none of `covers` hides.
fn in_view(rects: impl IntoIterator<Item = Rect>, covers: &[Rect]) -> Vec<Rect> {
    let mut parts: Vec<Rect> = rects.into_iter().collect();
    for &cover in covers {
        parts = parts.into_iter().flat_map(|r| r.minus(cover)).collect();
    }
    parts
}

/// The largest exposed part of a current client, on its monitor.
fn visible_window(stack: &Stack, address: &str, screen: Rect) -> Option<Rect> {
    let window = stack.windows.iter().find(|w| w.address == address)?;
    let area = window.rect.intersect(screen)?;
    let covers: Vec<_> = above(stack, address).map(|w| w.rect).collect();
    in_view([area], &covers)
        .into_iter()
        .max_by_key(|r| i64::from(r.w) * i64::from(r.h))
}

/// Shrinks a document body's tag to the clearest tag-sized patch nearest
/// its middle, and clicks there: an empty cell of a sheet, a blank stretch
/// of a page. `picture` is the window's ink without grid lines
/// ([`pixels::cell_space`]), at some pixels per logical pixel, whose
/// corner is at `origin`. Only a patch on `screen` and clear of `covers`
/// (the surfaces that may be above) will do; without one, or without a
/// picture, the tag stays on the click `keep_visible` found showing.
#[allow(clippy::cast_possible_truncation)]
fn focus_body(
    target: &mut Target,
    origin: Point,
    picture: Option<(&Space, f64)>,
    screen: Rect,
    covers: &[Rect],
) {
    let body = target.visible;
    let (w, h) = (BODY_TAG.0.min(body.w), BODY_TAG.1.min(body.h));
    let at = target.hit;
    let mut patch = Rect::new(at.x - w / 2, at.y - h / 2, w, h);
    if let Some((space, scale)) = picture
        && let Some(area) = body.intersect(screen)
    {
        let pixel = |v: i32| (f64::from(v) * scale).round() as i32;
        let to_picture = |r: Rect| {
            let (x, y) = (pixel(r.x - origin.x), pixel(r.y - origin.y));
            Rect::new(x, y, pixel(r.w), pixel(r.h))
        };
        let blocked: Vec<Rect> = covers.iter().map(|&r| to_picture(r)).collect();
        let middle = body.center();
        let aim = Point::new(pixel(middle.x - origin.x), pixel(middle.y - origin.y));
        let size = (pixel(w), pixel(h));
        if let Some(r) = space
            .block(&blocked)
            .spot(to_picture(area.inset(4)), size, aim, 1)
        {
            let logical = |v: i32| (f64::from(v) / scale).round() as i32;
            let spot = Rect::new(logical(r.x) + origin.x, logical(r.y) + origin.y, w, h);
            // Every patch may be busy or covered: a covered one is no use.
            let c = spot.center();
            if screen.contains(c) && !covers.iter().any(|r| r.contains(c)) {
                patch = spot;
            }
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

/// One focus target on Kitty's active cursor cell, without capturing pixels.
fn terminal_cursor(window: &Window) -> Result<(Rect, Stats)> {
    let start = Instant::now();
    let cell = kitty::cursor(window, KITTY_TIMEOUT)?
        .intersect(Rect::new(0, 0, window.rect.w, window.rect.h))
        .context("kitty's cursor is outside its window")?;
    Ok((
        cell,
        Stats {
            ms: start.elapsed().as_secs_f64() * 1000.0,
            ..Stats::default()
        },
    ))
}

/// Use Kitty's visible cursor, or a fixed focus hint without taking a picture.
fn read_terminal(window: &Window) -> Seen {
    let (cursor, stats) = if is_kitty(window) {
        terminal_cursor(window)
            .map(|(cell, stats)| (Some(cell), stats))
            .unwrap_or_default()
    } else {
        (None, Stats::default())
    };
    Seen::Terminal(window.clone(), cursor, stats)
}

/// `window` tagged from its pixels alone: what `dump --pixels` prints.
pub fn read_pixels(window: &Window, detector: Detector, terminals: &[String]) -> Result<Snapshot> {
    let terminal = is_terminal(window, terminals);
    let until = Instant::now() + CAPTURE_TIMEOUT;
    let seen = if terminal {
        read_terminal(window)
    } else {
        let (found, picture, stats) = look(&mut None, window, detector, false, until)?;
        Seen::Pixels(window.clone(), found, Some(picture), stats)
    };
    Ok(Snapshot::build(1, window.monitor, [seen.read()]))
}

/// Takes a picture of `window` by `until` and finds its tags, with the
/// `camera` made on first use. Also returns the picture, shrunk, with its
/// pixels per logical pixel, and what it cost.
fn look(
    camera: &mut Option<Camera>,
    window: &Window,
    detector: Detector,
    terminal: bool,
    until: Instant,
) -> Result<(Found, (Frame, f64), Stats)> {
    let start = Instant::now();
    let camera = match camera {
        Some(camera) => camera,
        empty => empty.insert(Camera::connect(until)?),
    };
    let picture = camera.shoot(&window.address, until)?;
    let (frame, scale) = pixels::shrink(picture, window.rect.w);
    let found = pixels::find(&frame, detector, terminal);
    let stats = Stats {
        ms: start.elapsed().as_secs_f64() * 1000.0,
        ..Stats::default()
    };
    Ok((found, (frame, scale), stats))
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
    fn window_targets_need_no_tree_and_skip_layers_and_fully_covered_windows() {
        let screen = Rect::new(0, 0, 800, 600);
        let monitor = Monitor {
            id: 0,
            name: "test".into(),
            rect: screen,
            scale: 1.0,
            focused: true,
        };
        let mut layer = window("bar", Rect::new(0, 0, 800, 20));
        layer.kind = Kind::Layer { level: 2 };
        let stack = stack(vec![
            window("hidden", Rect::new(100, 100, 200, 200)),
            window("visible", screen),
            layer,
        ]);
        let mut snapshot = Snapshot::build(1, 0, stack.windows.iter().map(Read::Untagged));
        Desktop::window_hints(&mut snapshot, &stack, &monitor);
        assert_eq!(snapshot.targets.len(), 1);
        let target = &snapshot.targets[0];
        assert_eq!(snapshot.windows[target.window].window.address, "visible");
        assert_eq!(target.shown(), Rect::new(0, 20, 800, 580));
        assert!(shows(&stack, &monitor, "visible", target.hit));
        assert!(target.allows(crate::action::Action::Left) && !target.in_pixels());
    }

    #[test]
    fn a_window_label_stays_near_the_top_right_when_its_middle_is_covered() {
        let screen = Rect::new(-800, -100, 800, 600);
        let monitor = Monitor {
            id: 0,
            name: "test".into(),
            rect: screen,
            scale: 1.0,
            focused: true,
        };
        let stack = stack(vec![
            window("back", screen),
            window("front", Rect::new(-550, 0, 300, 300)),
        ]);
        let mut snapshot = Snapshot::build(1, 0, stack.windows.iter().map(Read::Untagged));
        Desktop::window_hints(&mut snapshot, &stack, &monitor);
        let target = snapshot
            .targets
            .iter()
            .find(|t| snapshot.windows[t.window].window.address == "back")
            .unwrap();
        assert_eq!(target.shown().right(), screen.right());
        assert_eq!(target.shown().y, screen.y);
        assert!(shows(&stack, &monitor, "back", target.hit));
        let at = visible_window(&stack, "back", screen).unwrap().center();
        assert!(shows(&stack, &monitor, "back", at));
        assert!(visible_window(&stack, "closed", screen).is_none());
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
    fn terminals_without_remote_control_get_the_fixed_fallback_in_every_pixel_mode() {
        for class in ["foot", "kitty"] {
            let mut window = window("test", Rect::new(100, 50, 800, 600));
            window.class = class.into();
            window.pid = 0;
            for detector in [None, Some(Detector::Fast), Some(Detector::Full)] {
                let snapshot = if let Some(detector) = detector {
                    read_pixels(&window, detector, &[class.into()]).unwrap()
                } else {
                    Snapshot::build(1, 0, [read_terminal(&window).read()])
                };
                assert_eq!(snapshot.targets.len(), 1);
                let target = &snapshot.targets[0];
                assert_eq!(target.rect, Rect::new(110, 616, 36, 24));
                assert_eq!(target.role, "terminal fallback");
                assert!(target.editable && target.in_pixels() && target.opens.is_empty());
            }
        }
    }

    #[test]
    fn reported_terminal_cursor_is_exact_and_missing_cursor_gets_a_fallback() {
        let window = window("test", Rect::new(100, 50, 800, 600));
        let cell = Rect::new(57, 82, 8, 18);
        for cursor in [Some(cell), None] {
            let seen = Seen::Terminal(window.clone(), cursor, Stats::default());
            let snapshot = Snapshot::build(1, 0, [seen.read()]);
            let target = &snapshot.targets[0];
            assert_eq!(target.in_pixels(), cursor.is_none());
            assert!(target.editable && target.opens.is_empty());
            if cursor.is_some() {
                assert_eq!(target.role, "terminal");
                assert_eq!(target.rect, Rect::new(157, 132, 8, 18));
                assert_eq!(target.hit, Point::new(161, 141));
            } else {
                assert_eq!(target.rect, Rect::new(110, 616, 36, 24));
            }
        }
    }

    #[test]
    #[ignore = "requires a live desktop and WAYHINT_TEST_KITTY_ADDRESS"]
    fn live_terminal_hint_tracks_kitty_cursor_in_every_pixel_mode() {
        let address = std::env::var("WAYHINT_TEST_KITTY_ADDRESS").unwrap();
        let window = window_by_address(&address).unwrap().unwrap();
        assert!(is_kitty(&window));
        let monitor = monitors()
            .unwrap()
            .into_iter()
            .find(|m| m.id == window.monitor)
            .unwrap();
        let stack = stack(vec![window]);
        let mut desktop = Desktop::connect().unwrap();
        for pixels in [None, Some(Detector::Fast), Some(Detector::Full)] {
            for enabled in [true, false] {
                let (snapshot, unread) =
                    desktop.scan(&stack, &monitor, pixels, enabled, false, &["kitty".into()]);
                assert!(
                    unread.is_empty(),
                    "{:?}",
                    unread
                        .iter()
                        .map(|u| u.error.to_string())
                        .collect::<Vec<_>>()
                );
                assert_eq!(snapshot.targets.len(), usize::from(enabled));
                assert!(snapshot.windows.iter().any(|s| is_kitty(&s.window)));
                assert!(desktop.camera.is_none(), "cursor hints must not capture");
                if enabled {
                    let target = &snapshot.targets[0];
                    let window = &snapshot.windows[target.window].window;
                    let cell = kitty::cursor(window, KITTY_TIMEOUT)
                        .unwrap()
                        .offset(window.rect.origin());
                    assert_eq!(target.rect, cell);
                    assert_eq!(target.hit, cell.center());
                    assert_eq!(target.role, "terminal");
                    assert!(!target.in_pixels());
                    assert!(target.editable && target.opens.is_empty());
                }
            }
        }
        let window = &stack.windows[0];
        assert!(
            !kitty::read(window, KITTY_TIMEOUT).unwrap().is_empty(),
            "the shared helper must still return text panes"
        );
        let dump = read_pixels(window, Detector::Full, &["kitty".into()]).unwrap();
        assert_eq!(dump.targets.len(), 1);
        assert_eq!(dump.targets[0].role, "terminal");
        assert_eq!(
            dump.targets[0].rect,
            kitty::cursor(window, KITTY_TIMEOUT)
                .unwrap()
                .offset(window.rect.origin())
        );
    }

    #[test]
    fn a_frame_mapped_since_the_scan_is_resolved_again_before_a_click() {
        let id = |path: &str| tree::ObjectId {
            bus: ":1.9".into(),
            path: path.into(),
        };
        let layer = |frames: &[&str]| Window {
            kind: Kind::Layer { level: 3 },
            frames: frames.iter().map(|&f| id(f)).collect(),
            ..window("layer", Rect::new(0, 0, 1000, 1000))
        };
        let scanned = layer(&["/old"]);
        let unasked = |_: &Window| -> Option<Vec<tree::ObjectId>> { panic!("one frame is plain") };
        assert!(same_frame(&scanned, &layer(&["/old"]), unasked));
        assert!(!same_frame(&scanned, &layer(&["/new"]), unasked));
        let now = layer(&["/old", "/new"]);
        assert!(!same_frame(&scanned, &now, |_| None), "both have controls");
        assert!(!same_frame(&scanned, &now, |_| Some(vec![id("/new")])));
        assert!(same_frame(&scanned, &now, |_| Some(vec![id("/old")])));
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

    const SCREEN: Rect = Rect::new(0, 0, 3840, 2160);

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
        focus_body(
            &mut target,
            origin,
            Some((&picture.0, picture.1)),
            SCREEN,
            &[],
        );
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
            focus_body(&mut target, origin, Some((&cells, 1.0)), SCREEN, &[]);
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
        focus_body(&mut target, Point::new(0, 0), None, SCREEN, &[]);
        assert_eq!(target.visible, Rect::new(182, 138, 36, 24));
        assert_eq!(target.hit, Point::new(200, 150));
    }

    #[test]
    fn a_body_tag_stays_clear_of_a_window_above() {
        // A busy page, blank only at its top left and bottom right. The
        // top left is nearer the middle, but a window covers it.
        let body = Rect::new(0, 0, 400, 300);
        let cover = Rect::new(0, 0, 120, 120);
        let ink = [Rect::new(120, 0, 280, 200), Rect::new(0, 120, 300, 180)];
        let picture = Space::rectangles(body, &ink);
        let mut target = sheet(body);
        focus_body(
            &mut target,
            Point::new(0, 0),
            Some((&picture, 1.0)),
            SCREEN,
            &[],
        );
        assert!(cover.contains(target.hit), "uncovered, the top left wins");
        let mut target = sheet(body);
        focus_body(
            &mut target,
            Point::new(0, 0),
            Some((&picture, 1.0)),
            SCREEN,
            &[cover],
        );
        assert!(
            target.visible.intersect(cover).is_none(),
            "{:?}",
            target.visible
        );
        assert_eq!(picture.ink(target.visible), 0, "on the blank bottom right");

        // Blank only where it's covered (the audit's case): a busy patch
        // that shows beats a blank one under the window.
        let ink = [Rect::new(0, 120, 400, 180), Rect::new(120, 0, 280, 120)];
        let picture = Space::rectangles(body, &ink);
        let mut target = sheet(body);
        focus_body(
            &mut target,
            Point::new(0, 0),
            Some((&picture, 1.0)),
            SCREEN,
            &[cover],
        );
        assert!(!cover.contains(target.hit), "{:?}", target.hit);
    }

    #[test]
    fn an_unreadable_backdrop_blocks_only_the_gaps_between_windows() {
        // Quickshell's backdrop: a background layer the size of the
        // monitor, with no picture. Blocked whole, no tag could move.
        let monitor = Rect::new(0, 0, 2560, 1440);
        let windows = [Rect::new(8, 38, 1790, 1394), Rect::new(1806, 38, 746, 1394)];
        let blocked = in_view([monitor], &windows);
        let space = Space::rectangles(monitor, &blocked);
        for window in windows {
            assert_eq!(space.ink(window.inset(4)), 0, "{window:?}");
        }
        assert!(
            space.ink(Rect::new(0, 0, 2560, 38)) > 0,
            "the bar's strip shows it"
        );
        assert!(
            space.ink(Rect::new(1798, 38, 8, 100)) > 0,
            "so does the gap"
        );
    }

    #[test]
    fn a_long_name_is_matched_on_its_object() {
        use crate::{desktop::tree::ObjectId, snapshot::TextSource, text::Search};

        let mut snapshot = Snapshot::build(1, 0, std::iter::empty());
        snapshot.texts.push(TextSource {
            object: ObjectId {
                bus: ":1.42".into(),
                path: "/link".into(),
            },
            window: 0,
            role: "link".into(),
            editable: false,
            named: true,
            area: Rect::new(0, 0, 1000, 1000),
            clips: Vec::new(),
            owner: None,
            button: None,
            menu: None,
        });
        let name = format!("{} contact us", "read more about it ".repeat(6));
        assert!(name.chars().count() > 100, "longer than a label");
        let rect = Rect::new(10, 20, 600, 18);
        let mut search = Search::new(vec![Text {
            place: Place::Object(0),
            window: 0,
            name: name.clone(),
            rect,
            area: Rect::new(0, 0, 1000, 1000),
            start: 0,
            content: name,
        }]);
        "contact".chars().for_each(|c| search.push(c));
        let wanted = search.wanted();
        let boxes = wanted
            .iter()
            .map(|span| placed(&snapshot, &search.texts()[span.text], span, Vec::new()))
            .collect();
        search.located(&wanted, boxes);
        search.update(&|_, _| true);
        assert_eq!(search.matches().len(), 1, "the match keeps the link's box");
        assert_eq!(search.matches()[0].parts, [rect]);
    }
}
