//! Which objects get a tag. Pure: a [`Tree`] in, kept and dropped nodes out,
//! every drop with its [`Reason`].
//!
//! The rules come from the phase 1b scan spike, scored against fixture
//! windows (GTK4 8/8, Qt 8/8, Firefox 15/16, Chromium 16/16) and live pages
//! (YouTube, Dolphin, Sidebery). Each rule below names what taught it.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::{
    desktop::tree::{Node, Tree},
    geometry::Rect,
};

/// Smallest box worth a tag, in px.
const MIN_SIZE: i32 = 4;

/// Action names that mean "this gets clicked", lowercased. Chromium adds
/// `check` and `open`. `click-ancestor` (Firefox) and `clickAncestor`
/// (Chromium) mean the parent takes the click, so they don't count.
const REAL_ACTIONS: [&str; 10] = [
    "click",
    "press",
    "activate",
    "jump",
    "toggle",
    "expand or contract",
    "check",
    "uncheck",
    "open",
    "switch",
];

/// Controls by role, whatever actions they list: GTK4 check buttons and
/// switches list none, Qt combo boxes only "Set Focus".
const CONTROL_ROLES: [&str; 15] = [
    "button",
    "toggle button",
    "check box",
    "radio button",
    "combo box",
    "slider",
    "spin button",
    "link",
    "menu item",
    "check menu item",
    "radio menu item",
    "page tab",
    "push button menu",
    "entry",
    "switch",
];

/// Selectable wrappers (GTK4 wraps every flow box child in one): a target
/// only when they hold no control and aren't empty.
const CELL_ROLES: [&str; 3] = ["table cell", "list item", "tree item"];

/// Pictures. Never a target of their own in the first release; one inside a
/// clickable belongs to it (75 Sidebery favicons, 2026-10-20).
const MEDIA_ROLES: [&str; 4] = ["image", "video", "canvas", "animation"];

/// Generic containers. Unnamed, with targets inside, they're event-delegation
/// wrappers (YouTube puts `click` on the whole page), not targets.
const CONTAINER_ROLES: [&str; 9] = [
    "section",
    "panel",
    "landmark",
    "document web",
    "internal frame",
    "form",
    "scroll pane",
    "filler",
    "grouping",
];

/// Web and office documents: the walk treats their insides differently.
pub const DOCUMENT_ROLES: [&str; 6] = [
    "document web",
    "document frame",
    "document text",
    "document email",
    "document presentation",
    "document spreadsheet",
];

/// Documents typed into as a whole: one tag for the sheet or page, which
/// clicks it and ends the mode, never one per cell or paragraph. Calc's
/// sheet lists no cells and isn't focusable; Writer's lists each visible
/// paragraph as an editable target of its own (2026-10-25).
const BODY_ROLES: [&str; 2] = ["document spreadsheet", "document text"];

/// Whether an object with `role` is a document body, `editable` now.
pub fn is_body(role: &str, editable: bool) -> bool {
    editable && BODY_ROLES.contains(&role)
}

/// Why a node got no tag. `Serialize` with kebab-case names, for
/// `dump --explain`: `"reason": "too-small"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    TooSmall,
    /// Wholly outside the window, or less than [`MIN_SIZE`] inside it.
    OutsideWindow,
    /// Firefox gives its frame a `click` action.
    TheWindow,
    Disabled,
    NoAction,
    /// The same control reported twice (a label inside a button).
    Duplicate,
    CellHoldsControl,
    EmptyCell,
    InsideControl,
    /// Text inside a document body, which has the one tag.
    InsideBody,
    Wrapper,
    /// Scrolled out of view: GTK and Qt still say "showing".
    ClippedByScrollArea,
    /// A scroll area above it didn't give its box, so what's in view is
    /// unknown.
    ScrollAreaUnread,
}

/// A node that gets a tag, with the part of its box that's visible
/// (window-relative; the snapshot moves it to global).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kept {
    pub node: usize,
    pub visible: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Dropped {
    pub node: usize,
    pub reason: Reason,
}

#[derive(Debug, Default)]
pub struct Filtered {
    pub kept: Vec<Kept>,
    pub dropped: Vec<Dropped>,
}

/// Whether a node might get a tag, from what the walk reads for every node
/// (role, states, interfaces). The scan reads names, boxes and actions only
/// for these, which is most of its cost.
pub fn is_candidate(node: &Node) -> bool {
    let s = node.states;
    let actionable = node.action
        || CONTROL_ROLES.contains(&node.role.as_str())
        || (s.focusable && (s.editable || s.selectable))
        || is_body(&node.role, s.editable);
    s.showing && s.visible && node.component && actionable
}

/// Nodes whose box clips what's inside: scroll panes, viewports, and
/// anything with a scroll bar child (Qt's ScrollView). Only these clip; a
/// web page's pinned header sits outside its container's box (YouTube).
pub fn scroll_areas(tree: &Tree) -> BTreeSet<usize> {
    let by_role = tree
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| matches!(n.role.as_str(), "scroll pane" | "viewport"));
    let scrollbar_parents = tree
        .nodes
        .iter()
        .filter(|n| n.role == "scroll bar")
        .filter_map(|n| n.parent);
    by_role.map(|(i, _)| i).chain(scrollbar_parents).collect()
}

/// A candidate with everything the rules read, borrowed from the tree. The
/// lifetime `'t` ties every reference to the tree: a `Candidate` can't
/// outlive the `Tree` it points into, and the compiler checks that.
struct Candidate<'t> {
    index: usize,
    node: &'t Node,
    name: &'t str,
    actions: &'t [String],
    bounds: Rect,
}

impl Candidate<'_> {
    fn role(&self) -> &str {
        &self.node.role
    }

    fn named(&self) -> bool {
        !self.name.trim().is_empty()
    }

    fn has_real_action(&self) -> bool {
        self.actions
            .iter()
            .any(|a| REAL_ACTIONS.contains(&a.to_lowercase().as_str()))
    }

    fn is_control(&self) -> bool {
        self.has_real_action() || CONTROL_ROLES.contains(&self.role())
    }

    fn is_cell(&self) -> bool {
        CELL_ROLES.contains(&self.role()) && !self.is_control()
    }

    fn is_media(&self) -> bool {
        MEDIA_ROLES.contains(&self.role())
    }

    fn is_body(&self) -> bool {
        is_body(self.role(), self.node.states.editable)
    }

    /// Of two nested candidates with the same box, the higher rank stays:
    /// a control role beats a bare click action (YouTube's Create button
    /// inside an unnamed clickable section), which beats the rest; named
    /// beats unnamed at the same kind.
    fn rank(&self) -> u8 {
        let kind = if self.is_cell() {
            0
        } else if CONTROL_ROLES.contains(&self.role()) {
            3
        } else if self.has_real_action() {
            2
        } else {
            1
        };
        kind * 2 + u8::from(self.named())
    }

    /// Rule 1's first pass: things that are never a target on their own.
    fn first_pass(&self, window: Rect) -> Option<Reason> {
        let b = self.bounds;
        let s = self.node.states;
        let editable_like = s.focusable && (s.editable || s.selectable);
        if b.w < MIN_SIZE || b.h < MIN_SIZE {
            Some(Reason::TooSmall)
        } else if b.right() <= 0 || b.bottom() <= 0 || b.x >= window.w || b.y >= window.h {
            Some(Reason::OutsideWindow)
        } else if matches!(self.role(), "frame" | "window") {
            Some(Reason::TheWindow)
        } else if !s.enabled && !s.sensitive {
            Some(Reason::Disabled)
        } else if !self.is_control() && !editable_like && !self.is_body() {
            Some(Reason::NoAction)
        } else {
            None
        }
    }
}

/// Runs every rule, in order. Each step only looks at what the steps before
/// it kept.
pub fn filter(tree: &Tree) -> Filtered {
    let mut out = Filtered::default();

    // Candidates the walk read in full. A candidate without details or a box
    // was cut by the scan's budget; it's left out, and the scan says
    // "incomplete" for it.
    let candidates = tree
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| is_candidate(n))
        .filter_map(|(index, node)| {
            let details = node.details.as_ref()?;
            Some(Candidate {
                index,
                node,
                name: &details.name,
                actions: &details.actions,
                bounds: node.bounds?,
            })
        });

    // Step 1: one candidate at a time.
    let mut kept: Vec<Candidate> = Vec::new();
    let mut disabled_controls: Vec<usize> = Vec::new();
    for c in candidates {
        match c.first_pass(tree.window.rect) {
            None => kept.push(c),
            Some(reason) => {
                if reason == Reason::Disabled && c.is_control() {
                    disabled_controls.push(c.index);
                }
                out.dropped.push(Dropped {
                    node: c.index,
                    reason,
                });
            }
        }
    }

    // Step 2: wrappers. An unnamed container, or any document, with another
    // non-media target inside. A named one (a Sidebery tab row with its
    // close button inside) is a target.
    let wrapper: Vec<bool> = kept
        .iter()
        .map(|c| {
            CONTAINER_ROLES.contains(&c.role())
                && (!c.named() || matches!(c.role(), "document web" | "internal frame"))
                && kept
                    .iter()
                    .any(|k| !k.is_media() && tree.is_inside(k.index, c.index))
        })
        .collect();

    // Step 3: duplicates.
    let duplicate = duplicates(tree, &kept, &wrapper);

    // Step 4: what's left of rules 1-3, which need the whole kept set.
    // Wrappers aren't controls here: their click is the page's event
    // delegation, and everything inside them would count as inside a control.
    let controls: BTreeSet<usize> = kept
        .iter()
        .zip(&wrapper)
        .filter(|&(c, &is_wrapper)| c.is_control() && !is_wrapper)
        .map(|(c, _)| c.index)
        .chain(disabled_controls)
        .collect();
    let bodies: BTreeSet<usize> = kept
        .iter()
        .filter(|c| c.is_body())
        .map(|c| c.index)
        .collect();
    let scrollers = scroll_areas(tree);
    for ((c, is_duplicate), is_wrapper) in kept.iter().zip(duplicate).zip(wrapper) {
        let reason = if is_duplicate {
            Some(Reason::Duplicate)
        } else if c.is_cell() && controls.iter().any(|&k| tree.is_inside(k, c.index)) {
            Some(Reason::CellHoldsControl)
        } else if c.is_cell() && !c.named() && !has_showing_child(tree, c.index) {
            // GTK4 leaves the cell of a hidden child behind. A named cell
            // with no children is real: Dolphin's file rows.
            Some(Reason::EmptyCell)
        } else if !c.is_control()
            && !c.is_media()
            && tree.ancestors(c.index).any(|a| controls.contains(&a))
        {
            // A closed popup's row inside a combo box, whatever the boxes.
            Some(Reason::InsideControl)
        } else if !c.is_control() && tree.ancestors(c.index).any(|a| bodies.contains(&a)) {
            // Writer's paragraphs; a link or field in the page keeps its tag.
            Some(Reason::InsideBody)
        } else if is_wrapper {
            Some(Reason::Wrapper)
        } else {
            None
        };
        match reason {
            Some(reason) => out.dropped.push(Dropped {
                node: c.index,
                reason,
            }),
            None => match clip(tree, c, &scrollers) {
                Ok(visible) => out.kept.push(Kept {
                    node: c.index,
                    visible,
                }),
                Err(reason) => out.dropped.push(Dropped {
                    node: c.index,
                    reason,
                }),
            },
        }
    }
    out
}

/// Step 3: which of the `kept` candidates are duplicates, by position.
///
/// Each candidate is compared with its nearest ancestor that's still in the
/// running: nearly the same box means one control reported twice, and media
/// inside a clickable is the clickable's. Two kinds of ancestor are out of
/// the running:
///
/// - Wrappers, which step 4 drops: a wrapper that won here would take its
///   child down with it, and neither would get a tag.
/// - Ancestors already marked duplicate: in three nested boxes of one size,
///   if the middle loses to the outer, the inner must face the outer, or
///   both survive.
///
/// Parents come before children in walk order, so an ancestor's fate is
/// known by the time its descendants come up.
fn duplicates(tree: &Tree, kept: &[Candidate], wrapper: &[bool]) -> Vec<bool> {
    let mut duplicate = vec![false; kept.len()];
    for (i, inner) in kept.iter().enumerate() {
        let in_running = |node: usize| {
            kept.iter()
                .position(|k| k.index == node)
                .filter(|&j| !wrapper[j] && !duplicate[j])
        };
        let Some(j) = tree.ancestors(inner.index).find_map(in_running) else {
            continue;
        };
        let outer = &kept[j];
        let same_box = inner.bounds.iou(outer.bounds) > 0.8;
        let media_in_clickable = inner.is_media() && !outer.is_media() && outer.is_control();
        if same_box || media_in_clickable {
            let inner_wins = !inner.is_media() && inner.rank() > outer.rank();
            duplicate[if inner_wins { j } else { i }] = true;
        }
    }
    duplicate
}

fn has_showing_child(tree: &Tree, index: usize) -> bool {
    tree.nodes
        .iter()
        .any(|n| n.parent == Some(index) && n.states.showing)
}

/// The part of a candidate's box a click can reach: inside the window and
/// inside every scroll area above it. The click goes to the centre of this
/// part, so a button half past the window's edge is clicked in its visible
/// half, not beside the window.
///
/// Dropped when too little is left, or when a scroll area above it has no
/// box: that read failed, so which part is visible is unknown, and a click
/// could land on whatever covers the hidden part. An empty box (0x0) says
/// nothing, as in the walk rule, and is skipped.
fn clip(tree: &Tree, c: &Candidate, scrollers: &BTreeSet<usize>) -> Result<Rect, Reason> {
    let big_enough = |r: &Rect| r.w >= MIN_SIZE && r.h >= MIN_SIZE;
    let window = Rect::new(0, 0, tree.window.rect.w, tree.window.rect.h);
    let in_window = c
        .bounds
        .intersect(window)
        .filter(big_enough)
        .ok_or(Reason::OutsideWindow)?;
    // `try_fold` threads the visible part through every scroll area and
    // stops at the first `Err`.
    tree.ancestors(c.index)
        .filter(|a| scrollers.contains(a))
        .try_fold(in_window, |visible, area| match tree.nodes[area].bounds {
            None => Err(Reason::ScrollAreaUnread),
            Some(b) if b.is_empty() => Ok(visible),
            Some(b) => visible
                .intersect(b)
                .filter(big_enough)
                .ok_or(Reason::ClippedByScrollArea),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{
        Kind, Window,
        tree::{Details, ObjectId, States},
    };

    /// A small tree builder, so each test reads like the window it models.
    struct Builder {
        nodes: Vec<Node>,
    }

    const SHOWN: States = States {
        showing: true,
        visible: true,
        enabled: true,
        sensitive: true,
        focusable: false,
        editable: false,
        selectable: false,
        active: false,
    };

    impl Builder {
        fn new() -> Self {
            let mut b = Self { nodes: Vec::new() };
            b.add(None, "frame", "", &[], Rect::new(0, 0, 800, 600));
            b
        }

        fn add(
            &mut self,
            parent: Option<usize>,
            role: &str,
            name: &str,
            actions: &[&str],
            bounds: Rect,
        ) -> usize {
            self.nodes.push(Node {
                object: ObjectId {
                    bus: ":1.1".into(),
                    path: format!("/{}", self.nodes.len()),
                },
                parent,
                role: role.into(),
                states: SHOWN,
                action: !actions.is_empty(),
                component: true,
                bounds: Some(bounds),
                details: Some(Details {
                    name: name.into(),
                    actions: actions.iter().map(|&a| a.into()).collect(),
                }),
            });
            self.nodes.len() - 1
        }

        fn tree(self) -> Tree {
            let window = Window {
                address: "0x1".into(),
                pid: 1,
                class: "test".into(),
                title: "test".into(),
                rect: Rect::new(100, 100, 800, 600),
                monitor: 0,
                focused: true,
                kind: Kind::Client,
                frames: Vec::new(),
            };
            Tree {
                window,
                nodes: self.nodes,
            }
        }
    }

    fn kept(tree: &Tree) -> Vec<usize> {
        filter(tree).kept.iter().map(|k| k.node).collect()
    }

    fn reason(tree: &Tree, node: usize) -> Option<Reason> {
        filter(tree)
            .dropped
            .iter()
            .find(|d| d.node == node)
            .map(|d| d.reason)
    }

    #[test]
    fn buttons_and_links_are_kept_text_is_not() {
        let mut b = Builder::new();
        let ok = b.add(
            Some(0),
            "button",
            "OK",
            &["press"],
            Rect::new(10, 10, 80, 30),
        );
        let link = b.add(
            Some(0),
            "link",
            "Docs",
            &["jump"],
            Rect::new(10, 50, 80, 20),
        );
        let text = b.add(Some(0), "label", "Hello", &[], Rect::new(10, 90, 80, 20));
        let tree = b.tree();
        assert_eq!(kept(&tree), vec![ok, link]);
        assert!(
            !is_candidate(&tree.nodes[text]),
            "plain text is never read in full"
        );
    }

    #[test]
    fn controls_count_by_role_without_actions() {
        // GTK4 check buttons list no actions.
        let mut b = Builder::new();
        let check = b.add(
            Some(0),
            "check box",
            "Wi-Fi",
            &[],
            Rect::new(10, 10, 80, 30),
        );
        assert_eq!(kept(&b.tree()), vec![check]);
    }

    #[test]
    fn small_disabled_and_outside_are_dropped() {
        let mut b = Builder::new();
        let tiny = b.add(Some(0), "button", "x", &["press"], Rect::new(10, 10, 2, 30));
        let away = b.add(
            Some(0),
            "button",
            "y",
            &["press"],
            Rect::new(900, 10, 40, 30),
        );
        let off = b.add(
            Some(0),
            "button",
            "z",
            &["press"],
            Rect::new(10, 50, 40, 30),
        );
        b.nodes[off].states.enabled = false;
        b.nodes[off].states.sensitive = false;
        let tree = b.tree();
        assert_eq!(reason(&tree, tiny), Some(Reason::TooSmall));
        assert_eq!(reason(&tree, away), Some(Reason::OutsideWindow));
        assert_eq!(reason(&tree, off), Some(Reason::Disabled));
    }

    #[test]
    fn firefox_frame_with_click_is_not_a_target() {
        let mut b = Builder::new();
        b.nodes[0].details.as_mut().unwrap().actions = vec!["click".into()];
        b.nodes[0].action = true;
        assert_eq!(reason(&b.tree(), 0), Some(Reason::TheWindow));
    }

    #[test]
    fn a_label_inside_a_button_is_one_tag() {
        let mut b = Builder::new();
        let button = b.add(
            Some(0),
            "button",
            "Save",
            &["press"],
            Rect::new(10, 10, 80, 30),
        );
        let label = b.add(
            Some(button),
            "label",
            "Save",
            &["click"],
            Rect::new(12, 11, 76, 28),
        );
        let tree = b.tree();
        assert_eq!(kept(&tree), vec![button]);
        assert_eq!(reason(&tree, label), Some(Reason::Duplicate));
    }

    #[test]
    fn unnamed_clickable_section_loses_to_its_button() {
        // YouTube's Create button sits in an unnamed section with `click`.
        let mut b = Builder::new();
        let section = b.add(
            Some(0),
            "section",
            "",
            &["click"],
            Rect::new(10, 10, 40, 40),
        );
        let button = b.add(
            Some(section),
            "button",
            "Create",
            &["press"],
            Rect::new(10, 10, 40, 40),
        );
        let tree = b.tree();
        assert_eq!(kept(&tree), vec![button]);
    }

    #[test]
    fn unnamed_wrapper_goes_named_row_stays() {
        let mut b = Builder::new();
        let page = b.add(
            Some(0),
            "section",
            "",
            &["click"],
            Rect::new(0, 0, 800, 600),
        );
        let inner = b.add(
            Some(page),
            "button",
            "Play",
            &["press"],
            Rect::new(10, 10, 40, 40),
        );
        // Sidebery: a named tab row with a close button inside.
        let row = b.add(
            Some(0),
            "section",
            "Inbox",
            &["click"],
            Rect::new(0, 100, 300, 30),
        );
        let close = b.add(
            Some(row),
            "button",
            "Close",
            &["press"],
            Rect::new(270, 105, 20, 20),
        );
        let tree = b.tree();
        assert_eq!(reason(&tree, page), Some(Reason::Wrapper));
        assert_eq!(kept(&tree), vec![inner, row, close]);
    }

    #[test]
    fn image_inside_a_link_is_the_links() {
        let mut b = Builder::new();
        let link = b.add(
            Some(0),
            "link",
            "Home",
            &["jump"],
            Rect::new(10, 10, 200, 40),
        );
        let icon = b.add(
            Some(link),
            "image",
            "",
            &["click"],
            Rect::new(12, 12, 16, 16),
        );
        let tree = b.tree();
        assert_eq!(kept(&tree), vec![link]);
        assert_eq!(reason(&tree, icon), Some(Reason::Duplicate));
    }

    #[test]
    fn cells_holding_controls_or_nothing_are_dropped() {
        let mut b = Builder::new();
        let list = b.add(Some(0), "list", "", &[], Rect::new(0, 0, 400, 400));
        let holder = b.add(Some(list), "list item", "", &[], Rect::new(0, 0, 400, 40));
        b.nodes[holder].states.focusable = true;
        b.nodes[holder].states.selectable = true;
        let button = b.add(
            Some(holder),
            "button",
            "Go",
            &["press"],
            Rect::new(300, 5, 80, 30),
        );
        let empty = b.add(Some(list), "list item", "", &[], Rect::new(0, 40, 400, 40));
        b.nodes[empty].states.focusable = true;
        b.nodes[empty].states.selectable = true;
        // Dolphin: a named file row without children is a target.
        let file = b.add(
            Some(list),
            "list item",
            "notes.md",
            &[],
            Rect::new(0, 80, 400, 40),
        );
        b.nodes[file].states.focusable = true;
        b.nodes[file].states.selectable = true;
        let tree = b.tree();
        assert_eq!(reason(&tree, holder), Some(Reason::CellHoldsControl));
        assert_eq!(reason(&tree, empty), Some(Reason::EmptyCell));
        assert_eq!(kept(&tree), vec![button, file]);
    }

    #[test]
    fn an_editable_document_is_one_tag_and_keeps_its_links() {
        // As LibreOffice gave them (2026-10-25): Calc's sheet has no
        // children and isn't focusable; Writer's paragraphs are editable
        // targets of their own.
        let mut b = Builder::new();
        let name_box = b.add(Some(0), "text", "", &[], Rect::new(4, 70, 196, 36));
        b.nodes[name_box].states.focusable = true;
        b.nodes[name_box].states.editable = true;
        let sheet = b.add(
            Some(0),
            "document spreadsheet",
            "",
            &[],
            Rect::new(0, 140, 400, 400),
        );
        b.nodes[sheet].states.editable = true;
        let page = b.add(
            Some(0),
            "document text",
            "",
            &[],
            Rect::new(400, 140, 400, 400),
        );
        b.nodes[page].states.editable = true;
        let paragraph = b.add(
            Some(page),
            "paragraph",
            "",
            &[],
            Rect::new(420, 160, 360, 15),
        );
        b.nodes[paragraph].states.focusable = true;
        b.nodes[paragraph].states.editable = true;
        let link = b.add(
            Some(paragraph),
            "link",
            "wayhint",
            &["jump"],
            Rect::new(440, 160, 60, 15),
        );
        let preview = b.add(
            Some(0),
            "document text",
            "",
            &[],
            Rect::new(0, 540, 800, 60),
        );
        let tree = b.tree();
        assert!(
            !is_candidate(&tree.nodes[preview]),
            "read-only stays as it was"
        );
        assert_eq!(reason(&tree, paragraph), Some(Reason::InsideBody));
        assert_eq!(kept(&tree), vec![name_box, sheet, page, link]);
    }

    /// Kept names from a saved `dump --raw` of a fixture window, as a set:
    /// the filter keeps walk order, and only membership matters here.
    fn kept_names(raw: &str) -> BTreeSet<String> {
        #[derive(serde::Deserialize)]
        struct Raw {
            tree: Tree,
        }
        let tree = serde_json::from_str::<Raw>(raw).unwrap().tree;
        filter(&tree)
            .kept
            .iter()
            .map(|k| tree.nodes[k.node].details.as_ref().unwrap().name.clone())
            .collect()
    }

    #[test]
    fn gtk4_fixture_window() {
        // `include_str!` reads the file at compile time into the test binary.
        let names = kept_names(include_str!("../../tests/fixtures/gtk4.raw.json"));
        let expected = [
            "fx button",
            "fx link",
            "fx entry",
            "fx check",
            "fx switch",
            "one", // GTK4 names a dropdown by its value
            "fx icon button",
            "fx toggle",
            "fx label", // known false tag: a flow box cell holding only a label
        ];
        assert_eq!(names, expected.map(String::from).into());
    }

    #[test]
    fn qml_fixture_window() {
        let names = kept_names(include_str!("../../tests/fixtures/qml.raw.json"));
        let expected = [
            "fx slider",
            "fx button",
            "fx field",
            "fx combo",
            "fx icon button",
            "fx check",
            "fx switch",
            "fx radio",
        ];
        assert_eq!(names, expected.map(String::from).into());
    }

    #[test]
    fn scrolled_out_content_is_clipped() {
        let mut b = Builder::new();
        let pane = b.add(Some(0), "scroll pane", "", &[], Rect::new(0, 0, 400, 200));
        let half = b.add(
            Some(pane),
            "button",
            "Half",
            &["press"],
            Rect::new(10, 180, 80, 40),
        );
        let gone = b.add(
            Some(pane),
            "button",
            "Gone",
            &["press"],
            Rect::new(10, 300, 80, 40),
        );
        let tree = b.tree();
        let out = filter(&tree);
        assert_eq!(
            out.kept,
            vec![Kept {
                node: half,
                visible: Rect::new(10, 180, 80, 20)
            }]
        );
        assert_eq!(reason(&tree, gone), Some(Reason::ClippedByScrollArea));
    }

    #[test]
    fn a_control_past_the_window_edge_is_clicked_inside() {
        let mut b = Builder::new();
        let half = b.add(
            Some(0),
            "button",
            "Half",
            &["press"],
            Rect::new(-200, 10, 220, 30),
        );
        let sliver = b.add(
            Some(0),
            "button",
            "Sliver",
            &["press"],
            Rect::new(-200, 50, 202, 30),
        );
        let tree = b.tree();
        let out = filter(&tree);
        assert_eq!(
            out.kept,
            vec![Kept {
                node: half,
                visible: Rect::new(0, 10, 20, 30)
            }]
        );
        assert_eq!(reason(&tree, sliver), Some(Reason::OutsideWindow));
    }

    #[test]
    fn an_unread_scroll_area_hides_what_is_inside() {
        let mut b = Builder::new();
        let pane = b.add(Some(0), "scroll pane", "", &[], Rect::new(0, 0, 400, 200));
        let button = b.add(
            Some(pane),
            "button",
            "Maybe",
            &["press"],
            Rect::new(10, 300, 80, 40),
        );
        b.nodes[pane].bounds = None;
        let tree = b.tree();
        assert_eq!(kept(&tree), Vec::<usize>::new());
        assert_eq!(reason(&tree, button), Some(Reason::ScrollAreaUnread));
    }

    #[test]
    fn a_wrapper_never_takes_its_child_down() {
        // An unnamed clickable section the size of the file row inside it
        // outranks the row, but it's a wrapper and goes in step 4.
        let mut b = Builder::new();
        let section = b.add(Some(0), "section", "", &["click"], Rect::new(0, 0, 400, 40));
        let row = b.add(
            Some(section),
            "list item",
            "notes.md",
            &[],
            Rect::new(0, 0, 400, 40),
        );
        b.nodes[row].states.focusable = true;
        b.nodes[row].states.selectable = true;
        let tree = b.tree();
        assert_eq!(kept(&tree), vec![row]);
        assert_eq!(reason(&tree, section), Some(Reason::Wrapper));
    }

    #[test]
    fn three_nested_boxes_of_one_size_are_one_tag() {
        // The middle loses to the outer; the inner outranks the middle but
        // not the outer, so only the outer stays.
        let mut b = Builder::new();
        let outer = b.add(
            Some(0),
            "button",
            "Save",
            &["press"],
            Rect::new(10, 10, 80, 30),
        );
        let middle = b.add(
            Some(outer),
            "label",
            "",
            &["click"],
            Rect::new(10, 10, 80, 30),
        );
        let inner = b.add(
            Some(middle),
            "label",
            "Save",
            &["click"],
            Rect::new(10, 10, 80, 30),
        );
        let tree = b.tree();
        assert_eq!(kept(&tree), vec![outer]);
        assert_eq!(reason(&tree, middle), Some(Reason::Duplicate));
        assert_eq!(reason(&tree, inner), Some(Reason::Duplicate));
    }
}
