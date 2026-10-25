//! The scan's output: which targets the windows on a monitor have, where,
//! and what they are. Most come from a window's accessibility tree; a
//! window without one is tagged from its pixels.
//!
//! The snapshot is the one seam between the parts. `wayhint dump` writes
//! it as JSON, `wayhint overlay --from snap.json` draws it, the re-check
//! reads its targets. Plain data with `Serialize` and `Deserialize`, so
//! every part can run alone from a saved file.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{
    desktop::{
        Stats, Window,
        filter::{self, Filtered},
        tree::{ObjectId, Tree},
    },
    geometry::{Point, Rect},
    pixels::{Block, Corners, Found},
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
    /// Tagged from its pixels: it has no accessibility tree.
    #[serde(default)]
    pub pixels: bool,
}

/// Something that gets a tag. Global logical coordinates throughout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The accessible object, to ask again before clicking. `None` for a
    /// tag found in the window's pixels: there's nothing to ask.
    #[serde(default)]
    pub object: Option<ObjectId>,
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
    /// The scroll areas above it, nearest first, whose boxes clipped it
    /// (see `filter::clip`). The re-check reads them again: a pane that
    /// shrank since the scan can leave the hit point out of view while
    /// the target's own box stays the same.
    #[serde(default)]
    pub clips: Vec<ObjectId>,
    /// A block of text found in the pixels: its tag opens into tags on
    /// these lines instead of acting (see [`Snapshot::open`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opens: Vec<Rect>,
}

/// What a scan learnt about one window, for [`Snapshot::build`].
pub enum Read<'a> {
    /// Its accessibility tree, and what the filter kept of it.
    Tree(&'a Tree, &'a Filtered, &'a Stats),
    /// What the pixel detector found in its picture, which had `scale`
    /// pixels per logical pixel.
    Pixels {
        window: &'a Window,
        found: &'a Found,
        scale: f64,
        stats: &'a Stats,
    },
}

/// The roles pixel tags carry, for `dump` and whoever reads a snapshot:
/// a phrase or control, a block that opens, a picture or video.
const PIXELS: &str = "pixels";
const PIXEL_BLOCK: &str = "pixel block";
const PIXEL_MEDIA: &str = "pixel media";

impl Snapshot {
    /// Turns what each window gave into targets, moving every box from
    /// window-relative to global. `reads` go bottom to top, as the windows
    /// stack.
    pub fn build<'a>(scan: u32, monitor: i64, reads: impl IntoIterator<Item = Read<'a>>) -> Self {
        let mut windows = Vec::new();
        let mut targets = Vec::new();
        for (window, read) in reads.into_iter().enumerate() {
            let (tree, filtered, stats) = match read {
                Read::Tree(tree, filtered, stats) => (tree, filtered, stats),
                Read::Pixels {
                    window: seen,
                    found,
                    scale,
                    stats,
                } => {
                    targets.extend(pixel_targets(window, seen.rect.origin(), scale, found));
                    windows.push(Scanned {
                        window: seen.clone(),
                        stats: stats.clone(),
                        pixels: true,
                    });
                    continue;
                }
            };
            let origin = tree.window.rect.origin();
            let scrollers = filter::scroll_areas(tree);
            targets.extend(filtered.kept.iter().map(|kept| {
                let node = &tree.nodes[kept.node];
                // The filter only keeps nodes with a box and details.
                let bounds = node.bounds.unwrap_or(kept.visible);
                Target {
                    object: Some(node.object.clone()),
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
                    clips: tree
                        .ancestors(kept.node)
                        .filter(|a| scrollers.contains(a))
                        .map(|a| tree.nodes[a].object.clone())
                        .collect(),
                    opens: Vec::new(),
                }
            }));
            windows.push(Scanned {
                window: tree.window.clone(),
                stats: stats.clone(),
                pixels: false,
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

    /// The snapshot with the block at `index` opened: its lines are the
    /// targets now, and nothing else is.
    pub fn open(&self, index: usize) -> Self {
        let block = &self.targets[index];
        let mut targets: Vec<Target> = block
            .opens
            .iter()
            .map(|&rect| pixel_target(block.window, rect, PIXELS))
            .collect();
        targets.sort_by_key(|t| (t.hit.y, t.hit.x));
        Self {
            targets,
            ..self.clone()
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} isn't a wayhint snapshot", path.display()))
    }
}

/// The targets of a window read from its pixels. The boxes are in the
/// picture's pixels, `scale` of them per logical pixel, from the window's
/// `origin`.
fn pixel_targets(window: usize, origin: Point, scale: f64, found: &Found) -> Vec<Target> {
    #[allow(clippy::cast_possible_truncation)] // window pixels are far inside i32
    let rect = |b: Corners| {
        let at = |v: i32| (f64::from(v) / scale).round() as i32;
        let (x, y) = (at(b[0]), at(b[1]));
        Rect::new(x, y, at(b[2]) - x, at(b[3]) - y).offset(origin)
    };
    let direct = found
        .direct
        .iter()
        .map(|&b| pixel_target(window, rect(b), PIXELS));
    let blocks = found.blocks.iter().map(|block: &Block| Target {
        opens: block.phrases.iter().map(|&b| rect(b)).collect(),
        ..pixel_target(window, rect(block.rect), PIXEL_BLOCK)
    });
    let media = found
        .media
        .iter()
        .map(|&b| pixel_target(window, rect(b), PIXEL_MEDIA));
    direct.chain(blocks).chain(media).collect()
}

/// A target with nothing behind it but its box, clicked in the middle.
fn pixel_target(window: usize, rect: Rect, role: &str) -> Target {
    Target {
        object: None,
        window,
        rect,
        hit: rect.center(),
        role: role.to_string(),
        name: String::new(),
        editable: false,
        clips: Vec::new(),
        opens: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::Kind;

    fn window() -> Window {
        Window {
            address: "0x1".into(),
            pid: 1,
            class: "kitty".into(),
            title: String::new(),
            rect: Rect::new(100, 50, 800, 600),
            monitor: 0,
            focused: true,
            kind: Kind::Client,
            frames: Vec::new(),
        }
    }

    #[test]
    fn pixel_boxes_land_in_global_logical_coordinates() {
        let found = Found {
            direct: vec![[20, 40, 60, 60]],
            blocks: vec![Block {
                rect: [0, 100, 300, 160],
                phrases: vec![[0, 100, 300, 120], [0, 140, 200, 160]],
            }],
            media: vec![[400, 0, 800, 400]],
        };
        let window = window();
        let stats = Stats::default();
        // A picture at 1.5 pixels per logical pixel.
        let read = Read::Pixels {
            window: &window,
            found: &found,
            scale: 1.5,
            stats: &stats,
        };
        let snapshot = Snapshot::build(1, 0, [read]);
        assert!(snapshot.windows[0].pixels);
        let roles: Vec<&str> = snapshot.targets.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            ["pixels", "pixel block", "pixel media"],
            "in reading order, by hit point"
        );
        let word = &snapshot.targets[0];
        assert_eq!(word.rect, Rect::new(113, 77, 27, 13));
        assert_eq!(word.hit, Point::new(126, 83));
        assert_eq!(word.object, None);

        let opened = snapshot.open(1);
        let lines: Vec<Rect> = opened.targets.iter().map(|t| t.rect).collect();
        assert_eq!(
            lines,
            [Rect::new(100, 117, 200, 13), Rect::new(100, 143, 133, 14)]
        );
        assert!(
            opened
                .targets
                .iter()
                .all(|t| t.opens.is_empty() && t.window == 0)
        );
    }

    #[test]
    fn snapshots_from_before_pixels_still_load() {
        let old = r#"{"object":{"bus":":1.7","path":"/a"},"window":0,"rect":{"x":0,"y":0,"w":9,"h":9},
                      "hit":{"x":4,"y":4},"role":"button","name":"OK","editable":false}"#;
        let target: Target = serde_json::from_str(old).unwrap();
        assert!(target.object.is_some() && target.opens.is_empty());
    }
}
