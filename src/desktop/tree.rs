//! The accessibility tree as the scan read it: plain data, no D-Bus.
//!
//! This is the seam between fetching (`atspi.rs`, async, talks to apps) and
//! deciding (`filter.rs`, pure, tested on saved files). `wayhint dump --raw`
//! writes a [`Tree`] as JSON, and the filter tests read those files back, so
//! a filter change is checked against real windows without opening them.

use serde::{Deserialize, Serialize};

use crate::{desktop::Window, geometry::Rect};

/// Where an accessible object lives on the bus: the app's unique bus name
/// (`:1.42`) and the object's path. Enough to ask the same object again at
/// re-check time, instead of guessing from a position.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId {
    pub bus: String,
    pub path: String,
}

/// The AT-SPI states the filter reads, as named fields. AT-SPI has about 40;
/// the rest never decide anything here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // they are independent flags, not a state machine
pub struct States {
    /// Could be seen: mapped, and every ancestor too. Not "uncovered".
    pub showing: bool,
    pub visible: bool,
    pub enabled: bool,
    pub sensitive: bool,
    pub focusable: bool,
    pub editable: bool,
    pub selectable: bool,
    /// The window that has focus in its app (frames only).
    pub active: bool,
}

/// What the scan knows about one object. Everything past `component` is only
/// read when it's worth a request, hence the `Option`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub object: ObjectId,
    /// Index of the parent in [`Tree::nodes`]; `None` for the frame.
    pub parent: Option<usize>,
    /// AT-SPI's English role name: `"button"`, `"check box"`, `"switch"`.
    pub role: String,
    pub states: States,
    /// Implements the Action interface (lists actions it can do).
    pub action: bool,
    /// Implements the Component interface (has a box on screen).
    pub component: bool,
    /// Implements the Text interface (has text text mode can read). Absent
    /// in trees saved before it was kept.
    #[serde(default)]
    pub text: bool,
    /// Box relative to the window: read for candidates, scroll areas, and
    /// inside web documents (where it decides whether to walk in).
    pub bounds: Option<Rect>,
    /// Name and action names: read for candidates only.
    pub details: Option<Details>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Details {
    pub name: String,
    pub actions: Vec<String>,
}

/// One scan's raw result. Nodes are in walk order (breadth first), so a
/// parent always comes before its children.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    pub window: Window,
    pub nodes: Vec<Node>,
}

impl Tree {
    /// Indices of `index`'s ancestors, nearest first, up to the frame.
    ///
    /// `std::iter::successors` builds an iterator from a first item and a
    /// "next from this one" closure: here, parent of parent of parent. The
    /// `'_` says the iterator borrows the tree, so it can't outlive it.
    pub fn ancestors(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.nodes[index].parent, move |&p| self.nodes[p].parent)
    }

    /// Whether `ancestor` is somewhere above `index`.
    pub fn is_inside(&self, index: usize, ancestor: usize) -> bool {
        self.ancestors(index).any(|a| a == ancestor)
    }
}
