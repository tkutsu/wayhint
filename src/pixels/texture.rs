//! Pictures and video: where the pixels are photographic. A photo or a
//! video frame changes by a few shades on almost every pixel, while drawn
//! UI is flat between its edges. Words found on a photo are mostly its
//! texture; a control drawn over it stays flat inside.
//!
//! From `mds/spike-pixels/cheap-clutter/bench` (iteration 0 of the
//! clutter tests), with the per-box counts the baseline was scored with.

use super::{Corners, Frame};

/// The largest step to the left or upper neighbour that is flat, and the
/// step from which it's a hard edge.
const FLAT: u8 = 2;
const HARD: u8 = 24;
/// Media tiles: side in px, and what makes one photographic (at least
/// this % soft, under this % flat).
const TILE: usize = 16;
const TILE_SOFT: usize = 25;
const TILE_FLAT: usize = 50;
/// A media region needs this many tiles and sides this long, in px.
const REGION_TILES: usize = 12;
const REGION_SIDE: i32 = 64;

/// The biggest per-channel step from `(x, y)` to its left and upper
/// neighbours, where it has them.
fn step(frame: &Frame, x: usize, y: usize) -> u8 {
    let (px, i) = (&frame.rgb, (y * frame.width + x) * 3);
    let mut s = 0;
    for c in 0..3 {
        if x > 0 {
            s = s.max(px[i + c].abs_diff(px[i - 3 + c]));
        }
        if y > 0 {
            s = s.max(px[i + c].abs_diff(px[i - frame.width * 3 + c]));
        }
    }
    s
}

/// The % of flat pixels in `b` grown by 2 px (clamped to the frame).
pub fn flat_share(frame: &Frame, b: Corners) -> u32 {
    let clamp = |v: i32, n: usize| v.clamp(0, n as i32) as usize;
    let (x0, y0) = (clamp(b[0] - 2, frame.width), clamp(b[1] - 2, frame.height));
    let (x1, y1) = (clamp(b[2] + 2, frame.width), clamp(b[3] + 2, frame.height));
    let mut flat = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            flat += u32::from(step(frame, x, y) <= FLAT);
        }
    }
    let all = ((x1.saturating_sub(x0)) * (y1.saturating_sub(y0))).max(1) as u32;
    flat * 100 / all
}

/// The photographic regions, as the bounding boxes of 4-way joined
/// textured tiles, read from every second pixel of every second row.
pub fn media(frame: &Frame) -> Vec<Corners> {
    let (tw, th) = (frame.width / TILE, frame.height / TILE);
    if tw == 0 || th == 0 {
        return Vec::new();
    }
    let mut flat = vec![0usize; tw * th];
    let mut soft = vec![0usize; tw * th];
    // Each sampled row's steps per byte first, every pixel's but the first
    // (a loop the compiler vectorizes), then every second pixel's largest
    // channel.
    let (stride, span) = (frame.width * 3, tw * TILE * 3);
    let mut bytes = vec![0u8; span];
    for y in (1..th * TILE).step_by(2) {
        let row = &frame.rgb[y * stride..][..span];
        let up = &frame.rgb[(y - 1) * stride..][..span];
        for (s, (a, b)) in bytes[3..].iter_mut().zip(row[3..].iter().zip(row)) {
            *s = a.abs_diff(*b);
        }
        for (s, (a, b)) in bytes.iter_mut().zip(row.iter().zip(up)) {
            *s = (*s).max(a.abs_diff(*b));
        }
        let base = (y / TILE) * tw;
        // A pair of pixels is 6 bytes; the second, at odd x, is the sample.
        for (t, pairs) in bytes
            .as_chunks::<6>()
            .0
            .as_chunks::<{ TILE / 2 }>()
            .0
            .iter()
            .enumerate()
        {
            for p in pairs {
                let s = p[3].max(p[4]).max(p[5]);
                flat[base + t] += usize::from(s <= FLAT);
                soft[base + t] += usize::from(s > FLAT && s < HARD);
            }
        }
    }
    let samples = TILE * TILE / 4;
    let textured: Vec<bool> = (0..tw * th)
        .map(|i| soft[i] * 100 >= TILE_SOFT * samples && flat[i] * 100 < TILE_FLAT * samples)
        .collect();

    let mut seen = vec![false; tw * th];
    let mut out = Vec::new();
    for start in 0..tw * th {
        if !textured[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let (mut stack, mut count) = (vec![start], 0);
        let mut b = [i32::MAX, i32::MAX, 0, 0];
        while let Some(i) = stack.pop() {
            count += 1;
            let (x, y) = (i % tw, i / tw);
            let corner = |v: usize| (v * TILE) as i32;
            b = [
                b[0].min(corner(x)),
                b[1].min(corner(y)),
                b[2].max(corner(x + 1)),
                b[3].max(corner(y + 1)),
            ];
            let next = [
                (x > 0).then(|| i - 1),
                (x + 1 < tw).then_some(i + 1),
                (y > 0).then(|| i - tw),
                (y + 1 < th).then_some(i + tw),
            ];
            for j in next.into_iter().flatten() {
                if textured[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        if count >= REGION_TILES && b[2] - b[0] >= REGION_SIDE && b[3] - b[1] >= REGION_SIDE {
            out.push(b);
        }
    }
    out
}
