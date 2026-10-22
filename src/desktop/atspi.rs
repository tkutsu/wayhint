//! Reading the accessibility tree over AT-SPI (D-Bus), asynchronously.
//!
//! The scan, for one window:
//!
//! 1. Find the window's app: the registry child whose bus connection has
//!    the window's PID.
//! 2. Find the window's frame among the app's children: by title, else (for
//!    the focused window only) the one marked active. Two candidates is
//!    ambiguous: refuse, never guess.
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
    AccessibilityConnection, CoordType, Interface, ObjectRefOwned, State, StateSet,
    proxy::{accessible::AccessibleProxy, action::ActionProxy, component::ComponentProxy},
};
use futures::{StreamExt as _, stream};
use serde::Serialize;
use zbus::proxy::CacheProperties;

use crate::{
    desktop::{
        Window,
        filter::{self, DOCUMENT_ROLES},
        recheck::Fresh,
        tree::{Details, Node, ObjectId, States, Tree},
    },
    geometry::Rect,
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

    /// Reads one window's tree within `bounds`.
    pub async fn scan(&self, window: &Window, bounds: Bounds) -> Result<(Tree, Stats)> {
        let started = Instant::now();
        let walk = Walk::new(self.a11y.connection(), started + bounds.time);
        let frame = walk.find_frame(&self.a11y, window).await?;
        let nodes = walk.read_tree(frame, window.rect, bounds.nodes).await;
        let mut tree = Tree {
            window: window.clone(),
            nodes,
        };
        walk.read_candidates(&mut tree).await;
        walk.read_scroll_areas(&mut tree).await;
        let stats = Stats {
            ms: millis(started.elapsed()),
            nodes: tree.nodes.len(),
            requests: walk.requests.get(),
            incomplete: walk.incomplete.take(),
        };
        Ok((tree, stats))
    }

    /// Asks one object again: role, name, states and box, in one bounded go.
    pub async fn read(&self, object: &ObjectId, timeout: Duration) -> Result<Fresh> {
        let walk = Walk::new(self.a11y.connection(), Instant::now() + timeout);
        walk.read_fresh(object).await
    }
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

    /// Steps 1 and 2: the frame of `window` in the tree.
    async fn find_frame(
        &self,
        a11y: &AccessibilityConnection,
        window: &Window,
    ) -> Result<ObjectId> {
        let registry = a11y.root_accessible_on_registry().await?;
        let apps = self.call(registry.get_children()).await?;
        let dbus = zbus::fdo::DBusProxy::new(self.conn).await?;
        let mut app = None;
        for candidate in &apps {
            let Some(bus) = candidate.name() else {
                continue;
            };
            // Apps registered with a dead socket (Steam) fail here; skip them.
            if self
                .call(dbus.get_connection_unix_process_id(bus.clone().into()))
                .await
                .ok()
                == Some(window.pid)
            {
                app = Some(object_id(candidate));
                break;
            }
        }
        let app = app.with_context(|| {
            format!(
                "{} (pid {}) isn't on the accessibility bus: accessibility is off for it",
                window.class, window.pid
            )
        })?;

        let app: AccessibleProxy = self.proxy(&app).await?;
        let mut frames = Vec::new();
        for frame in self.call(app.get_children()).await? {
            let id = object_id(&frame);
            let proxy: AccessibleProxy = self.proxy(&id).await?;
            let name = self.call(proxy.name()).await.unwrap_or_default();
            let states = self.call(proxy.get_state()).await.unwrap_or_default();
            frames.push((id, name, states.contains(State::Active)));
        }
        pick_frame(&frames, &window.title, window.focused)
            .map(|(id, _, _)| id.clone())
            .with_context(|| {
                format!(
                    "{} has {} windows and none is clearly {:?}",
                    window.class,
                    frames.len(),
                    window.title
                )
            })
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
            bounds,
            details: None,
        };
        Ok((node, children, pending.in_document || document))
    }

    async fn extents(&self, object: &ObjectId) -> Result<Rect> {
        let component: ComponentProxy = self.proxy(object).await?;
        let (x, y, w, h) = self.call(component.get_extents(CoordType::Window)).await?;
        Ok(Rect::new(x, y, w, h))
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

    /// Step 4b: boxes of the scroll areas above candidates, for clipping.
    async fn read_scroll_areas(&self, tree: &mut Tree) {
        let scrollers = filter::scroll_areas(tree);
        let wanted: BTreeSet<usize> = (0..tree.nodes.len())
            .filter(|&i| tree.nodes[i].details.is_some())
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

    /// The re-check's read of one object.
    async fn read_fresh(&self, object: &ObjectId) -> Result<Fresh> {
        let accessible: AccessibleProxy = self.proxy(object).await?;
        let (role, name, states, bounds) = tokio::join!(
            self.call(accessible.get_role()),
            self.call(accessible.name()),
            self.call(accessible.get_state()),
            self.extents(object),
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

#[cfg(test)]
mod tests {
    use super::*;

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
