//! The edge pass: wl-kbptr's detector as Neru ports it (`contour/detect.go`),
//! at the settings the baseline was scored with. Luma, a 5x5 blur, Sobel,
//! thin edges, Canny's two thresholds, a dilation that merges letters into
//! words, then one box per blob of edge pixels, filtered by where it sits
//! among the others.
//!
//! Every step gives exactly what the Go gives: the scores in
//! `.plans/pixel-tags.md` hold only for these exact boxes, and the tests
//! compare them with the Go output box for box. Some steps get there
//! another way, faster: row loops the compiler vectorizes, the gradient's
//! direction only where thinning reads it, and row runs
//! ([`super::runs`]) instead of a flood fill.

use super::{Corners, Frame, buckets::Buckets, runs};

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

    fn corners(&self) -> Corners {
        let (x, y) = (self.min_x as i32, self.min_y as i32);
        [x, y, x + self.width(), y + self.height()]
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

/// The separable `[1 4 6 4 1] / 16` blur, edges clamped. Each row is
/// padded with its end pixels once, so the taps are five shifted slices
/// and the loops have no branches.
fn blur(gray: &[u8], w: usize, h: usize) -> Vec<u8> {
    // 16 x 255 + 8 fits u16, half the width of u32 in a vector register.
    let tap = |a: u8, b: u8, c: u8, d: u8, e: u8| {
        let sum = u16::from(a) + 4 * u16::from(b) + 6 * u16::from(c) + 4 * u16::from(d);
        ((sum + u16::from(e) + 8) >> 4) as u8
    };
    let mut tmp = vec![0; w * h];
    let mut padded = vec![0; w + 4];
    for (row, out) in gray.chunks_exact(w).zip(tmp.chunks_exact_mut(w)) {
        padded[..2].fill(row[0]);
        padded[2..w + 2].copy_from_slice(row);
        padded[w + 2..].fill(row[w - 1]);
        let at = |dx: usize| &padded[dx..dx + w];
        let (a, b, c, d, e) = (at(0), at(1), at(2), at(3), at(4));
        for (x, o) in out.iter_mut().enumerate() {
            *o = tap(a[x], b[x], c[x], d[x], e[x]);
        }
    }
    let mut out = vec![0; w * h];
    for (y, out) in out.chunks_exact_mut(w).enumerate() {
        let at = |dy: isize| {
            let y = y.saturating_add_signed(dy).min(h - 1);
            &tmp[y * w..(y + 1) * w]
        };
        let (a, b, c, d, e) = (at(-2), at(-1), at(0), at(1), at(2));
        for (x, o) in out.iter_mut().enumerate() {
            *o = tap(a[x], b[x], c[x], d[x], e[x]);
        }
    }
    out
}

/// Sobel's gradient `(gx, gy)` at `x` of the middle row of three.
fn gradient([up, row, down]: [&[u8]; 3], x: usize) -> (i16, i16) {
    let p = |r: &[u8], x: usize| i16::from(r[x]);
    let gx = (p(up, x + 1) + 2 * p(row, x + 1) + p(down, x + 1))
        - (p(up, x - 1) + 2 * p(row, x - 1) + p(down, x - 1));
    let gy = (p(down, x - 1) + 2 * p(down, x) + p(down, x + 1))
        - (p(up, x - 1) + 2 * p(up, x) + p(up, x + 1));
    (gx, gy)
}

/// Sobel, thinning along the gradient, and hysteresis: 255 on edges.
/// Thinning needs the gradient's direction only where the magnitude
/// reaches `LOW`, so it works the direction out there, and skips whole
/// 32 px stretches where nothing does.
fn canny(blurred: &[u8], w: usize, h: usize) -> Vec<u8> {
    const WEAK: u8 = 1;
    const STRONG: u8 = 255;
    // Edges lie inside the 1 px border: a frame under 3 px either way has
    // none.
    if w < 3 || h < 3 {
        return vec![0; w * h];
    }
    let rows = |y: usize| [y - 1, y, y + 1].map(|y| &blurred[y * w..(y + 1) * w]);
    // Magnitudes a row at a time from each column's two sums: gx is the
    // next column's weighted sum less the last one's, gy the weighted sum
    // of three columns' down-minus-up. Loops without branches vectorize.
    let mut mag = vec![0i16; w * h];
    let (mut sums, mut rises) = (vec![0i16; w], vec![0i16; w]);
    for y in 1..h - 1 {
        let [up, row, down] = rows(y);
        for ((s, r), ((&u, &c), &d)) in sums
            .iter_mut()
            .zip(&mut rises)
            .zip(up.iter().zip(row).zip(down))
        {
            let (u, c, d) = (i16::from(u), i16::from(c), i16::from(d));
            (*s, *r) = (u + 2 * c + d, d - u);
        }
        for (m, (s, r)) in mag[y * w + 1..(y + 1) * w - 1]
            .iter_mut()
            .zip(sums.windows(3).zip(rises.windows(3)))
        {
            *m = (s[2] - s[0]).abs() + (r[0] + 2 * r[1] + r[2]).abs();
        }
    }
    // Thin edges: WEAK from `LOW`, STRONG from `HIGH`.
    let mut marks = vec![0u8; w * h];
    let mut queue: Vec<usize> = Vec::with_capacity(w * h / 64);
    for y in 1..h - 1 {
        let rows = rows(y);
        for x0 in (1..w - 1).step_by(32) {
            let x1 = (x0 + 32).min(w - 1);
            // The largest, not `all`: a loop without early exits vectorizes.
            if mag[y * w + x0..y * w + x1].iter().fold(0, |a, &m| a.max(m)) < LOW {
                continue;
            }
            for x in x0..x1 {
                let i = y * w + x;
                let m = mag[i];
                if m < LOW {
                    continue;
                }
                let (gx, gy) = gradient(rows, x);
                let (ax, ay) = (i32::from(gx).abs(), i32::from(gy).abs());
                // The normal's direction, in four: horizontal, vertical, 45
                // degrees, 135 degrees (tan 22.5 = 424/1024, tan 67.5 =
                // 2472/1024).
                let (n1, n2) = if ay * 1024 <= ax * 424 {
                    (mag[i - 1], mag[i + 1])
                } else if ay * 1024 >= ax * 2472 {
                    (mag[i - w], mag[i + w])
                } else if (gx > 0 && gy > 0) || (gx < 0 && gy < 0) {
                    (mag[i - w + 1], mag[i + w - 1])
                } else {
                    (mag[i - w - 1], mag[i + w + 1])
                };
                if m >= n1 && m >= n2 {
                    marks[i] = WEAK;
                    if m >= HIGH {
                        marks[i] = STRONG;
                        queue.push(i);
                    }
                }
            }
        }
    }
    // Strong edges spread through the weak ones they touch. Marks are never
    // on the border, so every neighbour of one is in the frame.
    while let Some(i) = queue.pop() {
        for n in [
            i - w - 1,
            i - w,
            i - w + 1,
            i - 1,
            i + 1,
            i + w - 1,
            i + w,
            i + w + 1,
        ] {
            if marks[n] == WEAK {
                marks[n] = STRONG;
                queue.push(n);
            }
        }
    }
    for m in &mut marks {
        if *m == WEAK {
            *m = 0;
        }
    }
    marks
}

/// Grows edges by the rectangle: a pixel and its left neighbour, then that
/// row with the ones above and below, as OpenCV centres an even kernel at
/// `size / 2` from its start. Whole-row ORs, which the compiler vectorizes.
fn dilate(edges: &[u8], w: usize, h: usize) -> Vec<bool> {
    const { assert!(DILATE_ROWS == 3 && DILATE_COLUMNS == 2) };
    let mut wide = vec![0u8; w * h];
    for (row, out) in edges.chunks_exact(w).zip(wide.chunks_exact_mut(w)) {
        out[0] = row[0];
        for (o, (a, b)) in out[1..].iter_mut().zip(row.iter().zip(&row[1..])) {
            *o = a | b;
        }
    }
    let mut out = vec![false; w * h];
    for (y, out) in out.chunks_exact_mut(w).enumerate() {
        let at = |y: usize| &wide[y * w..(y + 1) * w];
        let (up, row, down) = (at(y.saturating_sub(1)), at(y), at((y + 1).min(h - 1)));
        for (o, ((a, b), c)) in out.iter_mut().zip(up.iter().zip(row).zip(down)) {
            *o = (a | b | c) != 0;
        }
    }
    out
}

/// 8-connected blobs, in raster order of their first pixel, each with its
/// box.
fn blobs(dilated: &[bool], w: usize, h: usize) -> Vec<Blob> {
    let runs = runs::label(dilated, w, h);
    let mut out: Vec<Blob> = Vec::with_capacity(runs.count);
    for y in 0..h {
        for run in runs.row(y) {
            let (x0, x1) = (run.x0 as usize, run.x1 as usize);
            if run.label == out.len() {
                out.push(Blob {
                    min_x: x0,
                    min_y: y,
                    max_x: x1,
                    max_y: y,
                    parent: None,
                    first_child: None,
                    next_sibling: None,
                });
            }
            let b = &mut out[run.label];
            b.min_x = b.min_x.min(x0);
            b.max_x = b.max_x.max(x1);
            b.max_y = y;
        }
    }
    out
}

/// Gives each blob its parent: the smallest other blob, strictly bigger,
/// whose box holds its box give or take 1 px. Strictly bigger keeps the
/// links acyclic.
fn nest(blobs: &mut [Blob]) {
    let boxes: Vec<Corners> = blobs.iter().map(Blob::corners).collect();
    let buckets = Buckets::new(&boxes);
    for i in 0..blobs.len() {
        let c = &blobs[i];
        let own = c.area();
        // A parent covers this box shrunk by 1 px, so it overlaps the box
        // grown by 1 px: the buckets under that hold every candidate. Of
        // the smallest, the lowest index, as Neru's scan of all pairs picks.
        let b = boxes[i];
        let grown = [b[0] - 1, b[1] - 1, b[2] + 1, b[3] + 1];
        let parent = buckets
            .near(grown)
            .filter(|&j| {
                let p = &blobs[j];
                p.area() > own
                    && p.min_x <= c.min_x + 1
                    && p.max_x + 1 >= c.max_x
                    && p.min_y <= c.min_y + 1
                    && p.max_y + 1 >= c.max_y
            })
            .min_by_key(|&j| (blobs[j].area(), j));
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
        .map(|(b, _)| b.corners())
        .collect()
}
