//! Tags from pixels, for windows with no accessibility tree: find what
//! looks like controls and words in a picture of the window. Pure: a
//! [`Frame`] in, boxes out, so every test runs on saved PNGs.
//!
//! ```text
//! frame -> edge boxes ----+-> words -> phrases -> blocks -> tags
//!       -> colour boxes --+                ^
//!       -> texture (pictures, video) ------+
//! ```
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
mod group;
mod runs;
mod segment;
mod texture;

pub use group::{Block, Found};

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

/// Everything worth a tag in `frame`. A terminal gets one focus hint in
/// clear space near its bottom, whichever detector is selected.
pub fn find(frame: &Frame, detector: Detector, terminal: bool) -> Found {
    if frame.width == 0 || frame.height == 0 {
        return Found::default();
    }
    if terminal {
        return group::terminal(frame);
    }
    let policy = detector.policy();
    group::group(frame, &words(frame, &policy), &policy)
}

/// `frame` at about one pixel per logical pixel, which every size rule
/// assumes: a frame `width` logical px wide but 2x or more as many pixels
/// (a scaled monitor) is averaged down by the whole part of the ratio, as
/// Neru reads a scaled frame. Returns the pixels per logical pixel left:
/// 1 at a whole scale, 1.25 at 2.5, 1.5 at 1.5 (a scale under 2 is read
/// as it is, untested; README, known gaps).
pub fn shrink(frame: Frame, width: i32) -> (Frame, f64) {
    let ratio = |f: &Frame| f.width as f64 / f64::from(width.max(1));
    let factor = ratio(&frame).floor() as usize;
    let frame = if factor >= 2 {
        average(&frame, factor)
    } else {
        frame
    };
    let left = ratio(&frame);
    (frame, left)
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
