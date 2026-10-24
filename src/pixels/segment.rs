//! The colour pass: Neru's segment detector (`segment/*.go`) in its
//! `words` mode, at scale 1. Where a neighbour differs by a few shades in
//! any channel there is ink; blobs of ink are sorted into lines, pictures,
//! frames and glyphs, and glyphs on one line, in one colour, close
//! together, are a word.
//!
//! It sees what the edge pass misses: faint text, and edges between two
//! colours of the same brightness, which luma erases. Ported like
//! [`super::contour`]: the Go's boxes exactly, some steps faster another
//! way, tested against its output. One change: Go returned the words in
//! map order, which varies per run, and this returns them by label.

use std::collections::{BTreeMap, HashMap};

use super::{
    Corners, Frame,
    runs::{self, Runs},
};

/// The smallest per-channel step between neighbours that is ink: low
/// enough for a 1 px border a few shades off its background.
const INK_STEP: u8 = 8;
/// Texture tiles: side in px; the floor is the step a quarter of a tile
/// stays under; a textured tile counts as ink only steps 3x its floor.
const NOISE_TILE: usize = 16;
const NOISE_QUANTILE: usize = 4;
const NOISE_FACTOR: usize = 3;
const NOISE_BINS: usize = 64;
/// The step that is still ink on a photo, and how dense a blob too big to
/// be one target must be to be one.
const OVERLAY_STEP: u8 = 56;
const PHOTO_FILL: f64 = 0.5;
/// Rows read at a blob's top and bottom to tell a frame from glyphs.
const RIM: usize = 2;

// The size rules, in px (Neru's limits at scale 1).
/// The tallest frame that is one control.
const MAX_CONTROL_HEIGHT: i32 = 64;
/// Wider than this is layout.
const MAX_TARGET_WIDTH: i32 = 650;
/// A picture that is one target (an avatar), and the smallest one.
const MAX_PICTURE_SIDE: i32 = 5 * MAX_CONTROL_HEIGHT;
const MIN_PICTURE_SIDE: i32 = 16;
/// Dense ink: text leaves a third of its box empty or more.
const PICTURE_FILL: f64 = 0.85;
/// The smallest frame, and how much of each side its ink covers.
const MIN_FRAME_SIDE: i32 = 10;
const FRAME_COVER: f64 = 0.7;
/// A big blob this sparse is crossing rules, not a glyph.
const LINE_ART_FILL: f64 = 0.1;
/// A separator: this long, this thin.
const LINE_LENGTH: i32 = 12;
const LINE_THICKNESS: i32 = 2;
/// Specks.
const MIN_TARGET_AREA: i32 = 12;
/// How far a dot, accent or underline sits from its glyph.
const MARK_GAP: i32 = 2;
/// The widest gap inside a word, in glyph heights.
const WORD_GAP: f64 = 0.4;
/// Glyphs too small to compare ink; ink counts that are a mark.
const SMALL_GLYPH: i32 = 6;
const SMALL_COUNT: i32 = 12;
/// A child this share of its holder's height makes the holder a shell.
const SHELL_CHILD: i32 = 4;
/// Two glyphs are one colour above this cosine, and when their ink
/// lengths are within this ratio (white from grey). Squared, as Go folds
/// its constants: `0.97 * 0.97` in f64 is a different number.
const INK_COS_SQUARED: f64 = 0.9409;
const INK_RATIO_SQUARED: f64 = 0.25;
/// The largest background difference inside one word.
const BG_MATCH: i32 = 40;
/// Nested targets this close (px, or a fifth of the outer's shorter side)
/// are one.
const COINCIDENT_SLACK: i32 = 3;
const COINCIDENT_SHARE: i32 = 5;
/// The spatial index's bucket, in px.
const GRID_CELL: i32 = 32;

type Rgb = [i32; 3];

/// One 8-connected run of ink. Bounds are inclusive.
#[derive(Clone)]
struct Blob {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
    count: i32,
    /// The pixel left of the first run, and the ink farthest from it.
    bg: Rgb,
    ink: Rgb,
    ink_dist: i32,
    /// Ink in the two outer rows at the top and the bottom, and rows with
    /// ink in a side band half the shorter side deep.
    top: [i32; RIM],
    bottom: [i32; RIM],
    left_rows: i32,
    right_rows: i32,
    last_left: i32,
    last_right: i32,
}

impl Blob {
    const fn width(&self) -> i32 {
        self.max_x - self.min_x + 1
    }

    const fn height(&self) -> i32 {
        self.max_y - self.min_y + 1
    }

    /// The box as the ink sits: the mask marks the pixel before each step,
    /// so the marked box starts one pixel early.
    const fn rect(&self) -> Corners {
        [
            self.min_x + 1,
            self.min_y + 1,
            self.max_x + 1,
            self.max_y + 1,
        ]
    }

    const fn encloses(&self, o: &Self) -> bool {
        self.min_x <= o.min_x
            && self.max_x >= o.max_x
            && self.min_y <= o.min_y
            && self.max_y >= o.max_y
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Glyph,
    Line,
    Picture,
    Control,
    Container,
}

/// The words, controls and small pictures, in px.
pub fn boxes(frame: &Frame) -> Vec<Corners> {
    let (w, h) = (frame.width, frame.height);
    let steps = steps(frame);
    let mut mask: Vec<bool> = steps.iter().map(|&v| v >= INK_STEP).collect();
    let (mut blobs, ink) = components(frame, &mask);

    // Texture raised some tile's threshold: read again through the quieter
    // mask, keeping the plain one inside pictures so an avatar stays whole.
    // A dense blob too big to be one target is a photo: read it again for
    // what is drawn on it.
    let photos: Vec<bool> = blobs.iter().map(is_photo).collect();
    let any_photo = photos.contains(&true);
    let quiet = noise_mask(&steps, w, h);
    let raised = quiet.is_some();
    if let Some(mut quiet) = quiet {
        for b in blobs.iter().filter(|b| is_picture(b)) {
            for y in b.min_y..=b.max_y {
                let row = y as usize * w;
                let (x0, x1) = (row + b.min_x as usize, row + b.max_x as usize + 1);
                quiet[x0..x1].copy_from_slice(&mask[x0..x1]);
            }
        }
        mask = quiet;
    }
    if any_photo {
        for y in 0..h {
            for r in ink.row(y) {
                if photos[r.label] {
                    for x in r.x0 as usize..=r.x1 as usize {
                        mask[y * w + x] = steps[y * w + x] >= OVERLAY_STEP;
                    }
                }
            }
        }
    }
    if raised || any_photo {
        blobs = components(frame, &mask).0;
    }
    classify(&blobs)
}

/// For each pixel, the largest channel difference to its right or lower
/// neighbour: channels, not luma, keep an edge between two colours of
/// one brightness. A row at a time, per byte, then the largest of each
/// pixel's three: loops without branches, which the compiler vectorizes.
fn steps(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width, frame.height);
    let stride = 3 * w;
    let mut out = vec![0; w * h];
    let mut bytes = vec![0; stride];
    for (y, (row, out)) in frame
        .rgb
        .chunks_exact(stride)
        .zip(out.chunks_exact_mut(w))
        .enumerate()
    {
        for (s, (a, b)) in bytes.iter_mut().zip(row.iter().zip(&row[3..])) {
            *s = a.abs_diff(*b);
        }
        // The last pixel has no right neighbour.
        bytes[stride - 3..].fill(0);
        if y + 1 < h {
            let below = &frame.rgb[(y + 1) * stride..(y + 2) * stride];
            for (s, (a, b)) in bytes.iter_mut().zip(row.iter().zip(below)) {
                *s = (*s).max(a.abs_diff(*b));
            }
        }
        for (o, &[r, g, b]) in out.iter_mut().zip(bytes.as_chunks::<3>().0) {
            *o = r.max(g).max(b);
        }
    }
    out
}

/// Marks a pixel when it steps by its tile's threshold: the tile's floor
/// times [`NOISE_FACTOR`], between [`INK_STEP`] and [`OVERLAY_STEP`].
/// `None` when no tile's threshold rose, so the plain mask stands.
fn noise_mask(steps: &[u8], w: usize, h: usize) -> Option<Vec<bool>> {
    let cols = w.div_ceil(NOISE_TILE);
    let mut thresholds = vec![INK_STEP; cols * h.div_ceil(NOISE_TILE)];
    // A floor this low leaves the threshold at INK_STEP. Most tiles have
    // one, and a count of their quiet pixels says so without a histogram.
    let quiet = (usize::from(INK_STEP) / NOISE_FACTOR) as u8;
    for (t, threshold) in thresholds.iter_mut().enumerate() {
        let (x0, y0) = (t % cols * NOISE_TILE, t / cols * NOISE_TILE);
        let (x1, y1) = ((x0 + NOISE_TILE).min(w), (y0 + NOISE_TILE).min(h));
        let rows = || (y0..y1).map(|y| &steps[y * w + x0..y * w + x1]);
        let quarter = (y1 - y0) * (x1 - x0) / NOISE_QUANTILE;
        if rows()
            .map(|r| r.iter().filter(|&&v| v <= quiet).count())
            .sum::<usize>()
            > quarter
        {
            continue;
        }
        let mut histogram = [0usize; NOISE_BINS];
        for &v in rows().flatten() {
            histogram[usize::from(v).min(NOISE_BINS - 1)] += 1;
        }
        let mut seen = 0;
        let mut floor = 0;
        for (v, n) in histogram.iter().enumerate() {
            seen += n;
            if seen > quarter {
                floor = v;
                break;
            }
        }
        *threshold = (NOISE_FACTOR * floor).clamp(INK_STEP.into(), OVERLAY_STEP.into()) as u8;
    }
    if thresholds.iter().all(|&t| t == INK_STEP) {
        return None;
    }
    let mut mask = vec![false; w * h];
    for (y, (mask, steps)) in mask
        .chunks_exact_mut(w)
        .zip(steps.chunks_exact(w))
        .enumerate()
    {
        let tiles = &thresholds[y / NOISE_TILE * cols..];
        for ((mask, steps), &t) in mask
            .chunks_mut(NOISE_TILE)
            .zip(steps.chunks(NOISE_TILE))
            .zip(tiles)
        {
            for (m, &s) in mask.iter_mut().zip(steps) {
                *m = s >= t;
            }
        }
    }
    Some(mask)
}

/// Dense, and small enough to be one target: an avatar, a thumbnail.
fn is_picture(b: &Blob) -> bool {
    let (w, h) = (b.width(), b.height());
    w >= MIN_PICTURE_SIDE
        && h >= MIN_PICTURE_SIDE
        && w <= MAX_PICTURE_SIDE
        && h <= MAX_PICTURE_SIDE
        && f64::from(b.count) >= PICTURE_FILL * f64::from(w * h)
}

/// Dense and too big to be one target: a photo.
fn is_photo(b: &Blob) -> bool {
    let (w, h) = (b.width(), b.height());
    (w > MAX_PICTURE_SIDE || h > MAX_PICTURE_SIDE)
        && f64::from(b.count) >= PHOTO_FILL * f64::from(w * h)
}

/// Labels 8-connected ink, then measures every blob: box, ink and
/// background, then the ink along each side of the box. Blobs come in
/// raster order of their first run.
fn components(frame: &Frame, mask: &[bool]) -> (Vec<Blob>, Runs) {
    let (w, h) = (frame.width, frame.height);
    let ink = runs::label(mask, w, h);
    let mut blobs: Vec<Blob> = Vec::with_capacity(ink.count);
    for y in 0..h {
        let row = &frame.rgb[y * w * 3..(y + 1) * w * 3];
        let pixel = |x: usize| {
            [
                i32::from(row[x * 3]),
                i32::from(row[x * 3 + 1]),
                i32::from(row[x * 3 + 2]),
            ]
        };
        for run in ink.row(y) {
            // A blob's first run comes before any other blob's after it.
            if run.label == blobs.len() {
                blobs.push(Blob {
                    min_x: run.x0,
                    max_x: run.x1,
                    min_y: y as i32,
                    max_y: y as i32,
                    count: 0,
                    bg: pixel((run.x0 as usize).saturating_sub(1)),
                    ink: [0; 3],
                    ink_dist: -1,
                    top: [0; RIM],
                    bottom: [0; RIM],
                    left_rows: 0,
                    right_rows: 0,
                    last_left: -1,
                    last_right: -1,
                });
            }
            let b = &mut blobs[run.label];
            b.min_x = b.min_x.min(run.x0);
            b.max_x = b.max_x.max(run.x1);
            b.max_y = y as i32;
            b.count += run.x1 - run.x0 + 1;
            for x in run.x0 as usize..=run.x1 as usize {
                let p = pixel(x);
                let d = distance(p, b.bg);
                if d > b.ink_dist {
                    (b.ink_dist, b.ink) = (d, p);
                }
            }
        }
    }

    for y in 0..h {
        let yi = y as i32;
        for run in ink.row(y) {
            let b = &mut blobs[run.label];
            let n = run.x1 - run.x0 + 1;
            if let Some(k) = usize::try_from(yi - b.min_y).ok().filter(|&k| k < RIM) {
                b.top[k] += n;
            }
            if let Some(k) = usize::try_from(b.max_y - yi).ok().filter(|&k| k < RIM) {
                b.bottom[k] += n;
            }
            let depth = (RIM as i32).max(b.width().min(b.height()) / 2);
            if run.x0 < b.min_x + depth && b.last_left != yi {
                b.left_rows += 1;
                b.last_left = yi;
            }
            if run.x1 > b.max_x - depth && b.last_right != yi {
                b.right_rows += 1;
                b.last_right = yi;
            }
        }
    }
    (blobs, ink)
}

/// The sum of absolute channel differences.
fn distance(a: Rgb, b: Rgb) -> i32 {
    (0..3).map(|k| (a[k] - b[k]).abs()).sum()
}

/// Sorts blobs into lines, pictures, controls, containers and glyphs,
/// groups glyphs into words, and returns the targets: controls, small
/// pictures and words. What sits in a control or a picture is part of it.
#[allow(clippy::too_many_lines)] // one function, as Neru wrote it
fn classify(blobs: &[Blob]) -> Vec<Corners> {
    let mut kinds: Vec<Kind> = blobs
        .iter()
        .map(|b| {
            let (w, h) = (b.width(), b.height());
            let area = f64::from(w * h);
            // A separator, or crossing rules (a table grid).
            let separator = (h <= LINE_THICKNESS && w >= LINE_LENGTH)
                || (w <= LINE_THICKNESS && h >= LINE_LENGTH);
            let rules = w.max(h) >= 3 * LINE_LENGTH
                && !is_frame(b)
                && f64::from(b.count) < LINE_ART_FILL * area;
            if separator || rules {
                Kind::Line
            } else if w >= MIN_PICTURE_SIDE
                && h >= MIN_PICTURE_SIDE
                && f64::from(b.count) >= PICTURE_FILL * area
            {
                Kind::Picture
            } else {
                Kind::Glyph
            }
        })
        .collect();

    let holder = holders(blobs, &kinds);

    // A shell holding anything is a control when control-sized, else a
    // container. A shell is a frame, or an outline around a child of real
    // size (a round key, an icon tile). A glyph cluster around a stray dot
    // stays a glyph.
    for (i, &j) in holder.iter().enumerate() {
        let Some(j) = j.filter(|&j| kinds[j] == Kind::Glyph) else {
            continue;
        };
        let (child, shell) = (&blobs[i], &blobs[j]);
        let substantial =
            child.count > SMALL_COUNT && child.height() * SHELL_CHILD >= shell.height();
        if !substantial && !is_frame(shell) {
            continue;
        }
        kinds[j] = if fits(shell) {
            Kind::Control
        } else {
            Kind::Container
        };
    }

    // The surface a blob sits on: its nearest enclosing frame or picture.
    // Glyphs only group with glyphs on the same one.
    let scope: Vec<Option<usize>> = (0..blobs.len())
        .map(|i| {
            let mut j = holder[i];
            while let Some(k) = j.filter(|&k| kinds[k] == Kind::Glyph && !is_frame(&blobs[k])) {
                j = holder[k];
            }
            j
        })
        .collect();
    let surfaces = |i: usize| std::iter::successors(scope[i], |&j| scope[j]);

    let mut glyphs: Vec<bool> = (0..blobs.len())
        .map(|i| {
            let b = &blobs[i];
            kinds[i] == Kind::Glyph
                && !is_frame(b)
                && fits(b)
                && !surfaces(i).any(|j| kinds[j] == Kind::Picture)
        })
        .collect();
    drop_separators(blobs, &mut glyphs);
    let label = group_labels(blobs, &glyphs, &scope);

    // A control holding two or more things is a group of them (a tab
    // strip). Its words are one thing when they read as one caption.
    let mut words: HashMap<usize, HashMap<usize, Corners>> = HashMap::new();
    let mut frames: HashMap<usize, i32> = HashMap::new();
    for i in 0..blobs.len() {
        let Some(j) = scope[i].filter(|&j| kinds[j] == Kind::Control && kinds[i] != Kind::Line)
        else {
            continue;
        };
        if glyphs[i] {
            let word = words
                .entry(j)
                .or_default()
                .entry(label[i])
                .or_insert(blobs[i].rect());
            *word = union(*word, blobs[i].rect());
        } else {
            *frames.entry(j).or_default() += 1;
        }
    }
    for (j, kind) in kinds.iter_mut().enumerate() {
        let parts = frames.get(&j).copied().unwrap_or(0) + words.get(&j).map_or(0, captions);
        if *kind == Kind::Control && parts >= 2 {
            *kind = Kind::Container;
        }
    }

    let mut targets = Vec::with_capacity(blobs.len() / 4);
    let mut boxes: BTreeMap<usize, Corners> = BTreeMap::new();
    for (i, b) in blobs.iter().enumerate() {
        if surfaces(i).any(|j| matches!(kinds[j], Kind::Control | Kind::Picture)) {
            continue;
        }
        if glyphs[i] {
            let word = boxes.entry(label[i]).or_insert(b.rect());
            *word = union(*word, b.rect());
        } else if kinds[i] == Kind::Control
            || (kinds[i] == Kind::Glyph && is_frame(b) && fits(b))
            || (kinds[i] == Kind::Picture
                && b.height() <= MAX_PICTURE_SIDE
                && b.width() <= MAX_PICTURE_SIDE)
        {
            targets.push(b.rect());
        }
    }
    targets.extend(
        boxes
            .into_values()
            .filter(|r| width(*r) * height(*r) >= MIN_TARGET_AREA && width(*r) <= MAX_TARGET_WIDTH),
    );
    drop_coincident(targets)
}

const fn width(r: Corners) -> i32 {
    r[2] - r[0]
}

const fn height(r: Corners) -> i32 {
    r[3] - r[1]
}

const fn is_empty(r: Corners) -> bool {
    r[0] >= r[2] || r[1] >= r[3]
}

/// Go's `Rectangle.Union`: an empty box adds nothing.
fn union(a: Corners, b: Corners) -> Corners {
    if is_empty(a) {
        return b;
    }
    if is_empty(b) {
        return a;
    }
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// Go's `Rectangle.In`: an empty box is in anything.
const fn within(inner: Corners, outer: Corners) -> bool {
    is_empty(inner)
        || (outer[0] <= inner[0]
            && inner[2] <= outer[2]
            && outer[1] <= inner[1]
            && inner[3] <= outer[3])
}

/// Drops a target inside another with nearly its centre: a ring and the
/// glyph in it. Both would click one spot, so the outer one stays.
fn drop_coincident(targets: Vec<Corners>) -> Vec<Corners> {
    let centre = |r: Corners| (i32::midpoint(r[0], r[2]), i32::midpoint(r[1], r[3]));
    let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, &t) in targets.iter().enumerate() {
        let (x, y) = centre(t);
        cells
            .entry((x / GRID_CELL, y / GRID_CELL))
            .or_default()
            .push(i);
    }
    let mut dropped = vec![false; targets.len()];
    for (i, &outer) in targets.iter().enumerate() {
        let (ox, oy) = centre(outer);
        let slack = COINCIDENT_SLACK.max(width(outer).min(height(outer)) / COINCIDENT_SHARE);
        for cy in (oy - slack) / GRID_CELL..=(oy + slack) / GRID_CELL {
            for cx in (ox - slack) / GRID_CELL..=(ox + slack) / GRID_CELL {
                for &j in cells.get(&(cx, cy)).into_iter().flatten() {
                    let inner = targets[j];
                    if j == i || dropped[j] || inner == outer || !within(inner, outer) {
                        continue;
                    }
                    let (ix, iy) = centre(inner);
                    if (ix - ox).abs() <= slack && (iy - oy).abs() <= slack {
                        dropped[j] = true;
                    }
                }
            }
        }
    }
    targets
        .into_iter()
        .zip(dropped)
        .filter_map(|(t, d)| (!d).then_some(t))
        .collect()
}

/// How many captions a control's words make: words on one line with a gap
/// of about one space are one caption ("Save changes"); tabs in a strip
/// stand further apart.
fn captions(words: &HashMap<usize, Corners>) -> i32 {
    let boxes: Vec<Corners> = words.values().copied().collect();
    let mut parent: Vec<usize> = (0..boxes.len()).collect();
    let find = |parent: &[usize], mut a: usize| {
        while parent[a] != a {
            a = parent[a];
        }
        a
    };
    for (i, a) in boxes.iter().enumerate() {
        for (j, b) in boxes.iter().enumerate().skip(i + 1) {
            let overlap = a[3].min(b[3]) - a[1].max(b[1]);
            let gap = a[0].max(b[0]) - a[2].min(b[2]);
            if overlap * 2 >= height(*a).min(height(*b)) && gap * 2 <= height(*a).max(height(*b)) {
                let (rj, ri) = (find(&parent, j), find(&parent, i));
                parent[rj] = ri;
            }
        }
    }
    (0..boxes.len()).filter(|&i| find(&parent, i) == i).count() as i32
}

/// Small enough to be one target.
const fn fits(b: &Blob) -> bool {
    b.height() <= MAX_CONTROL_HEIGHT && b.width() <= MAX_TARGET_WIDTH
}

/// A closed outline: ink on [`FRAME_COVER`] of every side of its box.
/// Words cover their baseline, and only their ascenders reach the top.
fn is_frame(b: &Blob) -> bool {
    let (w, h) = (b.width(), b.height());
    if w < MIN_FRAME_SIDE || h < MIN_FRAME_SIDE {
        return false;
    }
    let covered = |ink: i32, length: i32| f64::from(ink) >= FRAME_COVER * f64::from(length);
    covered(b.top[0].max(b.top[1]), w)
        && covered(b.bottom[0].max(b.bottom[1]), w)
        && covered(b.left_rows, h)
        && covered(b.right_rows, h)
}

/// For each blob, the smallest other blob whose box strictly encloses it.
/// Lines hold nothing.
fn holders(blobs: &[Blob], kinds: &[Kind]) -> Vec<Option<usize>> {
    let index = Grid::new(blobs, |_| 0);
    blobs
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let own = b.width() * b.height();
            let mut best: Option<(i32, usize)> = None;
            for &j in index.at(
                i32::midpoint(b.min_x, b.max_x),
                i32::midpoint(b.min_y, b.max_y),
            ) {
                let o = &blobs[j];
                if j == i || kinds[j] == Kind::Line || !o.encloses(b) {
                    continue;
                }
                let area = o.width() * o.height();
                if area > own && best.is_none_or(|(least, _)| area < least) {
                    best = Some((area, j));
                }
            }
            best.map(|(_, j)| j)
        })
        .collect()
}

/// Takes thin strokes standing apart between two words on their line (the
/// `|` in "hide | 93 comments") out of the glyphs, so they never join two
/// words. A stroke inside a word sits a pixel from its neighbours.
fn drop_separators(blobs: &[Blob], glyphs: &mut [bool]) {
    let index = Grid::new(blobs, |_| MARK_GAP + 1);
    for (i, b) in blobs.iter().enumerate() {
        if !glyphs[i] || b.width() > LINE_THICKNESS + 1 || b.height() < SMALL_GLYPH + MARK_GAP {
            continue;
        }
        let reach = b.height();
        let (mut touching, mut left, mut right) = (false, false, false);
        for j in index.each(b.min_x - reach, b.min_y, b.max_x + reach, b.max_y) {
            let o = &blobs[j];
            if j == i || !glyphs[j] || o.max_y < b.min_y || o.min_y > b.max_y {
                continue;
            }
            let gap = o.min_x.max(b.min_x) - o.max_x.min(b.max_x) - 1;
            if gap <= MARK_GAP {
                touching = true;
            } else if gap <= reach && o.max_x < b.min_x {
                left = true;
            } else if gap <= reach && o.min_x > b.max_x {
                right = true;
            }
        }
        if !touching && left && right {
            glyphs[i] = false;
        }
    }
}

/// Unions glyphs on one surface that read as one word; each blob's label
/// is one member's index, shared by the group.
fn group_labels(blobs: &[Blob], glyphs: &[bool], scope: &[Option<usize>]) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..blobs.len()).collect();
    let find = |parent: &mut Vec<usize>, mut a: usize| {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    };
    let reach = |b: &Blob| (WORD_GAP * f64::from(b.height())) as i32 + MARK_GAP;
    let index = Grid::new(blobs, reach);
    for (i, a) in blobs.iter().enumerate() {
        if !glyphs[i] {
            continue;
        }
        let pad = reach(a);
        for j in index.each(a.min_x - pad, a.min_y - pad, a.max_x + pad, a.max_y + pad) {
            if j <= i
                || !glyphs[j]
                || scope[j] != scope[i]
                || find(&mut parent, i) == find(&mut parent, j)
            {
                continue;
            }
            if same_label(a, &blobs[j]) {
                let (rj, ri) = (find(&mut parent, j), find(&mut parent, i));
                parent[rj] = ri;
            }
        }
    }
    (0..blobs.len()).map(|i| find(&mut parent, i)).collect()
}

/// Two sibling glyphs are one word: a mark stacked on a glyph, or
/// neighbours on one line closer than [`WORD_GAP`] heights in one ink on
/// one background.
fn same_label(a: &Blob, b: &Blob) -> bool {
    if a.encloses(b) || b.encloses(a) {
        return false;
    }
    let gap_x = a.min_x.max(b.min_x) - a.max_x.min(b.max_x) - 1;
    let gap_y = a.min_y.max(b.min_y) - a.max_y.min(b.max_y) - 1;
    let (lo, hi) = (a.height().min(b.height()), a.height().max(b.height()));
    let mark = a.count.min(b.count) <= SMALL_COUNT || lo <= LINE_THICKNESS + 1 || lo * 3 <= hi;
    if gap_x < 0 && gap_y <= MARK_GAP && mark {
        return true;
    }
    // One line: overlapping by at least half the shorter height.
    if gap_y >= 0 || -gap_y * 2 < lo {
        return false;
    }
    // The boxes start a pixel early (see `Blob::rect`): the ink gap is one
    // wider, and the ink one shorter, than they say.
    let (ink_gap, ink_height) = (gap_x + 1, hi - 1);
    if f64::from(ink_gap) > (WORD_GAP * f64::from(ink_height)).max(f64::from(MARK_GAP))
        || distance(a.bg, b.bg) > BG_MATCH
    {
        return false;
    }
    lo < SMALL_GLYPH || same_ink(a, b)
}

/// The direction of each glyph's ink away from its background: blending
/// with the background shortens it without turning it, so a thin and a
/// thick stroke of one colour agree. Neutral inks share a direction, so
/// for glyphs with a fully covered pixel the lengths must agree too.
fn same_ink(a: &Blob, b: &Blob) -> bool {
    let (mut dot, mut na, mut nb) = (0.0, 0.0, 0.0);
    for k in 0..3 {
        let (va, vb) = (f64::from(a.ink[k] - a.bg[k]), f64::from(b.ink[k] - b.bg[k]));
        dot += va * vb;
        na += va * va;
        nb += vb * vb;
    }
    if dot <= 0.0 || dot * dot < INK_COS_SQUARED * na * nb {
        return false;
    }
    if a.count > SMALL_COUNT && b.count > SMALL_COUNT {
        return na.min(nb) >= INK_RATIO_SQUARED * na.max(nb);
    }
    true
}

/// Blob indices bucketed by the [`GRID_CELL`] squares their padded boxes
/// cover.
struct Grid {
    cols: i32,
    rows: i32,
    cells: Vec<Vec<usize>>,
}

impl Grid {
    fn new(blobs: &[Blob], pad: impl Fn(&Blob) -> i32) -> Self {
        let max_x = blobs.iter().map(|b| b.max_x).max().unwrap_or(0);
        let max_y = blobs.iter().map(|b| b.max_y).max().unwrap_or(0);
        let mut g = Self {
            cols: max_x / GRID_CELL + 1,
            rows: max_y / GRID_CELL + 1,
            cells: Vec::new(),
        };
        g.cells = vec![Vec::new(); (g.cols * g.rows) as usize];
        for (i, b) in blobs.iter().enumerate() {
            let p = pad(b);
            let (x0, y0, x1, y1) = g.span(b.min_x - p, b.min_y - p, b.max_x + p, b.max_y + p);
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    g.cells[(cy * g.cols + cx) as usize].push(i);
                }
            }
        }
        g
    }

    /// The first and last column and row a pixel box covers, clamped.
    fn span(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> (i32, i32, i32, i32) {
        let clamp = |v: i32, n: i32| (v.max(0) / GRID_CELL).clamp(0, n - 1);
        (
            clamp(x0, self.cols),
            clamp(y0, self.rows),
            clamp(x1, self.cols),
            clamp(y1, self.rows),
        )
    }

    /// The blobs in the cell holding a pixel.
    fn at(&self, x: i32, y: i32) -> &[usize] {
        let (cx, cy, _, _) = self.span(x, y, x, y);
        &self.cells[(cy * self.cols + cx) as usize]
    }

    /// The blobs in every cell a pixel box covers; a blob in several cells
    /// comes once per cell.
    fn each(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> impl Iterator<Item = usize> + '_ {
        let (x0, y0, x1, y1) = self.span(x0, y0, x1, y1);
        (y0..=y1).flat_map(move |cy| {
            (x0..=x1).flat_map(move |cx| self.cells[(cy * self.cols + cx) as usize].iter().copied())
        })
    }
}
