//! Tags from pixels, for windows with no accessibility tree: find what
//! looks like controls and words in a picture of the window. Pure: a
//! [`Frame`] in, boxes out, so every test runs on saved PNGs.
//!
//! ```text
//! frame -> edge boxes ----+-> words -> phrases -> blocks -> tags
//!       -> colour boxes --+    ^                  ^
//!       -> sheet grids -------(drop)-> bodies ----+
//!       -> texture (pictures, video) -------------+
//! ```
//!
//! Each section of a window gets its own rule: a control or list row one
//! tag, prose and code one tag that opens into lines, a picture none
//! inside, a terminal or a sheet one body tag that focuses it and exits.
//!
//! The rules and every number come from the prototype in
//! `.plans/pixel-tags.md` ("Baseline (2026-10-24)"), calibrated against
//! windows that do have a tree: their accessibility targets are the right
//! answers. The port reproduces its scores on the same 30 captures.
//!
//! The edge and colour passes are ported from Neru
//! (<https://github.com/y3owk1n/neru>), MIT licence, Copyright (c) 2024
//! Neru Contributors; the full notice is in `LICENSE-NERU` next to this
//! file.

// Pixel arithmetic: coordinates and counts of one frame are far inside
// i32 and f64's exact range, and never negative where cast to usize. And
// pixel kernels: x, y, w, h, as image code writes them.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

mod buckets;
mod contour;
mod grid;
mod group;
mod runs;
mod segment;
mod texture;

pub use group::{Block, Found, terminal_patch};

use crate::space::Space;

/// A box as the prototype keeps one: left, top, right, bottom, the right
/// and bottom edges outside it. Plain arrays sort and compare as Python's
/// lists did, which the grouping relies on.
pub type Corners = [i32; 4];

/// A window's pixels, 3 bytes per pixel (red, green, blue), rows top to
/// bottom, no padding.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// How hard to look. The two settings the prototype scored best for their
/// cost (`.plans/pixel-tags.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Detector {
    /// Both passes, on two threads: 84.4% of targets on the first screen,
    /// 1,575 of 4,796 tags stray, on the 30 captures (px-blocks setting
    /// 10061, link tags on).
    #[default]
    Full,
    /// The colour pass alone, half the work: 82.4% of targets, 2,409 of
    /// 5,542 tags stray (setting 8948).
    Fast,
}

/// The grouping's settings for one [`Detector`].
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Run the edge pass, the colour pass filling in.
    edges: bool,
    /// Words join across gaps of at most this x their height.
    join: f64,
    /// A block reads as a list, and stays open, under this share of long
    /// rows and this many words per row.
    open: (f64, usize),
    /// A list-like block stays shut as code at this monospace lock.
    code: Option<f64>,
    /// Phrases in a link colour inside a shut block get tags.
    links: bool,
    /// In a picture, a phrase is texture when under `share` of its words
    /// are `flat` % flat or more.
    flat: u32,
    share: f64,
}

impl Detector {
    const fn policy(self) -> Policy {
        match self {
            Self::Full => Policy {
                edges: true,
                join: 0.5,
                open: (0.8, 12),
                code: Some(6.0),
                links: true,
                flat: 60,
                share: 0.5,
            },
            Self::Fast => Policy {
                edges: false,
                join: 0.3,
                open: (0.5, 8),
                code: None,
                links: false,
                flat: 50,
                share: 0.25,
            },
        }
    }
}

/// Everything worth a tag in `frame`. A terminal is one body, whichever
/// detector is selected; so is each sheet, whose cells get no tags.
pub fn find(frame: &Frame, detector: Detector, terminal: bool) -> Found {
    if frame.width == 0 || frame.height == 0 {
        return Found::default();
    }
    if terminal {
        return Found {
            bodies: vec![group::terminal(frame)],
            ..Found::default()
        };
    }
    let policy = detector.policy();
    // The grid on a thread of its own, as the detector's passes are.
    let (sheets, words) = std::thread::scope(|s| {
        let sheets = s.spawn(|| grid::sheets(frame));
        let words = words(frame, &policy);
        let sheets = sheets
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        (sheets, words)
    });
    let words: Vec<Corners> = words
        .into_iter()
        .filter(|&w| !sheets.iter().any(|s| s.holds(group::centre(w))))
        .collect();
    let mut found = group::group(frame, &words, &policy);
    if !sheets.is_empty() {
        let space = Space::pixels(&grid::unruled(frame, &sheets));
        found.bodies = sheets
            .iter()
            .map(|s| group::sheet(&space, s.cells))
            .collect();
    }
    found
}

/// Ink in `frame` other than a sheet's grid lines: where a body's tag can
/// go, so that an empty cell is clear space.
pub fn cell_space(frame: &Frame) -> Space {
    Space::pixels(&grid::unruled(frame, &grid::sheets(frame)))
}

/// `frame` at one pixel per logical pixel, which every size rule assumes:
/// a frame `width` logical px wide but with more pixels (a scaled monitor)
/// is averaged down by the whole part of the ratio, as Neru reads a scaled
/// frame, then what's left of a fractional scale is resampled. Without that
/// a 100 px button at 1.5 is 150 px to the rules, and the colour pass found
/// nothing at 1.25, 1.5, 1.75 or 2.5 (audit, 2026-10-27). Returns the
/// pixels per logical pixel left, 1 but for rounding.
pub fn shrink(frame: Frame, width: i32) -> (Frame, f64) {
    let width = width.max(1) as usize;
    let ratio = |f: &Frame| f.width as f64 / width as f64;
    let factor = ratio(&frame).floor() as usize;
    let frame = if factor >= 2 {
        average(&frame, factor)
    } else {
        frame
    };
    let frame = if frame.width > width {
        let height = (frame.height as f64 / ratio(&frame)).round().max(1.0) as usize;
        resample(&frame, width, height)
    } else {
        frame
    };
    let left = ratio(&frame);
    (frame, left)
}

/// `frame` at `width` x `height`, each pixel the average of the area of
/// `frame` it covers, a row at a time.
fn resample(frame: &Frame, width: usize, height: usize) -> Frame {
    let cols = spans(frame.width, width);
    let mut rgb = Vec::with_capacity(width * height * 3);
    let mut line = vec![0.0f32; frame.width * 3];
    for row in spans(frame.height, height) {
        line.fill(0.0);
        for (y, weight) in row {
            let source = &frame.rgb[y * frame.width * 3..(y + 1) * frame.width * 3];
            for (sum, &value) in line.iter_mut().zip(source) {
                *sum += weight * f32::from(value);
            }
        }
        for col in &cols {
            let mut sum = [0.0f32; 3];
            for &(x, weight) in col {
                for (c, total) in sum.iter_mut().enumerate() {
                    *total += weight * line[x * 3 + c];
                }
            }
            rgb.extend(sum.map(|v| v.round().clamp(0.0, 255.0) as u8));
        }
    }
    Frame { width, height, rgb }
}

/// For each of `to` cells laid over `from`, the cells under it and the
/// share of it each covers.
fn spans(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let step = from as f64 / to as f64;
    (0..to)
        .map(|i| {
            let start = i as f64 * step;
            let end = ((i + 1) as f64 * step).min(from as f64);
            (start.floor() as usize..(end.ceil() as usize).min(from))
                .map(|s| {
                    let cover = end.min(s as f64 + 1.0) - start.max(s as f64);
                    (s, (cover / (end - start)) as f32)
                })
                .filter(|&(_, share)| share > 0.0)
                .collect()
        })
        .collect()
}

/// Each `factor` x `factor` block of pixels as one, averaged; blocks at the
/// right and bottom edges average what they hold.
fn average(frame: &Frame, factor: usize) -> Frame {
    let (w, h) = (frame.width.div_ceil(factor), frame.height.div_ceil(factor));
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let rows = y * factor..((y + 1) * factor).min(frame.height);
        for x in 0..w {
            let cols = x * factor..((x + 1) * factor).min(frame.width);
            let mut sum = [0usize; 3];
            let mut n = 0;
            for sy in rows.clone() {
                for sx in cols.clone() {
                    let i = (sy * frame.width + sx) * 3;
                    for (c, total) in sum.iter_mut().enumerate() {
                        *total += usize::from(frame.rgb[i + c]);
                    }
                    n += 1;
                }
            }
            rgb.extend(sum.map(|s| ((s + n / 2) / n) as u8));
        }
    }
    Frame {
        width: w,
        height: h,
        rgb,
    }
}

/// How far apart a channel is from a button's fill to count as ink.
const INK: u8 = 48;

/// Whether a button's box in `frame` shows its name, `letters` long, as
/// letters rather than an icon. Letters leave a gap between most of them,
/// so a name's ink falls in about one run of columns per letter, where an
/// icon's falls in one or two: two runs or more, and one per two letters,
/// are letters. A one-letter name passes, as letter or icon alike (Dolphin's
/// `/` crumb is a button).
pub fn shows_letters(frame: &Frame, [l, t, r, b]: Corners, letters: usize) -> bool {
    if letters < 2 {
        return true;
    }
    // Inside the edge, which may be a border or a rounded hover fill.
    let (l, t) = ((l + 3).max(0) as usize, (t + 3).max(0) as usize);
    let r = (r - 3).clamp(0, frame.width as i32) as usize;
    let b = (b - 3).clamp(0, frame.height as i32) as usize;
    if l >= r || t >= b {
        return false;
    }
    let pixel = |x: usize, y: usize| {
        let i = (y * frame.width + x) * 3;
        [frame.rgb[i], frame.rgb[i + 1], frame.rgb[i + 2]]
    };
    // The fill: the commonest colour of the top and bottom rows.
    let mut counts = std::collections::HashMap::new();
    for x in l..r {
        for y in [t, b - 1] {
            *counts.entry(pixel(x, y)).or_insert(0) += 1;
        }
    }
    let Some((fill, _)) = counts.into_iter().max_by_key(|&(c, n)| (n, c)) else {
        return false;
    };
    let inked = |x: usize| {
        (t..b).any(|y| {
            pixel(x, y)
                .iter()
                .zip(fill)
                .any(|(&p, f)| p.abs_diff(f) >= INK)
        })
    };
    let mut runs = 0;
    let mut was = false;
    for x in l..r {
        let is = inked(x);
        runs += usize::from(is && !was);
        was = is;
    }
    runs >= 2 && runs * 2 >= letters
}

/// The detector boxes the grouping starts from.
fn words(frame: &Frame, policy: &Policy) -> Vec<Corners> {
    if !policy.edges {
        return segment::boxes(frame);
    }
    // The two passes read the frame and share nothing else: one thread
    // each takes the slower one's time instead of the sum.
    std::thread::scope(|s| {
        let edges = s.spawn(|| contour::boxes(frame));
        let colour = segment::boxes(frame);
        let edges = edges
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        group::fill(&edges, &colour)
    })
}

#[cfg(test)]
pub mod tests;
