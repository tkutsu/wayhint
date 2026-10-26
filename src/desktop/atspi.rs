//! Reading the accessibility tree over AT-SPI (D-Bus), asynchronously.
//!
//! The scan, for one window:
//!
//! 1. Find the window's app: the registry child whose bus connection has
//!    the window's PID.
//! 2. Find the window's frame among the app's children: by title, else (for
//!    the focused window only) the one marked active. Two candidates is
//!    ambiguous: refuse, never guess. A layer's or popup's frame is known
//!    before the scan (see `surfaces::place`), so it skips 1 and 2.
//! 3. Walk the frame breadth first, one level at a time, at most
//!    [`IN_FLIGHT`] requests at once, reading role, states and interfaces.
//! 4. For candidates only (see `filter::is_candidate`), read name, box and
//!    action names. For scroll areas above them, read their box.
//!
//! Every request races one deadline and the walk has a node budget. Hitting
//! either marks the scan incomplete, with the reason, instead of returning a
//! silent partial result.
//!
//! Why async at all: a window has hundreds of objects and each answer is a
//! D-Bus round trip. Asking eight nodes at a time, on one thread, scans
//! Dolphin (137 nodes, 828 requests) in 8 ms instead of 23 ms with one at a
//! time (measured 2026-10-22), and `async` is how Rust waits on several
//! answers without a thread per request.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    future::Future,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use atspi::{
    AccessibilityConnection, CoordType, Granularity, Interface, ObjectRefOwned, State, StateSet,
    proxy::{
        accessible::AccessibleProxy, action::ActionProxy, application::ApplicationProxy,
        component::ComponentProxy, text::TextProxy,
    },
};
use futures::{StreamExt as _, future, stream};
use serde::Serialize;
use zbus::proxy::CacheProperties;

use crate::{
    desktop::{
        Window,
        filter::{self, DOCUMENT_ROLES},
        recheck::Fresh,
        surfaces::Frame,
        tree::{Details, Node, ObjectId, States, Tree},
    },
    geometry::{Point, Rect},
};

/// Requests in flight at once. More doesn't help: apps answer one at a time.
const IN_FLIGHT: usize = 8;

/// The scan's bounds. `Esc` can't interrupt a scan, so these are the longest
/// you ever wait.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub time: Duration,
    pub nodes: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            time: Duration::from_secs(1),
            nodes: 5_000,
        }
    }
}

/// What a scan cost, written into every snapshot.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Stats {
    pub ms: f64,
    pub nodes: usize,
    pub requests: usize,
    /// Why the scan stopped early, if it did.
    pub incomplete: Option<String>,
}

/// An open connection to the accessibility bus.
pub struct Scanner {
    a11y: AccessibilityConnection,
}

impl Scanner {
    /// Connects to the accessibility bus (found through the session bus's
    /// `org.a11y.Bus`).
    pub async fn connect() -> Result<Self> {
        let a11y = AccessibilityConnection::new()
            .await
            .context("connecting to the accessibility bus (is at-spi-bus-launcher running?)")?;
        Ok(Self { a11y })
    }

    /// Reads one client's tree within `bounds`, from the frame with its
    /// title. Its errors start with the window's class, since a run reads
    /// several at once.
    pub async fn scan(&self, window: &Window, bounds: Bounds) -> Result<(Tree, Stats)> {
        let started = Instant::now();
        let walk = Walk::new(self.a11y.connection(), started + bounds.time);
        let frame = walk
            .find_frame(&self.a11y, window)
            .await
            .with_context(|| window.class.clone())?;
        Ok(walk.scan(window, frame, bounds, started).await)
    }

    /// Reads the tree under `frame`, a frame already known to be `window`'s
    /// (a layer's or a popup's, see `surfaces::place`).
    pub async fn scan_frame(
        &self,
        window: &Window,
        frame: &ObjectId,
        bounds: Bounds,
    ) -> (Tree, Stats) {
        let started = Instant::now();
        let walk = Walk::new(self.a11y.connection(), started + bounds.time);
        walk.scan(window, frame.clone(), bounds, started).await
    }

    /// The top-level frames that are showing, of each app whose process is
    /// in `pids`, with their boxes in screen coordinates. An app that isn't
    /// on the accessibility bus has none; one that fails to answer, an
    /// error.
    pub async fn frames(
        &self,
        pids: &BTreeSet<u32>,
        timeout: Duration,
    ) -> Vec<(u32, Result<Vec<Frame>>)> {
        let walk = Walk::new(self.a11y.connection(), Instant::now() + timeout);
        let mut all = Vec::new();
        for &pid in pids {
            let frames = async {
                let apps = walk.find_apps(&self.a11y, pid).await?;
                walk.frames(&apps).await
            };
            all.push((pid, frames.await));
        }
        all
    }

    /// Asks one object again: role, name, states and box, and the boxes of
    /// the scroll areas in `clips`, in one bounded go.
    pub async fn read(
        &self,
        object: &ObjectId,
        clips: &[ObjectId],
        timeout: Duration,
    ) -> Result<Fresh> {
        let walk = Walk::new(self.a11y.connection(), Instant::now() + timeout);
        walk.read_fresh(object, clips).await
    }

    /// Reads the text of `objects`, each with the part of its window it
    /// can show in and whether its name is its text, eight at a time, all
    /// within `timeout`. `None` for one that didn't answer, has no text,
    /// or shows none of it. Also says why the reading stopped early, if it
    /// did.
    pub async fn read_texts(
        &self,
        objects: &[(&ObjectId, Rect, bool)],
        timeout: Duration,
    ) -> (Vec<Option<TextRead>>, Option<String>) {
        let walk = Walk::new(self.a11y.connection(), Instant::now() + timeout);
        let reads = stream::iter(objects)
            .map(|&(object, area, named)| {
                let walk = &walk;
                async move { walk.read_text(object, area, named).await.ok().flatten() }
            })
            .buffered(IN_FLIGHT)
            .collect()
            .await;
        (reads, walk.incomplete.take())
    }

    /// Where each of `spans` (characters `start..end` of an object) is in
    /// its window: a box per line, none if the app can't say, the time ran
    /// out, or there was nothing to ask.
    pub async fn locate(
        &self,
        spans: &[Option<(&ObjectId, u32, u32)>],
        timeout: Duration,
    ) -> Vec<Vec<Rect>> {
        let walk = Walk::new(self.a11y.connection(), Instant::now() + timeout);
        stream::iter(spans)
            .map(|&span| {
                let walk = &walk;
                async move {
                    match span {
                        Some((object, start, end)) => walk.locate(object, start, end).await,
                        None => Vec::new(),
                    }
                }
            })
            .buffered(IN_FLIGHT)
            .collect()
            .await
    }

    /// Characters `start..end` of an object's text, read again.
    pub async fn read_range(
        &self,
        object: &ObjectId,
        start: u32,
        end: u32,
        timeout: Duration,
    ) -> Result<String> {
        let walk = Walk::new(self.a11y.connection(), Instant::now() + timeout);
        let text: TextProxy = walk.proxy(object).await?;
        walk.call(text.get_text(offset(start), offset(end))).await
    }
}

/// A text this long is read only from where its visible part starts, and
/// at most this much of it: an editor's whole file isn't on screen.
const LONG_TEXT: i32 = 8_192;

/// The most lines one match is split into, wrapped.
const MAX_LINES: usize = 8;

/// One object's text as text mode reads it. Window-relative.
pub struct TextRead {
    pub name: String,
    pub bounds: Rect,
    /// The character offset `content` starts at.
    pub start: u32,
    pub content: String,
}

/// A character offset as AT-SPI takes it.
fn offset(at: u32) -> i32 {
    i32::try_from(at).unwrap_or(i32::MAX)
}

/// One scan's working state. `Cell` and `RefCell` let the many concurrent
/// requests (all borrowing `&Walk`) bump the counters without `&mut`: they
/// all run on this one thread, so no lock is needed.
struct Walk<'c> {
    conn: &'c zbus::Connection,
    deadline: Instant,
    requests: Cell<usize>,
    incomplete: RefCell<Option<String>>,
}

/// A node waiting to be read, with what the walk knows from its parent.
struct Pending {
    object: ObjectId,
    parent: Option<usize>,
    in_document: bool,
}

impl<'c> Walk<'c> {
    fn new(conn: &'c zbus::Connection, deadline: Instant) -> Self {
        Self {
            conn,
            deadline,
            requests: Cell::new(0),
            incomplete: RefCell::new(None),
        }
    }

    /// Records the first reason the scan is incomplete; later ones add nothing.
    fn mark_incomplete(&self, why: impl Into<String>) {
        self.incomplete
            .borrow_mut()
            .get_or_insert_with(|| why.into());
    }

    /// Runs one D-Bus call against the scan's deadline and counts it.
    async fn call<T, E>(&self, request: impl Future<Output = Result<T, E>>) -> Result<T>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.requests.set(self.requests.get() + 1);
        let deadline = tokio::time::Instant::from_std(self.deadline);
        // `timeout_at` wraps the call's own `Result` in another one: the
        // outer `Err` means the deadline passed first.
        let Ok(result) = tokio::time::timeout_at(deadline, request).await else {
            self.mark_incomplete("time limit");
            bail!("time limit");
        };
        Ok(result?)
    }

    /// Builds a proxy of any AT-SPI interface for one object. Generic over
    /// `P`: the same code makes an `AccessibleProxy`, a `ComponentProxy` or
    /// an `ActionProxy`, whichever the caller's type asks for. Property
    /// caching is off, so no proxy fetches anything it wasn't asked for.
    async fn proxy<P>(&self, object: &ObjectId) -> Result<P>
    where
        P: From<zbus::Proxy<'static>> + zbus::proxy::Defaults,
    {
        Ok(zbus::proxy::Builder::<P>::new(self.conn)
            .destination(object.bus.clone())?
            .path(object.path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?)
    }

    /// Step 1: the apps of process `pid`; none if it isn't on the bus. Most
    /// processes have one, but Quickshell registers a second, empty one
    /// before the one with its frames (2026-10-24).
    async fn find_apps(&self, a11y: &AccessibilityConnection, pid: u32) -> Result<Vec<ObjectId>> {
        let registry = a11y.root_accessible_on_registry().await?;
        let apps = self.call(registry.get_children()).await?;
        let dbus = zbus::fdo::DBusProxy::new(self.conn).await?;
        let mut found = Vec::new();
        for candidate in &apps {
            let Some(bus) = candidate.name() else {
                continue;
            };
            // Apps registered with a dead socket (Steam) fail here; skip them.
            if self
                .call(dbus.get_connection_unix_process_id(bus.clone().into()))
                .await
                .ok()
                == Some(pid)
            {
                found.push(object_id(candidate));
            }
        }
        // A skipped app may have been this one, if the time ran out.
        if let Some(why) = self.incomplete.borrow().as_ref() {
            bail!("looking for pid {pid}: {why}");
        }
        Ok(found)
    }

    /// When the bus started: its registry starts with it.
    async fn bus_started(&self) -> Option<u64> {
        let dbus = zbus::fdo::DBusProxy::new(self.conn).await.ok()?;
        let registry = "org.a11y.atspi.Registry".try_into().ok()?;
        let pid = self
            .call(dbus.get_connection_unix_process_id(registry))
            .await
            .ok()?;
        started(pid)
    }

    /// The showing top-level frames of `apps`, boxed in screen coordinates.
    async fn frames(&self, apps: &[ObjectId]) -> Result<Vec<Frame>> {
        let mut frames = Vec::new();
        for app in apps {
            frames.extend(self.frames_of(app).await?);
        }
        Ok(frames)
    }

    async fn frames_of(&self, app: &ObjectId) -> Result<Vec<Frame>> {
        let app: AccessibleProxy = self.proxy(app).await?;
        let mut frames = Vec::new();
        for child in self.call(app.get_children()).await? {
            let object = object_id(&child);
            let accessible: AccessibleProxy = self.proxy(&object).await?;
            if !self
                .call(accessible.get_state())
                .await?
                .contains(State::Showing)
            {
                continue;
            }
            let component: ComponentProxy = self.proxy(&object).await?;
            let (x, y, w, h) = self.call(component.get_extents(CoordType::Screen)).await?;
            frames.push(Frame {
                object,
                rect: Rect::new(x, y, w, h),
            });
        }
        Ok(frames)
    }

    /// Steps 1 and 2: the frame of `window` in the tree.
    async fn find_frame(
        &self,
        a11y: &AccessibilityConnection,
        window: &Window,
    ) -> Result<ObjectId> {
        let apps = self.find_apps(a11y, window.pid).await?;
        if apps.is_empty() {
            // Qt apps don't rejoin a restarted bus (Qt 6.12), and Chromium and
            // Electron apps don't join without the flag.
            let bus = self.bus_started().await;
            if started(window.pid)
                .zip(bus)
                .is_some_and(|(app, bus)| app < bus)
            {
                bail!(
                    "pid {} isn't on the accessibility bus, which restarted after it started: \
                     reopen it so wayhint can see its controls",
                    window.pid
                );
            }
            bail!(
                "pid {} isn't on the accessibility bus, so wayhint can't see its controls \
                 (Chromium and Electron apps join it with --force-renderer-accessibility)",
                window.pid
            );
        }

        let mut frames = Vec::new();
        for app in &apps {
            let app: AccessibleProxy = self.proxy(app).await?;
            for frame in self.call(app.get_children()).await? {
                let id = object_id(&frame);
                let proxy: AccessibleProxy = self.proxy(&id).await?;
                let name = self.call(proxy.name()).await.unwrap_or_default();
                let states = self.call(proxy.get_state()).await.unwrap_or_default();
                frames.push((id, name, states.contains(State::Active)));
            }
        }
        pick_frame(&frames, &window.title, window.focused)
            .map(|(id, _, _)| id.clone())
            .with_context(|| {
                format!(
                    "{} windows and none is clearly {:?}",
                    frames.len(),
                    window.title
                )
            })
    }

    /// Steps 3 and 4, from `frame`.
    async fn scan(
        &self,
        window: &Window,
        frame: ObjectId,
        bounds: Bounds,
        started: Instant,
    ) -> (Tree, Stats) {
        let nodes = self.read_tree(frame, window.rect, bounds.nodes).await;
        let mut tree = Tree {
            window: window.clone(),
            nodes,
        };
        self.read_candidates(&mut tree).await;
        self.read_scroll_areas(&mut tree).await;
        let stats = Stats {
            ms: millis(started.elapsed()),
            nodes: tree.nodes.len(),
            requests: self.requests.get(),
            incomplete: self.incomplete.take(),
        };
        (tree, stats)
    }

    /// Step 3: breadth-first walk from the frame.
    async fn read_tree(&self, frame: ObjectId, window: Rect, budget: usize) -> Vec<Node> {
        let mut nodes = Vec::new();
        let mut level = vec![Pending {
            object: frame,
            parent: None,
            in_document: false,
        }];
        while !level.is_empty() {
            if nodes.len() + level.len() > budget {
                self.mark_incomplete(format!("node budget ({budget})"));
                level.truncate(budget.saturating_sub(nodes.len()));
            }
            // `buffered` keeps the results in order, so the same window
            // gives the same node order every time.
            let results: Vec<_> = stream::iter(level.drain(..))
                .map(|pending| self.read_node(pending, window))
                .buffered(IN_FLIGHT)
                .collect()
                .await;
            let mut next = Vec::new();
            for result in results {
                // An object that vanished mid-walk is normal (a tooltip
                // closing); it's left out, and its children with it.
                let Ok((node, children, in_document)) = result else {
                    continue;
                };
                let parent = Some(nodes.len());
                next.extend(children.into_iter().map(|object| Pending {
                    object,
                    parent,
                    in_document,
                }));
                nodes.push(node);
            }
            level = next;
            if Instant::now() >= self.deadline {
                self.mark_incomplete("time limit");
                break;
            }
        }
        nodes
    }

    /// One node: role, states and interfaces (three requests at once), its
    /// box when the walk rule needs it, and its children if worth entering.
    async fn read_node(
        &self,
        pending: Pending,
        window: Rect,
    ) -> Result<(Node, Vec<ObjectId>, bool)> {
        let proxy: AccessibleProxy = self.proxy(&pending.object).await?;
        let (role, states, interfaces) = tokio::join!(
            self.call(proxy.get_role()),
            self.call(proxy.get_state()),
            self.call(proxy.get_interfaces())
        );
        let role = match role {
            Ok(role) => role.name().to_string(),
            // atspi 0.30's Role stops at 129; GTK4's switch is 130. Ask for
            // the name instead.
            Err(_) => self.call(proxy.get_role_name()).await?,
        };
        let (states, interfaces) = (states_from(states?), interfaces?);
        let component = interfaces.contains(Interface::Component);

        let document = DOCUMENT_ROLES.contains(&role.as_str());
        let needs_box = pending.in_document && !states.showing && states.visible && component;
        let bounds = if needs_box {
            Some(self.extents(&pending.object).await?)
        } else {
            None
        };
        let children = if worth_entering(states, document, pending.in_document, bounds, window) {
            self.call(proxy.get_children())
                .await?
                .iter()
                .map(object_id)
                .collect()
        } else {
            Vec::new()
        };
        let node = Node {
            object: pending.object,
            parent: pending.parent,
            role,
            states,
            action: interfaces.contains(Interface::Action),
            component,
            text: interfaces.contains(Interface::Text),
            bounds,
            details: None,
        };
        Ok((node, children, pending.in_document || document))
    }

    async fn extents(&self, object: &ObjectId) -> Result<Rect> {
        self.extents_in(object, CoordType::Window).await
    }

    async fn extents_in(&self, object: &ObjectId, coords: CoordType) -> Result<Rect> {
        let component: ComponentProxy = self.proxy(object).await?;
        let (x, y, w, h) = self.call(component.get_extents(coords)).await?;
        Ok(Rect::new(x, y, w, h))
    }

    /// The coordinates to ask an object's text in, and how far to move
    /// what comes back to make it window-relative.
    ///
    /// Window coordinates, except in Chromium (Electron too): its text in
    /// window coordinates is measured from the page's corner, not the
    /// window's (`kRootFrame`), so a match there lands a toolbar's height
    /// off. Its screen coordinates are one frame for text and boxes alike,
    /// so the object's own box says how far screen is from window. Not for
    /// every app: GTK4 has no screen coordinates for text.
    async fn text_frame(&self, object: &ObjectId) -> (CoordType, Point) {
        let window = (CoordType::Window, Point::default());
        let root = ObjectId {
            bus: object.bus.clone(),
            path: "/org/a11y/atspi/accessible/root".into(),
        };
        let Ok(app) = self.proxy::<ApplicationProxy>(&root).await else {
            return window;
        };
        if self.call(app.toolkit_name()).await.ok().as_deref() != Some("Chromium") {
            return window;
        }
        let (on_window, on_screen) = tokio::join!(
            self.extents_in(object, CoordType::Window),
            self.extents_in(object, CoordType::Screen)
        );
        match (on_window, on_screen) {
            (Ok(w), Ok(s)) => (CoordType::Screen, Point::new(w.x - s.x, w.y - s.y)),
            _ => window,
        }
    }

    /// Name, box and action names of one candidate.
    async fn details(&self, node: &Node) -> Result<(Details, Rect)> {
        let accessible: AccessibleProxy = self.proxy(&node.object).await?;
        let name = self.call(accessible.name());
        let bounds = self.extents(&node.object);
        let actions = async {
            if !node.action {
                return Ok(Vec::new());
            }
            // `GetActions` comes back empty or garbled through atspi 0.30
            // (Chromium, Firefox); one `GetName(i)` per action is reliable.
            let action: ActionProxy = self.proxy(&node.object).await?;
            let count = self.call(action.n_actions()).await?;
            let mut names = Vec::new();
            for i in 0..count {
                names.push(self.call(action.get_name(i)).await?);
            }
            Ok::<_, anyhow::Error>(names)
        };
        let (name, bounds, actions) = tokio::try_join!(name, bounds, actions)?;
        Ok((Details { name, actions }, bounds))
    }

    /// Step 4a: details for every candidate, eight at a time.
    async fn read_candidates(&self, tree: &mut Tree) {
        let wanted: Vec<usize> = (0..tree.nodes.len())
            .filter(|&i| filter::is_candidate(&tree.nodes[i]))
            .collect();
        let nodes = &tree.nodes;
        let results: Vec<_> = stream::iter(&wanted)
            .map(|&i| self.details(&nodes[i]))
            .buffered(IN_FLIGHT)
            .collect()
            .await;
        for (i, result) in wanted.into_iter().zip(results) {
            if let Ok((details, bounds)) = result {
                tree.nodes[i].details = Some(details);
                tree.nodes[i].bounds = Some(bounds);
            }
        }
    }

    /// Step 4b: boxes of the scroll areas above candidates, for clipping,
    /// and above text text mode may read.
    async fn read_scroll_areas(&self, tree: &mut Tree) {
        let scrollers = filter::scroll_areas(tree);
        let wanted: BTreeSet<usize> = (0..tree.nodes.len())
            .filter(|&i| tree.nodes[i].details.is_some() || filter::has_text(&tree.nodes[i]))
            .flat_map(|i| tree.ancestors(i))
            .filter(|a| scrollers.contains(a) && tree.nodes[*a].bounds.is_none())
            .collect();
        let nodes = &tree.nodes;
        let results: Vec<_> = stream::iter(&wanted)
            .map(|&i| self.extents(&nodes[i].object))
            .buffered(IN_FLIGHT)
            .collect()
            .await;
        // A failed read stays `None`, and the filter drops everything inside
        // that scroll area rather than guess what's in view.
        for (i, result) in wanted.into_iter().zip(results) {
            tree.nodes[i].bounds = result.ok();
        }
    }

    /// One object's text, if any of it shows in `area`: its name when
    /// that's `named` its text. Name, box and length come at once; a long
    /// text is read from the offset at the area's top-left corner to the
    /// one at its bottom-right.
    async fn read_text(
        &self,
        object: &ObjectId,
        area: Rect,
        named: bool,
    ) -> Result<Option<TextRead>> {
        let accessible: AccessibleProxy = self.proxy(object).await?;
        if named {
            let (name, bounds) = tokio::join!(self.call(accessible.name()), self.extents(object));
            let (name, bounds) = (name?, bounds?);
            let shows = !name.trim().is_empty() && bounds.intersect(area).is_some();
            return Ok(shows.then(|| TextRead {
                content: name.clone(),
                name,
                bounds,
                start: 0,
            }));
        }
        let text: TextProxy = self.proxy(object).await?;
        let (name, bounds, count) = tokio::join!(
            self.call(accessible.name()),
            self.extents(object),
            self.call(text.character_count())
        );
        let (bounds, count) = (bounds?, count?);
        // An empty box says nothing, as in the walk rule.
        let shown = if bounds.is_empty() {
            Some(area)
        } else {
            bounds.intersect(area)
        };
        let Some(shown) = shown.filter(|_| count > 0) else {
            return Ok(None);
        };
        let (start, end) = if count <= LONG_TEXT {
            (0, count)
        } else {
            let (coords, shift) = self.text_frame(object).await;
            let at = |x: i32, y: i32| text.get_offset_at_point(x - shift.x, y - shift.y, coords);
            let (first, last) = tokio::join!(
                self.call(at(shown.x, shown.y)),
                self.call(at(shown.right() - 1, shown.bottom() - 1))
            );
            // -1 is the app's "no character there".
            let start = first.ok().filter(|&f| f >= 0).unwrap_or(0);
            let end = last.ok().filter(|&l| l >= start).map_or(count, |l| l + 1);
            (start, end.min(count).min(start + LONG_TEXT))
        };
        let content = self.call(text.get_text(start, end)).await?;
        Ok(Some(TextRead {
            name: name.unwrap_or_default(),
            bounds,
            start: u32::try_from(start).unwrap_or(0),
            content,
        }))
    }

    /// Where characters `start..end` of an object are, a box per line.
    /// A box taller than a line and a half, by the height of the match's
    /// first character, is wrapped: it's split where the line it starts
    /// on ends, and so on. Nothing if the app can't say.
    async fn locate(&self, object: &ObjectId, start: u32, end: u32) -> Vec<Rect> {
        let Ok(text) = self.proxy::<TextProxy>(object).await else {
            return Vec::new();
        };
        let text = &text;
        let (coords, shift) = self.text_frame(object).await;
        let range = move |from: i32, to: i32| async move {
            let request = text.get_range_extents(from, to, coords);
            let (x, y, w, h) = self.call(request).await.ok()?;
            Some(Rect::new(x, y, w, h).offset(shift)).filter(|r| !r.is_empty())
        };
        let (mut from, end) = (offset(start), offset(end));
        let mut parts = Vec::new();
        for _ in 0..MAX_LINES {
            let (whole, first) = tokio::join!(
                range(from, end),
                self.call(text.get_character_extents(from, coords))
            );
            let Some(whole) = whole else { break };
            let line = first.map_or(0, |(_, _, _, h)| h);
            let wrapped = line > 0 && whole.h * 2 > line * 3;
            let line_end = if wrapped {
                self.call(text.get_string_at_offset(from, Granularity::Line))
                    .await
                    .map_or(end, |(_, _, line_end)| line_end)
            } else {
                end
            };
            if line_end <= from || line_end >= end {
                parts.push(whole);
                break;
            }
            if let Some(part) = range(from, line_end).await {
                parts.push(part);
            }
            from = line_end;
        }
        parts
    }

    /// The re-check's read of one object and the scroll areas above it.
    async fn read_fresh(&self, object: &ObjectId, clips: &[ObjectId]) -> Result<Fresh> {
        let accessible: AccessibleProxy = self.proxy(object).await?;
        // A scroll area that fails to answer is `None`, not an error: the
        // object itself answered, and the verdict says which part didn't.
        let clips = future::join_all(clips.iter().map(|c| async { self.extents(c).await.ok() }));
        let (role, name, states, bounds, clips) = tokio::join!(
            self.call(accessible.get_role()),
            self.call(accessible.name()),
            self.call(accessible.get_state()),
            self.extents(object),
            clips,
        );
        let role = match role {
            Ok(role) => role.name().to_string(),
            Err(_) => self.call(accessible.get_role_name()).await?,
        };
        Ok(Fresh {
            role,
            name: name?,
            states: states_from(states?),
            bounds: bounds?,
            clips,
        })
    }
}

/// The walk rule, settled on a live YouTube page (2026-10-20):
///
/// - Showing: always go in.
/// - Outside a document: go into what's visible, but not into a document
///   that isn't showing (Firefox's background tabs).
/// - Inside a document: Firefox never marks a real page's containers
///   "showing", so go into what's visible and not wholly outside the
///   window. An empty box says nothing (YouTube's custom elements report
///   0x0), so look inside.
///
/// That finds the same targets as entering everything visible, at half the
/// cost (38 ms and 925 nodes, against 79 ms and 1,793).
fn worth_entering(
    states: States,
    document: bool,
    in_document: bool,
    bounds: Option<Rect>,
    window: Rect,
) -> bool {
    if states.showing {
        true
    } else if !in_document {
        states.visible && !document
    } else {
        states.visible
            && bounds.is_none_or(|b| {
                b.is_empty()
                    || (b.x < window.w && b.y < window.h && b.right() > 0 && b.bottom() > 0)
            })
    }
}

/// The frame whose name is the window title. Several with that title (two
/// Chromium windows on "New Tab"), or none: the one active frame among them,
/// but only for the focused window. "Active" means focused, so for any other
/// window the active frame is a different window of the same app, and its
/// targets would be drawn over this window. Still not exactly one: nothing,
/// so the caller refuses instead of guessing.
///
/// The returned reference borrows from `frames`, and the lifetime `'f` says
/// so: the caller can't drop the list while holding the answer.
fn pick_frame<'f>(
    frames: &'f [(ObjectId, String, bool)],
    title: &str,
    focused: bool,
) -> Option<&'f (ObjectId, String, bool)> {
    let by_title: Vec<_> = frames.iter().filter(|f| f.1 == title).collect();
    if let [one] = by_title.as_slice() {
        return Some(one);
    }
    if !focused {
        return None;
    }
    let pool = if by_title.is_empty() {
        frames.iter().collect()
    } else {
        by_title
    };
    match pool.as_slice() {
        [one] => Some(one),
        many => match many.iter().filter(|f| f.2).collect::<Vec<_>>().as_slice() {
            [one] => Some(one),
            _ => None,
        },
    }
}

fn object_id(object: &ObjectRefOwned) -> ObjectId {
    ObjectId {
        bus: object.name_as_str().unwrap_or_default().to_string(),
        path: object.path_as_str().to_string(),
    }
}

fn states_from(set: StateSet) -> States {
    States {
        showing: set.contains(State::Showing),
        visible: set.contains(State::Visible),
        enabled: set.contains(State::Enabled),
        sensitive: set.contains(State::Sensitive),
        focusable: set.contains(State::Focusable),
        editable: set.contains(State::Editable),
        selectable: set.contains(State::Selectable),
        active: set.contains(State::Active),
    }
}

/// Milliseconds with one decimal, for stats people read.
pub fn millis(d: Duration) -> f64 {
    (d.as_secs_f64() * 10_000.0).round() / 10.0
}

/// When process `pid` started, in clock ticks since boot.
fn started(pid: u32) -> Option<u64> {
    start_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Field 22 of a `/proc/<pid>/stat` line. The name, field 2, is in
/// parentheses and may hold spaces, so count from after it.
fn start_time(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_is_field_22_even_with_spaces_in_the_name() {
        let stat = "783639 (dolphin (x) 1) S 1 2 3 0 -1 4 5 6 7 8 9 10 11 12 20 0 4 0 \
                    81500123 4096 100";
        assert_eq!(start_time(stat), Some(81_500_123));
        assert!(started(std::process::id()).is_some());
    }

    const WINDOW: Rect = Rect::new(0, 0, 800, 600);

    fn states(showing: bool, visible: bool) -> States {
        States {
            showing,
            visible,
            ..States::default()
        }
    }

    #[test]
    fn walk_rule_outside_documents() {
        assert!(worth_entering(
            states(true, true),
            false,
            false,
            None,
            WINDOW
        ));
        assert!(
            worth_entering(states(false, true), false, false, None, WINDOW),
            "Firefox chrome containers"
        );
        assert!(
            !worth_entering(states(false, true), true, false, None, WINDOW),
            "a background tab's document"
        );
        assert!(!worth_entering(
            states(false, false),
            false,
            false,
            None,
            WINDOW
        ));
    }

    #[test]
    fn walk_rule_inside_documents() {
        let visible = states(false, true);
        assert!(worth_entering(
            visible,
            false,
            true,
            Some(Rect::new(10, 10, 100, 100)),
            WINDOW
        ));
        assert!(
            worth_entering(visible, false, true, Some(Rect::new(0, 0, 0, 0)), WINDOW),
            "empty box: look inside"
        );
        assert!(
            !worth_entering(
                visible,
                false,
                true,
                Some(Rect::new(0, 2000, 100, 100)),
                WINDOW
            ),
            "below the fold"
        );
    }

    #[test]
    fn frames_by_title_then_active() {
        let id = |n: &str| ObjectId {
            bus: ":1.1".into(),
            path: n.into(),
        };
        let pick = |frames: &[(ObjectId, String, bool)], title: &str, focused: bool| {
            pick_frame(frames, title, focused).map(|f| f.0.path.clone())
        };
        let frames = vec![
            (id("/a"), "Inbox".to_string(), false),
            (id("/b"), "New Tab".to_string(), true),
        ];
        assert_eq!(pick(&frames, "Inbox", true).as_deref(), Some("/a"));
        assert_eq!(pick(&frames, "Inbox", false).as_deref(), Some("/a"));
        assert_eq!(pick(&frames, "Other", true).as_deref(), Some("/b"));
        assert_eq!(
            pick(&frames, "Other", false),
            None,
            "unfocused: the active frame is some other window"
        );
        let twins = vec![
            (id("/a"), "New Tab".to_string(), false),
            (id("/b"), "New Tab".to_string(), true),
        ];
        assert_eq!(
            pick(&twins, "New Tab", true).as_deref(),
            Some("/b"),
            "same title: the active one"
        );
        assert_eq!(pick(&twins, "New Tab", false), None);
        let both = vec![
            (id("/a"), "New Tab".to_string(), true),
            (id("/b"), "New Tab".to_string(), true),
        ];
        assert_eq!(pick(&both, "New Tab", true), None, "still two: refuse");
    }
}
