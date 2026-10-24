//! The edge pass: wl-kbptr's detector as Neru ports it (`contour/detect.go`),
//! at the settings the baseline was scored with. Luma, a 5x5 blur, Sobel,
//! thin edges, Canny's two thresholds, a dilation that merges letters into
//! words, then one box per blob of edge pixels, filtered by where it sits
//! among the others.
//!
//! Every step is a literal port: the scores in `.plans/pixel-tags.md` hold
//! only for these exact boxes, and the tests compare them with the Go
//! output box for box.

use super::{Corners, Frame};

/// Canny's pair, on Sobel magnitudes `|gx| + |gy|` of a 0-255 luma: above
/// `HIGH` starts an edge, above `LOW` extends one.
const LOW: i16 = 70;
const HIGH: i16 = 220;
/// The dilation, in px: 3 rows by 2 columns. wl-kbptr's 2.5 x 3.5 joined
/// words across their spaces (iteration 4, `pixel-clutter-tests.md`).
const DILATE_ROWS: usize = 3;
const DILATE_COLUMNS: usize = 2;
/// Blobs outside these sizes are noise (at or under the minimum) or layout
/// (at or over the maximum).
const MIN_WIDTH: i32 = 7;
const MIN_HEIGHT: i32 = 3;
const MAX_WIDTH: i32 = 650;
const MAX_HEIGHT: i32 = 160;
/// A nested blob this tall or less is a stroke inside something (a
/// hamburger icon's lines).
const FLAT_LINE: i32 = 6;
/// From this height a blob is a card or dialog when it holds targets.
const CONTAINER: i32 = 50;
/// A nested blob whose centre is this close to its parent's is the same
/// click.
const SAME_CENTRE: f64 = 8.0;
/// A parent about square (sides within `SQUARE_SLACK`) and under
/// `SQUARE_ICON` is an icon: what's inside is its artwork.
const SQUARE_ICON: i32 = 40;
const SQUARE_SLACK: i32 = 5;

/// One 8-connected blob of dilated edges and its place among the others.
/// Indices are into the blob list; `None` is none.
#[derive(Clone)]
struct Blob {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    parent: Option<usize>,
    first_child: Option<usize>,
    next_sibling: Option<usize>,
}

impl Blob {
    fn area(&self) -> usize {
        (self.max_x - self.min_x + 1) * (self.max_y - self.min_y + 1)
    }

    fn width(&self) -> i32 {
        (self.max_x - self.min_x + 1) as i32
    }

    fn height(&self) -> i32 {
        (self.max_y - self.min_y + 1) as i32
    }

    /// The children, newest first, as the sibling links run.
    fn children<'a>(&self, blobs: &'a [Blob]) -> impl Iterator<Item = usize> + 'a {
        std::iter::successors(self.first_child, |&c| blobs[c].next_sibling)
    }
}

/// The boxes, in the order their blobs were found (raster order of their
/// first pixel).
pub fn boxes(frame: &Frame) -> Vec<Corners> {
    let (w, h) = (frame.width, frame.height);
    let blurred = blur(&luma(frame), w, h);
    let edges = canny(&blurred, w, h);
    let mut blobs = blobs(&dilate(&edges, w, h), w, h);
    nest(&mut blobs);
    keep(&blobs)
}

/// ITU-R BT.601 luma in integers, as Neru rounds it.
fn luma(frame: &Frame) -> Vec<u8> {
    let (pixels, _) = frame.rgb.as_chunks::<3>();
    pixels
        .iter()
        .map(|&[r, g, b]| {
            let (r, g, b) = (u32::from(r), u32::from(g), u32::from(b));
            ((77 * r + 150 * g + 29 * b + 128) >> 8) as u8
        })
        .collect()
}

/// The separable `[1 4 6 4 1] / 16` blur, edges clamped.
fn blur(gray: &[u8], w: usize, h: usize) -> Vec<u8> {
    let tap = |a: u8, b: u8, c: u8, d: u8, e: u8| {
        let sum = u32::from(a) + 4 * u32::from(b) + 6 * u32::from(c) + 4 * u32::from(d);
        ((sum + u32::from(e) + 8) >> 4) as u8
    };
    let mut tmp = vec![0; w * h];
    for y in 0..h {
        let row = &gray[y * w..(y + 1) * w];
        for x in 0..w {
            let at = |dx: isize| row[x.saturating_add_signed(dx).min(w - 1)];
            tmp[y * w + x] = tap(at(-2), at(-1), row[x], at(1), at(2));
        }
    }
    let mut out = vec![0; w * h];
    for y in 0..h {
        let at = |dy: isize| y.saturating_add_signed(dy).min(h - 1) * w;
        let (a, b, d, e) = (at(-2), at(-1), at(1), at(2));
        for x in 0..w {
            out[y * w + x] = tap(
                tmp[a + x],
                tmp[b + x],
                tmp[y * w + x],
                tmp[d + x],
                tmp[e + x],
            );
        }
    }
    out
}

/// Sobel, thinning along the gradient, and hysteresis: 255 on edges.
fn canny(blurred: &[u8], w: usize, h: usize) -> Vec<u8> {
    let total = w * h;
    let mut mag = vec![0i16; total];
    let mut dir = vec![0u8; total];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w - 1 {
            let p = |dx: usize, dy: usize| i32::from(blurred[(y + dy - 1) * w + x + dx - 1]);
            let gx = (p(2, 0) + 2 * p(2, 1) + p(2, 2)) - (p(0, 0) + 2 * p(0, 1) + p(0, 2));
            let gy = (p(0, 2) + 2 * p(1, 2) + p(2, 2)) - (p(0, 0) + 2 * p(1, 0) + p(2, 0));
            let (ax, ay) = (gx.abs(), gy.abs());
            let i = y * w + x;
            mag[i] = (ax + ay) as i16;
            // The normal's direction, in four: horizontal, 45 degrees,
            // vertical, 135 degrees (tan 22.5 = 424/1024, tan 67.5 = 2472/1024).
            dir[i] = if ay * 1024 <= ax * 424 {
                0
            } else if ay * 1024 >= ax * 2472 {
                2
            } else if (gx > 0 && gy > 0) || (gx < 0 && gy < 0) {
                1
            } else {
                3
            };
        }
    }
    let mut thin = vec![0i16; total];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w - 1 {
            let i = y * w + x;
            let m = mag[i];
            if m < LOW {
                continue;
            }
            let (n1, n2) = match dir[i] {
                0 => (mag[i - 1], mag[i + 1]),
                1 => (mag[i - w + 1], mag[i + w - 1]),
                2 => (mag[i - w], mag[i + w]),
                _ => (mag[i - w - 1], mag[i + w + 1]),
            };
            if m >= n1 && m >= n2 {
                thin[i] = m;
            }
        }
    }
    let mut edges = vec![0u8; total];
    let mut queue: Vec<usize> = Vec::with_capacity(total / 8);
    for (i, &v) in thin.iter().enumerate() {
        if v >= HIGH {
            edges[i] = 255;
            queue.push(i);
        }
    }
    let mut head = 0;
    while head < queue.len() {
        let i = queue[head];
        head += 1;
        for n in neighbours(i % w, i / w, w, h) {
            if edges[n] == 0 && thin[n] >= LOW {
                edges[n] = 255;
                queue.push(n);
            }
        }
    }
    edges
}

/// The 8 neighbours of `(x, y)` inside a `w` x `h` frame, as indices, in
/// Neru's order: the row above, the two sides, the row below.
fn neighbours(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    const D: [(isize, isize); 8] = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    D.into_iter().filter_map(move |(dx, dy)| {
        let nx = x.checked_add_signed(dx).filter(|&v| v < w)?;
        let ny = y.checked_add_signed(dy).filter(|&v| v < h)?;
        Some(ny * w + nx)
    })
}

/// Grows edges by the rectangle, rows then columns. The kernel's centre
/// sits at `size / 2` from its start, as OpenCV puts an even kernel's.
fn dilate(edges: &[u8], w: usize, h: usize) -> Vec<bool> {
    let mut wide = vec![false; w * h];
    let rx = DILATE_COLUMNS / 2;
    for y in 0..h {
        let row = &edges[y * w..(y + 1) * w];
        for x in 0..w {
            let (start, end) = (
                x.saturating_sub(rx),
                (x + DILATE_COLUMNS - 1 - rx).min(w - 1),
            );
            wide[y * w + x] = row[start..=end].iter().any(|&e| e != 0);
        }
    }
    let mut out = vec![false; w * h];
    let ry = DILATE_ROWS / 2;
    for y in 0..h {
        let (start, end) = (y.saturating_sub(ry), (y + DILATE_ROWS - 1 - ry).min(h - 1));
        let row = &mut out[y * w..(y + 1) * w];
        for k in start..=end {
            for (o, &v) in row.iter_mut().zip(&wide[k * w..(k + 1) * w]) {
                *o |= v;
            }
        }
    }
    out
}

/// 8-connected blobs by breadth-first search, in raster order of their
/// first pixel, each with its box.
fn blobs(dilated: &[bool], w: usize, h: usize) -> Vec<Blob> {
    let mut seen = vec![false; w * h];
    let mut queue = Vec::with_capacity(1024);
    let mut out = Vec::with_capacity(256);
    for start in 0..w * h {
        if !dilated[start] || seen[start] {
            continue;
        }
        let (x, y) = (start % w, start / w);
        let mut b = Blob {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
            parent: None,
            first_child: None,
            next_sibling: None,
        };
        seen[start] = true;
        queue.clear();
        queue.push(start);
        let mut head = 0;
        while head < queue.len() {
            let i = queue[head];
            head += 1;
            let (cx, cy) = (i % w, i / w);
            b.min_x = b.min_x.min(cx);
            b.max_x = b.max_x.max(cx);
            b.min_y = b.min_y.min(cy);
            b.max_y = b.max_y.max(cy);
            for n in neighbours(cx, cy, w, h) {
                if dilated[n] && !seen[n] {
                    seen[n] = true;
                    queue.push(n);
                }
            }
        }
        out.push(b);
    }
    out
}

/// Gives each blob its parent: the smallest other blob, strictly bigger,
/// whose box holds its box give or take 1 px. Strictly bigger keeps the
/// links acyclic.
fn nest(blobs: &mut [Blob]) {
    // Sorted by area, the candidates for a parent are the blobs after the
    // child, and the first that holds it is the smallest. Ties in area keep
    // the lower index first, which is the one Neru's scan of all pairs picks.
    let mut by_area: Vec<usize> = (0..blobs.len()).collect();
    by_area.sort_by_key(|&i| blobs[i].area());
    for (rank, &i) in by_area.iter().enumerate() {
        let c = &blobs[i];
        let own = c.area();
        let parent = by_area[rank + 1..].iter().copied().find(|&j| {
            let p = &blobs[j];
            p.area() > own
                && p.min_x <= c.min_x + 1
                && p.max_x + 1 >= c.max_x
                && p.min_y <= c.min_y + 1
                && p.max_y + 1 >= c.max_y
        });
        blobs[i].parent = parent;
        if let Some(p) = parent {
            blobs[i].next_sibling = blobs[p].first_child;
            blobs[p].first_child = Some(i);
        }
    }
}

/// wl-kbptr's filters, at scale 1: sizes, then the nesting rules.
fn keep(blobs: &[Blob]) -> Vec<Corners> {
    let mut dropped: Vec<bool> = blobs
        .iter()
        .map(|b| {
            let (w, h) = (b.width(), b.height());
            h >= MAX_HEIGHT || w >= MAX_WIDTH || h <= MIN_HEIGHT || w <= MIN_WIDTH
        })
        .collect();
    let centre = |b: &Blob| {
        let x = b.min_x as f64 + f64::from(b.width()) / 2.0;
        (x, b.min_y as f64 + f64::from(b.height()) / 2.0)
    };

    // Depth first from every nested blob. A blob can be reached twice, as
    // a start and as a child: Neru walks it that way, so this does too.
    let mut stack: Vec<usize> = (0..blobs.len())
        .filter(|&i| blobs[i].parent.is_some())
        .collect();
    while let Some(i) = stack.pop() {
        if !dropped[i]
            && let Some(p) = blobs[i].parent
        {
            let (b, parent) = (&blobs[i], &blobs[p]);
            if dropped[p] {
                // The parent is layout (a dialog, a card): its children are
                // the targets, as they are.
            } else if b.height() <= FLAT_LINE {
                dropped[i] = true;
            } else {
                let ((cx, cy), (px, py)) = (centre(b), centre(parent));
                let same = (cx - px).abs() < SAME_CENTRE && (cy - py).abs() < SAME_CENTRE;
                let (pw, ph) = (parent.width(), parent.height());
                let icon = (ph - pw).abs() < SQUARE_SLACK && ph < SQUARE_ICON && pw < SQUARE_ICON;
                if same || icon {
                    dropped[i] = true;
                }
            }
        }
        stack.extend(blobs[i].children(blobs));
    }

    // A tall blob holding button-sized children is a dialog, and they are
    // its targets; one holding none is a toast, clickable itself.
    for i in 0..blobs.len() {
        if dropped[i] || blobs[i].height() < CONTAINER {
            continue;
        }
        let holds = blobs[i].children(blobs).any(|c| {
            let h = blobs[c].height();
            !dropped[c] && h < CONTAINER && h > FLAT_LINE
        });
        if holds {
            dropped[i] = true;
        }
    }

    blobs
        .iter()
        .zip(&dropped)
        .filter(|&(_, &d)| !d)
        .map(|(b, _)| {
            let (x, y) = (b.min_x as i32, b.min_y as i32);
            [x, y, x + b.width(), y + b.height()]
        })
        .collect()
}
