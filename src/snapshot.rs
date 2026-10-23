//! The scan's output: which targets the windows on a monitor have, where,
//! and what they are.
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
    /// The monitor the windows are on, as in `Monitor::id`.
    pub monitor: i64,
    /// The windows scanned, bottom to top.
    pub windows: Vec<Scanned>,
    /// In reading order across all windows: top to bottom, then left to
    /// right.
    pub targets: Vec<Target>,
}

/// One window, and what scanning it cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scanned {
    pub window: Window,
    pub stats: Stats,
}

/// Something that gets a tag. Global logical coordinates throughout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The accessible object, to ask again before clicking.
    pub object: ObjectId,
    /// Its window, as an index into [`Snapshot::windows`].
    pub window: usize,
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
    /// Turns the filter's verdict on each window's tree into targets,
    /// moving every box from window-relative to global. `scans` go bottom
    /// to top, as the windows stack.
    pub fn build<'a>(
        scan: u32,
        monitor: i64,
        scans: impl IntoIterator<Item = (&'a Tree, &'a Filtered, &'a Stats)>,
    ) -> Self {
        let mut windows = Vec::new();
        let mut targets = Vec::new();
        for (window, (tree, filtered, stats)) in scans.into_iter().enumerate() {
            let origin = tree.window.rect.origin();
            targets.extend(filtered.kept.iter().map(|kept| {
                let node = &tree.nodes[kept.node];
                // The filter only keeps nodes with a box and details.
                let bounds = node.bounds.unwrap_or(kept.visible);
                Target {
                    object: node.object.clone(),
                    window,
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
            }));
            windows.push(Scanned {
                window: tree.window.clone(),
                stats: stats.clone(),
            });
        }
        // `sort_by_key` with a tuple sorts by y first, then x: tuples compare
        // field by field.
        targets.sort_by_key(|t| (t.hit.y, t.hit.x));
        Self {
            scan,
            monitor,
            windows,
            targets,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} isn't a wayhint snapshot", path.display()))
    }
}
