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
mod ocr;
pub mod recheck;
pub mod surfaces;
pub mod tree;

use std::{
    collections::BTreeSet,
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
        Kind, Monitor, Window, changes_workspace, cursor, events, focused_window, monitors,
        window_by_address,
    },
    kitty::is_kitty,
    notify::warn,
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

/// How long a window's picture may take, connecting included: the
/// spike's took 6 to 12 ms.
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long reading text for text mode may take, every window together.
const TEXT_TIMEOUT: Duration = Duration::from_secs(1);

/// How long finding where a query's new matches are may take.
const LOCATE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long kitty may take to say what one window shows: one request,
/// answered in a few ms.
const KITTY_TIMEOUT: Duration = Duration::from_millis(500);

/// How long OCR may take, every window together: a full-screen sheet took
/// 1.9 s (`ocr`).
const OCR_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// The pictures text mode's OCR last read, by window address: a match
    /// must still show the same pixels before it's acted on.
    pictures: Vec<(String, Frame)>,
    /// The apps' open menus, once a run starts following them.
    menus: Option<Menus>,
}

/// One window as the scan saw it, until the snapshot is built from it.
enum Seen {
    Tree(tree::Tree, filter::Filtered, Stats),
    /// With its picture, shrunk, and the picture's pixels per logical
    /// pixel: `layout` reads the same one.
    Pixels(Window, Found, (Frame, f64), Stats),
    Untagged(Window),
}

impl Seen {
    fn address(&self) -> &str {
        match self {
            Self::Tree(tree, ..) => &tree.window.address,
            Self::Pixels(window, ..) | Self::Untagged(window) => &window.address,
        }
    }

    fn read(&self) -> Read<'_> {
        match self {
            Self::Tree(tree, filtered, stats) => Read::Tree(tree, filtered, stats),
            Self::Pixels(window, found, (_, scale), stats) => Read::Pixels {
                window,
                found,
                scale: *scale,
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
            pictures: Vec::new(),
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
    /// pixels with `pixels`, or gets no tags when that's `None`; a layer or
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
        terminals: &[String],
    ) -> (Snapshot, Vec<Unread>) {
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
        let mut failed: Vec<_> = stack
            .unplaced
            .iter()
            .map(|u| Unread {
                app: None,
                error: anyhow!("{u}"),
            })
            .collect();
        for (window, read) in readable.into_iter().zip(reads) {
            let detector = pixels.filter(|_| window.kind == Kind::Client);
            match (read, detector) {
                (Ok((tree, stats)), _) => {
                    let filtered = filter::filter(&tree);
                    seen.push(Seen::Tree(tree, filtered, stats));
                }
                (Err(e), None) => failed.push(Unread::new(window, e)),
                (Err(no_tree), Some(detector)) => {
                    match look(&mut self.camera, window, detector, terminals) {
                        Ok((found, picture, stats)) => {
                            seen.push(Seen::Pixels(window.clone(), found, picture, stats));
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
            .filter(|w| w.kind != Kind::Popup && is_kitty(w))
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
        let pictures: Vec<_> = seen
            .iter()
            .filter_map(|s| match s {
                Seen::Pixels(window, _, picture, _) => Some((window.address.as_str(), picture)),
                Seen::Tree(..) | Seen::Untagged(_) => None,
            })
            .collect();
        self.layout(&mut snapshot, monitor, stack, &pictures);
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
        (snapshot, failed)
    }

    /// Capture what labels may cover separately from target detection. An
    /// unreadable window stays fully protected instead of guessing it is blank.
    /// The same picture puts each document body's tag in its clearest spot
    /// that no surface in `stack` may cover. A client in `pictures` (by
    /// address, shrunk, with its scale) was taken this scan already.
    pub fn layout(
        &mut self,
        snapshot: &mut Snapshot,
        monitor: &Monitor,
        stack: &Stack,
        pictures: &[(&str, &(Frame, f64))],
    ) {
        let mut obstacles = Vec::new();
        // Text sources kept: a button only where its picture shows letters.
        let mut keep: Vec<bool> = snapshot.texts.iter().map(|t| t.button.is_none()).collect();
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
                let taken = pictures.iter().find(|(a, _)| *a == window.address);
                let shot;
                let (frame, scale) = if let Some((_, (frame, scale))) = taken {
                    (frame, *scale)
                } else {
                    shot = pixels::shrink(self.shoot(&window.address)?, window.rect.w);
                    (&shot.0, shot.1)
                };
                let cells = bodies.then(|| pixels::cell_space(frame));
                for (i, text) in snapshot.texts.iter().enumerate() {
                    if let Some(button) = text.button.filter(|_| text.window == index) {
                        let r = button.rect;
                        #[allow(clippy::cast_possible_truncation)]
                        let at = |v: i32, from: i32| (f64::from(v - from) * scale).round() as i32;
                        let (x, y) = (window.rect.x, window.rect.y);
                        let corners = [at(r.x, x), at(r.y, y), at(r.right(), x), at(r.bottom(), y)];
                        keep[i] = pixels::shows_letters(frame, corners, button.letters);
                    }
                }
                Ok((Space::pixels(frame), cells, scale))
            })();
            let origin = window.rect.origin();
            // What a window shows only blocks tags where it's on top: an
            // unreadable background layer would otherwise block them all.
            let covers: Vec<Rect> = above(stack, &window.address).map(|w| w.rect).collect();
            match &captured {
                #[allow(clippy::cast_possible_truncation)]
                Ok((space, _, scale)) => {
                    let ink = space.obstacles().into_iter().map(|r| {
                        let x = (f64::from(r.x) / scale).floor() as i32;
                        let y = (f64::from(r.y) / scale).floor() as i32;
                        let right = (f64::from(r.right()) / scale).ceil() as i32;
                        let bottom = (f64::from(r.bottom()) / scale).ceil() as i32;
                        Rect::new(x, y, right - x, bottom - y).offset(origin)
                    });
                    obstacles.extend(in_view(ink, &covers));
                }
                Err(_) => obstacles.extend(in_view([window.rect], &covers)),
            }
            let cells = captured
                .as_ref()
                .ok()
                .and_then(|(_, cells, scale)| Some((cells.as_ref()?, *scale)));
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
    /// of every window read from pixels, for text mode: kitty's panes from
    /// kitty, the rest by OCR in `ocr`'s languages, unless that's `None`.
    /// OCR starts first: Tesseract reads while the apps are asked.
    pub fn read_text(&mut self, snapshot: &Snapshot, ocr: Option<&str>) -> Reading {
        let mut others = Vec::new();
        let mut unread = Vec::new();
        let mut jobs = Vec::new();
        self.pictures.clear();
        let untreed = snapshot.windows.iter().enumerate();
        for (index, scanned) in untreed.filter(|(_, s)| s.pixels || is_kitty(&s.window)) {
            let window = &scanned.window;
            let mut kitty = None;
            if is_kitty(window) {
                match kitty::read(window, KITTY_TIMEOUT) {
                    Ok(panes) => {
                        others.extend(panes.into_iter().map(|pane| Text {
                            rect: pane.cells.whole(),
                            place: Place::Cells(pane.cells),
                            window: index,
                            name: pane.title,
                            area: window.rect,
                            start: 0,
                            content: pane.content,
                        }));
                        continue;
                    }
                    Err(e) => kitty = Some(e),
                }
            }
            let Some(languages) = ocr else {
                unread.extend(kitty.map(|e| Unread::new(window, e.context(window.class.clone()))));
                continue;
            };
            let started = self.shoot(&window.address).and_then(|frame| {
                let job = ocr::start(&frame, window, languages)?;
                Ok((job, frame))
            });
            match (started, kitty) {
                (Ok((job, frame)), _) => {
                    jobs.push((index, job));
                    self.pictures.push((window.address.clone(), frame));
                }
                (Err(e), None) => {
                    unread.push(Unread::new(window, e.context(window.class.clone())));
                }
                (Err(e), Some(kitty)) => {
                    unread.push(Unread::new(
                        window,
                        anyhow!("{}: {kitty:#}, and {e:#}", window.class),
                    ));
                }
            }
        }
        let (mut texts, incomplete) = self.read_objects(snapshot);
        texts.append(&mut others);
        let until = Instant::now() + OCR_TIMEOUT;
        for (index, job) in jobs {
            let window = &snapshot.windows[index].window;
            match job.finish(until) {
                Ok((content, glyphs)) => texts.push(Text {
                    rect: glyphs.whole(),
                    place: Place::Glyphs(glyphs),
                    window: index,
                    name: window.title.clone(),
                    area: window.rect,
                    start: 0,
                    content,
                }),
                Err(e) => unread.push(Unread::new(window, e.context(window.class.clone()))),
            }
        }
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

    /// The text of every object in `snapshot.texts` that shows some, and
    /// why the reading stopped early, if it did.
    fn read_objects(&self, snapshot: &Snapshot) -> (Vec<Text>, Option<String>) {
        let origin = |window: usize| snapshot.windows[window].window.rect.origin();
        let objects: Vec<(&ObjectId, Rect, bool)> = snapshot
            .texts
            .iter()
            .map(|source| {
                let at = origin(source.window);
                let area = source.area.offset(Point::new(-at.x, -at.y));
                (&source.object, area, source.named)
            })
            .collect();
        let read = self.scanner.read_texts(&objects, TEXT_TIMEOUT);
        let (reads, incomplete) = self.runtime.block_on(read);
        let texts = reads
            .into_iter()
            .zip(snapshot.texts.iter().enumerate())
            .filter_map(|(read, (index, source))| {
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
        (texts, incomplete)
    }

    /// Where each of `spans` of `texts` (read from `snapshot`) is on
    /// screen: a global box per line, none if the app can't say. A text
    /// that is a name has no characters to ask about; a terminal's isn't
    /// asked, its cells say, nor is OCR's, its glyphs say.
    pub fn locate(&self, snapshot: &Snapshot, texts: &[Text], spans: &[Span]) -> Vec<Vec<Rect>> {
        let asks: Vec<Option<(&ObjectId, u32, u32)>> = spans
            .iter()
            .map(|s| match texts[s.text].place {
                Place::Object(index) => {
                    let source = &snapshot.texts[index];
                    (!source.named).then_some((&source.object, s.start, s.end))
                }
                Place::Cells(_) | Place::Glyphs(_) => None,
            })
            .collect();
        let boxes = self
            .runtime
            .block_on(self.scanner.locate(&asks, LOCATE_TIMEOUT));
        boxes
            .into_iter()
            .zip(spans)
            .map(|(parts, span)| {
                let text = &texts[span.text];
                match &text.place {
                    Place::Cells(cells) => return cells.boxes(span.start, span.end),
                    Place::Glyphs(glyphs) => return glyphs.boxes(span.start, span.end),
                    Place::Object(_) => {}
                }
                let origin = snapshot.windows[text.window].window.rect.origin();
                parts.into_iter().map(|r| r.offset(origin)).collect()
            })
            .collect()
    }

    /// Whether a text in `snapshot`'s window `window` still reads what
    /// `check` says, and if not, why: the last look before acting on a
    /// match. A terminal's must also still be in the same cells; OCR's
    /// can't be read again in time, so its pixels must be the same.
    pub fn still_reads(&mut self, snapshot: &Snapshot, window: usize, check: &Check) -> Result<()> {
        let Check {
            place,
            start,
            end,
            text,
        } = check;
        let (start, end) = (*start, *end);
        let now: String = match place {
            Place::Object(index) => {
                let object = &snapshot.texts[*index].object;
                let read = self.scanner.read_range(object, start, end, RECHECK_TIMEOUT);
                self.runtime.block_on(read)?
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
            Place::Glyphs(glyphs) => {
                let address = &snapshot.windows[window].window.address;
                let now = self.shoot(address)?;
                let (_, then) = self
                    .pictures
                    .iter()
                    .find(|(a, _)| a == address)
                    .context("its picture is gone")?;
                let corners = glyphs.corners(start, end);
                ensure!(ocr::unchanged(then, &now, &corners), "the picture changed");
                return Ok(());
            }
        };
        ensure!(now == *text, "the text changed");
        Ok(())
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

/// `window` tagged from its pixels alone: what `dump --pixels` prints.
pub fn read_pixels(window: &Window, detector: Detector, terminals: &[String]) -> Result<Snapshot> {
    let (found, (_, scale), stats) = look(&mut None, window, detector, terminals)?;
    let read = Read::Pixels {
        window,
        found: &found,
        scale,
        stats: &stats,
    };
    Ok(Snapshot::build(1, window.monitor, [read]))
}

/// Takes a picture of `window` and finds its tags, with the `camera`
/// made on first use. Also returns the picture, shrunk, with its pixels
/// per logical pixel, and what it cost.
fn look(
    camera: &mut Option<Camera>,
    window: &Window,
    detector: Detector,
    terminals: &[String],
) -> Result<(Found, (Frame, f64), Stats)> {
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
}
