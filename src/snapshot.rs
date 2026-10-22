//! The scan's output: which targets one window has, where, and what they are.
//!
//! The snapshot is the one seam between the parts. `wayhint dump` writes
//! it as JSON, `wayhint overlay --from snap.json` draws it, the re-check
//! reads its targets. Plain data with `Serialize` and `Deserialize`, so
//! every part can run alone from a saved file.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{
    desktop::{Stats, Window, filter::Filtered, tree::ObjectId, tree::Tree},
    geometry::{Point, Rect},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Counts up per run, so a result from an older scan is never taken for
    /// the current one.
    pub scan: u32,
    pub window: Window,
    /// In reading order: top to bottom, then left to right.
    pub targets: Vec<Target>,
    pub stats: Stats,
}

/// Something that gets a tag. Global logical coordinates throughout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The accessible object, to ask again before clicking.
    pub object: ObjectId,
    /// The whole box, as the app reports it.
    pub rect: Rect,
    /// Where a click goes: the centre of the box's visible part.
    pub hit: Point,
    pub role: String,
    pub name: String,
    /// A text field: clicking it ends the mode, so typing goes to the app.
    pub editable: bool,
}

impl Snapshot {
    /// Turns the filter's verdict on a tree into targets, moving every box
    /// from window-relative to global.
    pub fn build(scan: u32, tree: &Tree, filtered: &Filtered, stats: Stats) -> Self {
        let origin = tree.window.rect.origin();
        let mut targets: Vec<Target> = filtered
            .kept
            .iter()
            .map(|kept| {
                let node = &tree.nodes[kept.node];
                // The filter only keeps nodes with a box and details.
                let bounds = node.bounds.unwrap_or(kept.visible);
                Target {
                    object: node.object.clone(),
                    rect: bounds.offset(origin),
                    hit: kept.visible.offset(origin).center(),
                    role: node.role.clone(),
                    name: node
                        .details
                        .as_ref()
                        .map(|d| d.name.clone())
                        .unwrap_or_default(),
                    editable: node.states.editable,
                }
            })
            .collect();
        // `sort_by_key` with a tuple sorts by y first, then x: tuples compare
        // field by field.
        targets.sort_by_key(|t| (t.hit.y, t.hit.x));
        Self {
            scan,
            window: tree.window.clone(),
            targets,
            stats,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} isn't a wayhint snapshot", path.display()))
    }
}
