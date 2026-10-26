//! The scan's output: which targets the windows on a monitor have, where,
//! and what they are. Most come from a window's accessibility tree; a
//! window without one is tagged from its pixels.
//!
//! The snapshot is the one seam between the parts. `wayhint dump` writes
//! it as JSON, `wayhint overlay --from snap.json` draws it, the re-check
//! reads its targets. Plain data with `Serialize` and `Deserialize`, so
//! every part can run alone from a saved file.

use std::{collections::HashMap, path::Path};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{
    desktop::{
        Kind, Stats, Window,
        filter::{self, Filtered},
        tree::{Node, ObjectId, Tree},
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
    /// Captured ink and controls that displaced labels must not cover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obstacles: Vec<Rect>,
    /// The objects with text the windows show, in walk order: what text
    /// mode reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub texts: Vec<TextSource>,
}

/// An object with text, as the scan found it. Global coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSource {
    pub object: ObjectId,
    /// Its window, as an index into [`Snapshot::windows`].
    pub window: usize,
    pub role: String,
    pub editable: bool,
    /// It has no Text interface: its name is the text it shows (see
    /// `filter::has_text`), and where a match is in it can't be asked.
    #[serde(default)]
    pub named: bool,
    /// The part of the screen it can show in: its window, cut by the
    /// scroll areas above it.
    pub area: Rect,
    /// The scroll areas above it, nearest first, as a target's `clips`.
    #[serde(default)]
    pub clips: Vec<ObjectId>,
    /// A button's name may be letters or an icon: `Desktop::layout` keeps
    /// it only where its picture shows letters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<Button>,
}

/// What `Desktop::layout` checks a button's picture by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Button {
    pub rect: Rect,
    /// Its name's length in characters.
    pub letters: usize,
}

impl Button {
    /// A button's, for a node `filter::is_button` says may be one.
    fn of(node: &Node, origin: Point) -> Option<Self> {
        if !filter::is_button(node) {
            return None;
        }
        Some(Self {
            rect: node.bounds?.offset(origin),
            letters: node.details.as_ref()?.name.trim().chars().count(),
        })
    }
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
    /// The part of `rect` in view, inside its window and scroll areas:
    /// what the tag is placed on. Empty in a snapshot saved before it was
    /// kept; [`Target::shown`] reads `rect` then.
    #[serde(default)]
    pub visible: Rect,
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

impl Target {
    /// Found in its window's pixels: a guess, drawn in `[style.pixels]`.
    pub fn in_pixels(&self) -> bool {
        self.object.is_none()
            && [PIXELS, PIXEL_BODY, PIXEL_BLOCK, PIXEL_MEDIA, "terminal"]
                .contains(&self.role.as_str())
    }

    /// The box its tag is placed on: the part in view.
    pub fn shown(&self) -> Rect {
        if self.visible.is_empty() {
            self.rect
        } else {
            self.visible
        }
    }
}

/// A target as [`Snapshot::survivors`] recognises it across scans.
#[derive(PartialEq, Eq, Hash)]
enum Same<'a> {
    Object(&'a ObjectId),
    /// The window's address and the box within the window.
    Pixels(&'a str, Rect),
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
    /// Nothing to tag, but text to read: kitty, which text mode asks
    /// itself what it shows.
    Untagged(&'a Window),
}

/// The roles pixel tags carry, for `dump` and whoever reads a snapshot:
/// a phrase or control, a place to type into, a block that opens, a
/// picture or video.
const PIXELS: &str = "pixels";
const PIXEL_BODY: &str = "pixel body";
const PIXEL_BLOCK: &str = "pixel block";
const PIXEL_MEDIA: &str = "pixel media";

impl Snapshot {
    /// Turns what each window gave into targets, moving every box from
    /// window-relative to global. `reads` go bottom to top, as the windows
    /// stack.
    pub fn build<'a>(scan: u32, monitor: i64, reads: impl IntoIterator<Item = Read<'a>>) -> Self {
        let mut windows = Vec::new();
        let mut targets = Vec::new();
        let mut texts = Vec::new();
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
                Read::Untagged(seen) => {
                    windows.push(Scanned {
                        window: seen.clone(),
                        stats: Stats::default(),
                        pixels: false,
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
                let visible = kept.visible.offset(origin);
                Target {
                    object: Some(node.object.clone()),
                    window,
                    rect: bounds.offset(origin),
                    visible,
                    hit: visible.center(),
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
            // A menu's items are read again in the app's own coordinates,
            // which aren't the menu's (Firefox): text mode leaves them out.
            let sources = match tree.window.kind {
                Kind::Menu => Vec::new(),
                _ => filter::text_sources(tree),
            };
            texts.extend(sources.into_iter().map(|(node, area)| {
                let read = &tree.nodes[node];
                TextSource {
                    object: read.object.clone(),
                    window,
                    role: read.role.clone(),
                    editable: read.states.editable,
                    named: !read.text,
                    area: area.offset(origin),
                    clips: tree
                        .ancestors(node)
                        .filter(|a| scrollers.contains(a))
                        .map(|a| tree.nodes[a].object.clone())
                        .collect(),
                    button: Button::of(read, origin),
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
            obstacles: Vec::new(),
            texts,
        }
    }

    /// Keeps the target nearest each group's average hit point. Centers
    /// within `distance` logical px join the same group, including chains
    /// of neighbors. Ties keep the earlier target; retained targets stay
    /// in their original order, with their click behavior unchanged.
    pub fn merge_nearby(&mut self, distance: u16) {
        if distance == 0 || self.targets.len() < 2 {
            return;
        }
        let distance = i64::from(distance);
        let cell = |point: Point| {
            (
                i64::from(point.x).div_euclid(distance),
                i64::from(point.y).div_euclid(distance),
            )
        };
        let mut cells: HashMap<_, Vec<usize>> = HashMap::new();
        for (i, target) in self.targets.iter().enumerate() {
            cells.entry(cell(target.hit)).or_default().push(i);
        }
        let mut seen = vec![false; self.targets.len()];
        let mut keep = vec![false; self.targets.len()];
        for start in 0..self.targets.len() {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let mut group = vec![start];
            let mut at = 0;
            // Remove grouped targets from the cells, so dense duplicates
            // don't compare every pair of centers.
            while at < group.len() {
                let hit = self.targets[group[at]].hit;
                let (cx, cy) = cell(hit);
                for x in cx - 1..=cx + 1 {
                    for y in cy - 1..=cy + 1 {
                        let Some(nearby) = cells.get_mut(&(x, y)) else {
                            continue;
                        };
                        nearby.retain(|&i| {
                            if seen[i] {
                                return false;
                            }
                            let other = self.targets[i].hit;
                            let dx = i64::from(hit.x) - i64::from(other.x);
                            let dy = i64::from(hit.y) - i64::from(other.y);
                            if dx * dx + dy * dy <= distance * distance {
                                seen[i] = true;
                                group.push(i);
                                return false;
                            }
                            true
                        });
                    }
                }
                at += 1;
            }
            let (sx, sy, count) = group.iter().fold((0.0, 0.0, 0.0), |(x, y, n), &i| {
                let hit = self.targets[i].hit;
                (x + f64::from(hit.x), y + f64::from(hit.y), n + 1.0)
            });
            let score = |i: usize| {
                let hit = self.targets[i].hit;
                (f64::from(hit.x) - sx / count).powi(2) + (f64::from(hit.y) - sy / count).powi(2)
            };
            let central = group
                .into_iter()
                .min_by(|&a, &b| score(a).total_cmp(&score(b)).then(a.cmp(&b)))
                .expect("each group starts with one target");
            keep[central] = true;
        }
        let mut at = 0;
        self.targets.retain(|_| {
            let kept = keep[at];
            at += 1;
            kept
        });
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

    /// For each target, where the same one is in `before`, if it was there:
    /// the same accessible object, or, for a target found in pixels, the
    /// same box in the same window, wherever the window moved. A rescan
    /// gives these their old tags.
    pub fn survivors(&self, before: &Self) -> Vec<Option<usize>> {
        let old: HashMap<Same, usize> = before
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| (before.same(t), i))
            .collect();
        self.targets
            .iter()
            .map(|t| old.get(&self.same(t)).copied())
            .collect()
    }

    /// What makes `target` the same one in another scan.
    fn same<'a>(&'a self, target: &'a Target) -> Same<'a> {
        if let Some(object) = &target.object {
            return Same::Object(object);
        }
        let window = &self.windows[target.window].window;
        let origin = window.rect.origin();
        let rect = target.rect.offset(Point::new(-origin.x, -origin.y));
        Same::Pixels(&window.address, rect)
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
    // Editable, as a text field is: the click ends the mode.
    let bodies = found.bodies.iter().map(|&b| Target {
        editable: true,
        ..pixel_target(window, rect(b), PIXEL_BODY)
    });
    let blocks = found.blocks.iter().map(|block: &Block| Target {
        opens: block.phrases.iter().map(|&b| rect(b)).collect(),
        ..pixel_target(window, rect(block.rect), PIXEL_BLOCK)
    });
    let media = found
        .media
        .iter()
        .map(|&b| pixel_target(window, rect(b), PIXEL_MEDIA));
    direct.chain(bodies).chain(blocks).chain(media).collect()
}

/// A target with nothing behind it but its box, clicked in the middle.
fn pixel_target(window: usize, rect: Rect, role: &str) -> Target {
    Target {
        object: None,
        window,
        rect,
        visible: rect,
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

    fn hints(points: &[(i32, i32)]) -> Snapshot {
        Snapshot {
            scan: 1,
            monitor: 0,
            windows: Vec::new(),
            obstacles: Vec::new(),
            texts: Vec::new(),
            targets: points
                .iter()
                .map(|&(x, y)| pixel_target(0, Rect::new(x - 1, y - 1, 2, 2), PIXELS))
                .collect(),
        }
    }

    #[test]
    fn a_body_is_editable_and_never_opens_lines_and_its_neighbours_are_not() {
        let window = window();
        let found = Found {
            direct: vec![[10, 10, 40, 30]],
            bodies: vec![[320, 760, 356, 784]],
            ..Found::default()
        };
        let snapshot = Snapshot::build(
            1,
            window.monitor,
            [Read::Pixels {
                window: &window,
                found: &found,
                scale: 1.0,
                stats: &Stats::default(),
            }],
        );
        let [control, body] = &snapshot.targets[..] else {
            panic!("{:?}", snapshot.targets);
        };
        assert_eq!((control.role.as_str(), control.editable), ("pixels", false));
        assert_eq!(body.role, "pixel body");
        assert!(body.editable && body.opens.is_empty() && body.object.is_none());
    }

    #[test]
    fn nearby_hints_keep_the_existing_target_nearest_the_average_center() {
        let mut snapshot = hints(&[(0, 0), (4, 0), (3, 1), (20, 0)]);
        let central = &mut snapshot.targets[2];
        central.object = Some(ObjectId {
            bus: ":1.7".into(),
            path: "/central".into(),
        });
        central.editable = true;
        central.opens = vec![Rect::new(1, 1, 2, 2)];
        let expected = vec![central.clone(), snapshot.targets[3].clone()];
        snapshot.merge_nearby(5);
        assert_eq!(snapshot.targets, expected);
        snapshot.merge_nearby(5);
        assert_eq!(snapshot.targets, expected, "merging twice changes nothing");
    }

    #[test]
    fn merging_uses_inclusive_euclidean_distance_and_can_be_disabled() {
        for (points, distance, expected) in [
            (vec![(0, 0), (3, 4)], 5, 1),
            (vec![(0, 0), (4, 4)], 5, 2),
            (vec![(0, 0), (6, 0)], 5, 2),
            (vec![(0, 0), (6, 0)], 6, 1),
            (vec![(0, 0), (0, 0)], 0, 2),
            (vec![(0, 0), (0, 0)], 5, 1),
            (vec![], 5, 0),
            (vec![(0, 0)], 5, 1),
        ] {
            let mut snapshot = hints(&points);
            snapshot.merge_nearby(distance);
            assert_eq!(snapshot.targets.len(), expected, "{points:?}, {distance}");
        }
    }

    #[test]
    fn neighboring_chains_form_one_group_with_deterministic_ties() {
        let mut snapshot = hints(&[(0, 0), (4, 0), (8, 0), (12, 0), (30, 0)]);
        let expected = vec![snapshot.targets[1].clone(), snapshot.targets[4].clone()];
        snapshot.merge_nearby(5);
        assert_eq!(snapshot.targets, expected);
    }

    #[test]
    fn merging_works_across_negative_coordinates_and_cell_boundaries() {
        let mut snapshot = hints(&[(-6, -6), (-3, -2), (4, 0), (9, 0)]);
        let expected = vec![snapshot.targets[0].clone(), snapshot.targets[2].clone()];
        snapshot.merge_nearby(5);
        assert_eq!(snapshot.targets, expected);
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
            bodies: Vec::new(),
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
    fn targets_survive_by_object_or_by_box_in_their_window() {
        let found = |x| Found {
            direct: vec![[x, 0, x + 20, 20]],
            ..Found::default()
        };
        let stats = Stats::default();
        let scan = |window: &Window, found: &Found| {
            let mut snapshot = Snapshot::build(
                1,
                0,
                [Read::Pixels {
                    window,
                    found,
                    scale: 1.0,
                    stats: &stats,
                }],
            );
            // One target from a tree, at the end, in the same window.
            let mut button = snapshot.targets[0].clone();
            button.object = Some(ObjectId {
                bus: ":1.7".into(),
                path: "/ok".into(),
            });
            snapshot.targets.push(button);
            snapshot
        };
        let before = scan(&window(), &found(0));
        // The window moved; one box is new, one is the same within it.
        let moved = Window {
            rect: Rect::new(300, 300, 800, 600),
            ..window()
        };
        let mut after = scan(&moved, &found(40));
        after.targets.insert(0, before.targets[0].clone());
        after.targets[0].rect = after.targets[0].rect.offset(Point::new(200, 250));
        assert_eq!(after.survivors(&before), [Some(0), None, Some(1)]);
        let elsewhere = Window {
            address: "0x2".into(),
            ..window()
        };
        assert_eq!(
            scan(&elsewhere, &found(0)).survivors(&before),
            [None, Some(1)],
            "the same box in another window is another target"
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
