//! From boxes to tags: words join into phrases, phrases stacked at text
//! pitch make blocks, and a block that reads as prose or code gets one tag
//! that opens into its phrases. Lists, menus and icon-led rows stay open:
//! their phrases are tags straight away.
//!
//! A port of the prototype's Python (`mds/spike-pixels/a11y-truth`:
//! `pipeline.py`, `tune.py`, `blocks4.py`, `fill_tune.py`, `mono_q.py`,
//! and `px-blocks-2026-10-24/experiment.py`), rule for rule, so the port
//! scores what the prototype scored. Where Python's arithmetic differs
//! from Rust's (banker's rounding, NumPy's medians) the port does it
//! Python's way.
//!
//! Places to type into are one body tag each: a whole [`terminal`], near
//! its bottom, leaving its text and TUI controls to the keyboard; a
//! [`sheet`], near its middle.

use std::collections::{HashMap, HashSet};

use super::{Corners, Frame, Policy, buckets::Buckets, texture};
use crate::{geometry::Rect, space::Space};

/// Text is this tall, in px; anything else passes through as it is.
const TEXT: std::ops::RangeInclusive<i32> = 3..=48;
/// A pixel is ink when a channel is this far from the background.
const INK: f64 = 40.0;
/// A block opens when this share of its rows starts with an icon.
const ICON_ROWS: f64 = 0.4;
/// Link tags: the block's body colour covers this share of its width; a
/// link colour is this colourful, on this many phrases, and there are at
/// most `LINK_COLOURS` of them.
const BODY_SHARE: f64 = 0.6;
const LINK_CHROMA: f64 = 40.0;
const LINK_PHRASES: usize = 3;
const LINK_COLOURS: usize = 2;

pub const fn width(b: Corners) -> i32 {
    b[2] - b[0]
}

pub const fn height(b: Corners) -> i32 {
    b[3] - b[1]
}

/// The centre, rounded down, as Python's `//` does.
pub const fn centre(b: Corners) -> (i32, i32) {
    ((b[0] + b[2]).div_euclid(2), (b[1] + b[3]).div_euclid(2))
}

/// Left and top edges in, right and bottom out.
pub const fn inside(p: (i32, i32), r: Corners) -> bool {
    r[0] <= p.0 && p.0 < r[2] && r[1] <= p.1 && p.1 < r[3]
}

pub fn union(rs: &[Corners]) -> Corners {
    rs.iter()
        .fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |u, r| {
            [
                u[0].min(r[0]),
                u[1].min(r[1]),
                u[2].max(r[2]),
                u[3].max(r[3]),
            ]
        })
}

fn is_text(b: Corners) -> bool {
    TEXT.contains(&height(b))
}

/// What a scan of one frame found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Found {
    /// Tags that act where they are: phrases, controls, link phrases.
    pub direct: Vec<Corners>,
    /// Collapsed blocks: one tag each, opening into its phrases.
    pub blocks: Vec<Block>,
    /// Pictures and video big enough to be content.
    pub media: Vec<Corners>,
    /// One tag for a whole place to type into (a terminal, a sheet): a
    /// click there focuses it and ends the mode.
    pub bodies: Vec<Corners>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub rect: Corners,
    pub phrases: Vec<Corners>,
}

/// Groups `words` (detector boxes) into tags under `policy`.
pub fn group(frame: &Frame, words: &[Corners], policy: &Policy) -> Found {
    let mut inks = Inks::new(frame);
    let phrases = phrases(words, &mut inks, policy.join);
    let groups: Vec<Vec<Corners>> = blocks(&phrases).into_iter().filter(|g| prose(g)).collect();
    let shut: Vec<bool> = groups
        .iter()
        .map(|g| collapses(g, words, &mut inks, policy))
        .collect();
    let media = texture::media(frame);

    let blocked: HashSet<Corners> = groups
        .iter()
        .zip(&shut)
        .filter(|&(_, &s)| s)
        .flat_map(|(g, _)| g.iter().copied())
        .collect();
    let textured = |p: Corners| {
        if !media.iter().any(|&r| inside(centre(p), r)) {
            return false;
        }
        let shares: Vec<u32> = members(words, p)
            .map(|w| texture::flat_share(frame, w))
            .collect();
        // A phrase holds its words' centres, so there is a share at least;
        // the prototype counted a phrase without words as flat.
        if shares.is_empty() {
            return false;
        }
        let flat = shares.iter().filter(|&&s| s >= policy.flat).count();
        (flat as f64) / (shares.len() as f64) < policy.share
    };
    let mut direct: Vec<Corners> = phrases
        .iter()
        .copied()
        .filter(|&p| !blocked.contains(&p) && !textured(p))
        .collect();
    let mut found_blocks = Vec::new();
    for (g, &s) in groups.iter().zip(&shut) {
        if !s {
            continue;
        }
        if policy.links {
            direct.extend(links(g, &mut inks));
        }
        found_blocks.push(Block {
            rect: union(g),
            phrases: g.clone(),
        });
    }
    Found {
        direct,
        blocks: found_blocks,
        media,
        bodies: Vec::new(),
    }
}

/// A terminal's preferred focus patch, 10 px from its bottom-right edges.
pub fn terminal_patch(width: i32, height: i32) -> Rect {
    let (w, h) = (36.min(width), 24.min(height));
    let (right, bottom) = (10.min(width - w), 10.min(height - h));
    Rect::new(width - w - right, height - h - bottom, w, h)
}

/// A terminal's body: the clearest patch near its bottom right. Text and
/// TUI widgets are keyboard driven, so they never become separate hints.
/// If the lower half is completely occupied, its least busy patch.
pub fn terminal(frame: &Frame) -> Corners {
    let space = Space::pixels(frame);
    let (w, h) = (frame.width as i32, frame.height as i32);
    let preferred = terminal_patch(w, h);
    if space.ink(preferred) == 0 {
        return [
            preferred.x,
            preferred.y,
            preferred.right(),
            preferred.bottom(),
        ];
    }
    let (bw, bh) = (preferred.w, preferred.h);
    let (last_x, last_y) = (preferred.x, preferred.y);
    let (x, y) = (4.min(last_x), (h / 2).min(last_y));
    let area = Rect::new(x, y, last_x - x + bw, last_y - y + bh);
    let aim = preferred.center();
    let r = space
        .spot(area, (bw, bh), aim, 2)
        .unwrap_or(Rect::new(0, 0, w, h));
    [r.x, r.y, r.right(), r.bottom()]
}

/// A sheet's body: the clearest patch nearest its middle, 4 px in from its
/// edges, or its middle when the patch doesn't fit.
pub fn sheet(space: &Space, grid: Corners) -> Corners {
    let area = Rect::new(grid[0], grid[1], width(grid), height(grid));
    let (bw, bh) = (36.min(area.w), 24.min(area.h));
    let middle = area.center();
    let r = space
        .spot(area.inset(4), (bw, bh), middle, 1)
        .unwrap_or(Rect::new(middle.x - bw / 2, middle.y - bh / 2, bw, bh));
    [r.x, r.y, r.right(), r.bottom()]
}

/// The words whose centres lie in `p`, in their order.
fn members(words: &[Corners], p: Corners) -> impl Iterator<Item = Corners> + '_ {
    words.iter().copied().filter(move |&w| inside(centre(w), p))
}

/// The edge pass's boxes plus the colour pass's that overlap none of them:
/// the colour pass only fills in where the edges found nothing. Dashed
/// lines it fills in with are dropped.
pub fn fill(base: &[Corners], low: &[Corners]) -> Vec<Corners> {
    let overlaps =
        |a: Corners, b: Corners| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
    let near = Buckets::new(base);
    let added: Vec<Corners> = low
        .iter()
        .copied()
        .filter(|&b| !near.near(b).any(|j| overlaps(b, base[j])))
        .collect();
    let dashed = dashes(&added);
    base.iter()
        .copied()
        .chain(added.into_iter().filter(|b| !dashed.contains(b)))
        .collect()
}

/// Pieces of a dashed or dotted line: at most 14 px both ways, one size
/// give or take 1 px, 4 or more in a column or a row. Words on a line
/// differ in width, so they don't count.
fn dashes(boxes: &[Corners]) -> HashSet<Corners> {
    // Python's `round`, half to even, which the groups were keyed with.
    let round = |v: f64| v.round_ties_even() as i32;
    let small: Vec<Corners> = boxes
        .iter()
        .copied()
        .filter(|&b| width(b) <= 14 && height(b) <= 14)
        .collect();
    let mut out = HashSet::new();
    for axis in [0, 1] {
        let mut groups: HashMap<(i32, i32, i32), Vec<Corners>> = HashMap::new();
        for &b in &small {
            let key = (
                round(f64::from(b[axis]) / 3.0),
                round(f64::from(width(b)) / 2.0),
                round(f64::from(height(b)) / 2.0),
            );
            groups.entry(key).or_default().push(b);
        }
        out.extend(groups.into_values().filter(|g| g.len() >= 4).flatten());
    }
    out
}

/// Words on one baseline join across gaps of at most `join` x their
/// height, except two icon-shaped boxes, and words in clearly different
/// ink. Boxes that aren't text-sized pass through.
fn phrases(words: &[Corners], inks: &mut Inks, join: f64) -> Vec<Corners> {
    let mut text: Vec<Corners> = words.iter().copied().filter(|&b| is_text(b)).collect();
    // By centre height; `sort_by_key` is stable, as Python's `sorted` is.
    text.sort_by_key(|b| b[1] + b[3]);
    let mut bands: Vec<Vec<Corners>> = Vec::new();
    for b in text {
        let fits = |a: Corners| {
            let lo = height(a).min(height(b));
            ((b[1] + b[3]) - (a[1] + a[3])).abs() <= lo && lo * 2 >= height(a).max(height(b))
        };
        let start = bands.len().saturating_sub(12);
        match bands[start..].iter_mut().rev().find(|band| fits(band[0])) {
            Some(band) => band.push(b),
            None => bands.push(vec![b]),
        }
    }
    let mut apart = |a: Corners, b: Corners| {
        if icon(a) && icon(b) {
            return true;
        }
        match (inks.of(a), inks.of(b)) {
            (Some(i), Some(j)) => (0..3).any(|c| (i.colour[c] - j.colour[c]).abs() > INK),
            _ => false,
        }
    };
    let mut out = Vec::new();
    for mut band in bands {
        band.sort_unstable();
        let (mut cur, mut prev) = (band[0], band[0]);
        for &b in &band[1..] {
            let h = height(cur).max(height(b));
            if f64::from(b[0] - cur[2]) <= join * f64::from(h) && !apart(prev, b) {
                cur = union(&[cur, b]);
            } else {
                out.push(cur);
                cur = b;
            }
            prev = b;
        }
        out.push(cur);
    }
    out.extend(words.iter().copied().filter(|&b| !is_text(b)));
    out
}

/// Icon-shaped: 10 to 48 px tall, about square.
fn icon(b: Corners) -> bool {
    let (w, h) = (f64::from(width(b)), f64::from(height(b)));
    (10..=48).contains(&height(b)) && (0.7..=1.4).contains(&(w / h))
}

/// One step of a text block: `b` sits a line or two below or above `a`,
/// about as tall, and lined up with it.
fn stacked(a: Corners, b: Corners) -> bool {
    let h = f64::from(height(a).max(height(b)));
    let dy = f64::from(((a[1] + a[3]) - (b[1] + b[3])).abs()) / 2.0;
    let overlap = f64::from(a[2].min(b[2]) - a[0].max(b[0]));
    (0.8 * h..=2.8 * h).contains(&dy)
        && f64::from(height(a).min(height(b))) >= 0.65 * h
        && (overlap >= 0.3 * f64::from(width(a).min(width(b)))
            || f64::from((a[0] - b[0]).abs()) <= h)
}

/// Text phrases joined by [`stacked`] steps, with union-find, in the order
/// the prototype made them.
fn blocks(phrases: &[Corners]) -> Vec<Vec<Corners>> {
    let text: Vec<Corners> = phrases.iter().copied().filter(|&p| is_text(p)).collect();
    let mut parent: Vec<usize> = (0..text.len()).collect();
    let root = |parent: &mut Vec<usize>, mut i: usize| {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    };
    // A step is at most 2.8 text heights between centres, and a text
    // height sideways when the two don't overlap: the buckets under that
    // much around a phrase hold every phrase it steps to. Which pairs join
    // decides the blocks, not the order they join in.
    let reach = *TEXT.end();
    let near = Buckets::new(&text);
    let mut seen = vec![usize::MAX; text.len()];
    for (i, &a) in text.iter().enumerate() {
        for j in near.near([
            a[0] - reach,
            a[1] - 3 * reach,
            a[2] + reach,
            a[3] + 3 * reach,
        ]) {
            // Once per pair: a phrase in several cells comes once per cell.
            if j <= i || seen[j] == i {
                continue;
            }
            seen[j] = i;
            if stacked(a, text[j]) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut at: HashMap<usize, usize> = HashMap::new();
    let mut out: Vec<Vec<Corners>> = Vec::new();
    for (i, &p) in text.iter().enumerate() {
        let r = root(&mut parent, i);
        let g = *at.entry(r).or_insert_with(|| {
            out.push(Vec::new());
            out.len() - 1
        });
        out[g].push(p);
    }
    out
}

/// A block worth one tag: 3 phrases or more, 2 of them long lines.
fn prose(g: &[Corners]) -> bool {
    g.len() >= 3
        && g.iter()
            .filter(|&&p| width(p) >= 120.max(8 * height(p)))
            .count()
            >= 2
}

/// A block's phrases in lines, top to bottom, each line left to right.
fn rows(g: &[Corners]) -> Vec<Vec<Corners>> {
    let mut sorted = g.to_vec();
    sorted.sort_by_key(|p| p[1] + p[3]);
    // Each line: the doubled centre and the height of its first phrase.
    let mut out: Vec<(i32, i32, Vec<Corners>)> = Vec::new();
    for p in sorted {
        let (cy, h) = (p[1] + p[3], height(p));
        match out.last_mut() {
            Some((ly, lh, items)) if (cy - *ly).abs() <= h.max(*lh) => items.push(p),
            _ => out.push((cy, h, vec![p])),
        }
    }
    out.into_iter()
        .map(|(_, _, mut items)| {
            items.sort_unstable();
            items
        })
        .collect()
}

/// Whether a prose-shaped block collapses to one tag. It stays open when
/// its rows start with icons, or when it reads as a list (few long rows
/// and few words per row), unless it's code by the policy's gate.
fn collapses(g: &[Corners], words: &[Corners], inks: &mut Inks, policy: &Policy) -> bool {
    let rs = rows(g);
    let n = rs.len() as f64;
    let span = f64::from(width(union(g)));
    let long = rs
        .iter()
        .filter(|r| f64::from(r[r.len() - 1][2] - r[0][0]) >= 0.7 * span)
        .count() as f64
        / n;
    let icons = rs
        .iter()
        .filter(|r| lead_icon(inks.frame, words, r))
        .count() as f64
        / n;
    let mut per_row: Vec<usize> = rs
        .iter()
        .map(|r| r.iter().map(|&p| members(words, p).count()).sum())
        .collect();
    per_row.sort_unstable();
    let row_words = per_row[rs.len() / 2];

    let mut open = icons >= ICON_ROWS || (long < policy.open.0 && row_words < policy.open.1);
    if let Some(at) = policy.code
        && open
        && row_words >= 2
        && icons < ICON_ROWS
        && mono(g, words) >= at
    {
        // Code: a monospace grid, repeated indents, more than one colour.
        let indents = indents(&rs);
        let colours = colours(g, inks)
            .iter()
            .filter(|c| c.members.len() >= 3)
            .count();
        if indents >= 2 && colours >= 2 {
            open = false;
        }
    }
    !open
}

/// How many indent levels repeat: the rows' left edges, chained within
/// 4 px, in clusters of two or more.
fn indents(rs: &[Vec<Corners>]) -> usize {
    let mut lefts: Vec<i32> = rs.iter().map(|r| r[0][0]).collect();
    lefts.sort_unstable();
    let mut clusters: Vec<Vec<i32>> = Vec::new();
    for x in lefts {
        match clusters.last_mut() {
            Some(c) if x - c[c.len() - 1] <= 4 => c.push(x),
            _ => clusters.push(vec![x]),
        }
    }
    clusters.iter().filter(|c| c.len() >= 2).count()
}

/// The row's first word is icon-shaped and one solid shape (unlike a line
/// number), and stands apart from the next word, or is its phrase alone.
fn lead_icon(frame: &Frame, words: &[Corners], row: &[Corners]) -> bool {
    let mut inner: Vec<Corners> = members(words, row[0]).collect();
    inner.sort_unstable();
    let Some(&first) = inner.first() else {
        return false;
    };
    if !icon(first) || !solid(frame, first) {
        return false;
    }
    (inner.len() == 1 && row.len() > 1) || (inner.len() > 1 && inner[1][0] - first[2] >= 4)
}

/// How strongly the words' left edges lock to one character advance (0.4
/// to 0.75 text heights): the circular concentration at the best period,
/// times the square root of the count, so a random layout scores 1 or 2
/// whatever its size. Monospace text scores high.
fn mono(g: &[Corners], words: &[Corners]) -> f64 {
    let ws: Vec<Corners> = words
        .iter()
        .copied()
        .filter(|&w| g.iter().any(|&p| inside(centre(w), p)))
        .collect();
    if ws.len() < 20 {
        return 0.0;
    }
    let h = median(g.iter().map(|&p| f64::from(height(p))).collect());
    let left = f64::from(union(g)[0]);
    let xs: Vec<f64> = ws.iter().map(|w| f64::from(w[0]) - left).collect();
    // NumPy's `arange(0.4 h, 0.75 h, 0.02)`: its length, and its step as
    // the difference of its first two values.
    let (start, stop) = (0.4 * h, 0.75 * h);
    let delta = (start + 0.02) - start;
    let count = ((stop - start) / 0.02).ceil().max(0.0) as usize;
    let best = (0..count)
        .map(|i| {
            let a = start + i as f64 * delta;
            let (mut re, mut im) = (0.0, 0.0);
            for &x in &xs {
                let t = 2.0 * std::f64::consts::PI * x / a;
                re += t.cos();
                im += t.sin();
            }
            let n = xs.len() as f64;
            (re / n).hypot(im / n)
        })
        .fold(0.0, f64::max);
    best * (ws.len() as f64).sqrt()
}

/// One ink colour among a block's phrases, and how wide its phrases are.
struct Cluster {
    colour: [f64; 3],
    width: i32,
    members: Vec<(Corners, Ink)>,
}

/// Greedy colour clusters of the phrases' ink, widest phrase first,
/// heaviest cluster first. Phrases with no ink are left out.
fn colours(g: &[Corners], inks: &mut Inks) -> Vec<Cluster> {
    let mut sorted = g.to_vec();
    sorted.sort_by_key(|&p| -width(p));
    let mut out: Vec<Cluster> = Vec::new();
    for p in sorted {
        let Some(ink) = inks.of(p) else {
            continue;
        };
        let near = |c: &&mut Cluster| (0..3).all(|k| (c.colour[k] - ink.colour[k]).abs() <= INK);
        match out.iter_mut().find(near) {
            Some(c) => {
                c.width += width(p);
                c.members.push((p, ink));
            }
            None => out.push(Cluster {
                colour: ink.colour,
                width: width(p),
                members: vec![(p, ink)],
            }),
        }
    }
    out.sort_by_key(|c| -c.width);
    out
}

/// The phrases of a block drawn in a link colour: the block has one body
/// colour and at most [`LINK_COLOURS`] others, each clearly coloured and
/// on [`LINK_PHRASES`] phrases or more. Syntax-coloured code has more
/// colours than that, so it gives none.
fn links(g: &[Corners], inks: &mut Inks) -> Vec<Corners> {
    let cs = colours(g, inks);
    let total: i32 = cs.iter().map(|c| c.width).sum();
    let Some(body) = cs.first() else {
        return Vec::new();
    };
    if f64::from(body.width) < BODY_SHARE * f64::from(total) {
        return Vec::new();
    }
    let others: Vec<&Cluster> = cs[1..]
        .iter()
        .filter(|c| {
            c.members.len() >= LINK_PHRASES || f64::from(c.width) >= 0.05 * f64::from(body.width)
        })
        .collect();
    if others.len() > LINK_COLOURS {
        return Vec::new();
    }
    others
        .into_iter()
        .filter(|c| {
            let chroma: f64 = c.members.iter().map(|(_, ink)| ink.chroma).sum();
            c.members.len() >= LINK_PHRASES && chroma / c.members.len() as f64 >= LINK_CHROMA
        })
        .flat_map(|c| c.members.iter().map(|&(p, _)| p))
        .collect()
}

/// A box's ink: the pixels far from the median of its 1 px border ring.
#[derive(Debug, Clone, Copy)]
struct Ink {
    /// The median ink colour, per channel.
    colour: [f64; 3],
    /// How colourful: the mean of each ink pixel's max - min channel.
    chroma: f64,
}

/// [`Ink`]s by box, worked out once each.
struct Inks<'a> {
    frame: &'a Frame,
    cache: HashMap<Corners, Option<Ink>>,
}

impl<'a> Inks<'a> {
    fn new(frame: &'a Frame) -> Self {
        Self {
            frame,
            cache: HashMap::new(),
        }
    }

    fn of(&mut self, b: Corners) -> Option<Ink> {
        let frame = self.frame;
        *self.cache.entry(b).or_insert_with(|| {
            let patch = Patch::around(frame, b)?;
            if patch.w < 3 || patch.h < 3 {
                return None;
            }
            let bg = patch.ring_median();
            let (mut ink, mut chroma) = (Counts::new(), 0);
            for p in patch.inner().filter(|&p| far(p, bg)) {
                ink.add(p);
                chroma += u32::from(p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]));
            }
            if ink.n < 4 {
                return None;
            }
            Some(Ink {
                colour: ink.median(),
                chroma: f64::from(chroma) / f64::from(ink.n),
            })
        })
    }
}

/// One solid shape: no column inside the box is free of ink, as the gap
/// between a line number's digits is.
fn solid(frame: &Frame, b: Corners) -> bool {
    let Some(patch) = Patch::around(frame, b) else {
        return false;
    };
    if patch.w < 3 || patch.h < 3 {
        return false;
    }
    let bg = patch.ring_median();
    let inked: Vec<bool> = (1..patch.w - 1)
        .map(|x| (1..patch.h - 1).any(|y| far(patch.at(x, y), bg)))
        .collect();
    match (
        inked.iter().position(|&i| i),
        inked.iter().rposition(|&i| i),
    ) {
        (Some(first), Some(last)) => inked[first..=last].iter().all(|&i| i),
        _ => false,
    }
}

/// A box grown by 1 px, clipped to the frame: the box and its ring.
struct Patch<'a> {
    frame: &'a Frame,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
}

impl<'a> Patch<'a> {
    fn around(frame: &'a Frame, b: Corners) -> Option<Self> {
        let clip = |v: i32, n: usize| v.clamp(0, n as i32) as usize;
        let (x0, y0) = (clip(b[0] - 1, frame.width), clip(b[1] - 1, frame.height));
        let (x1, y1) = (clip(b[2] + 1, frame.width), clip(b[3] + 1, frame.height));
        (x1 > x0 && y1 > y0).then_some(Self {
            frame,
            x0,
            y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = ((self.y0 + y) * self.frame.width + self.x0 + x) * 3;
        let p = &self.frame.rgb;
        [p[i], p[i + 1], p[i + 2]]
    }

    /// The median of the ring, per channel. The ring is NumPy's: top row,
    /// bottom row, left column, right column, so each corner counts twice.
    fn ring_median(&self) -> [f64; 3] {
        let (w, h) = (self.w, self.h);
        let mut ring = Counts::new();
        for x in 0..w {
            ring.add(self.at(x, 0));
            ring.add(self.at(x, h - 1));
        }
        for y in 0..h {
            ring.add(self.at(0, y));
            ring.add(self.at(w - 1, y));
        }
        ring.median()
    }

    /// The pixels inside the ring, row by row.
    fn inner(&self) -> impl Iterator<Item = [u8; 3]> + '_ {
        (1..self.h - 1).flat_map(move |y| (1..self.w - 1).map(move |x| self.at(x, y)))
    }
}

/// Ink: a channel more than [`INK`] from the background.
fn far(p: [u8; 3], bg: [f64; 3]) -> bool {
    (0..3).any(|c| (f64::from(p[c]) - bg[c]).abs() > INK)
}

/// How many pixels have each value, per channel: the medians of a box's
/// colours without sorting them.
struct Counts {
    bins: [[u32; 256]; 3],
    n: u32,
}

impl Counts {
    const fn new() -> Self {
        Self {
            bins: [[0; 256]; 3],
            n: 0,
        }
    }

    fn add(&mut self, p: [u8; 3]) {
        for (bins, v) in self.bins.iter_mut().zip(p) {
            bins[usize::from(v)] += 1;
        }
        self.n += 1;
    }

    /// [`median`] of each channel's values, from the counts.
    fn median(&self) -> [f64; 3] {
        let n = self.n;
        self.bins.each_ref().map(|bins| {
            // The `k`th smallest value, from 0.
            let kth = |k: u32| {
                let mut seen = 0;
                bins.iter()
                    .position(|&c| {
                        seen += c;
                        seen > k
                    })
                    .map_or(255.0, |v| v as f64)
            };
            if n % 2 == 1 {
                kth(n / 2)
            } else {
                f64::midpoint(kth(n / 2 - 1), kth(n / 2))
            }
        })
    }
}

/// NumPy's median: the middle value, or the mean of the middle two.
fn median(mut v: Vec<f64>) -> f64 {
    let n = v.len();
    v.sort_unstable_by(f64::total_cmp);
    if n % 2 == 1 {
        v[n / 2]
    } else {
        f64::midpoint(v[n / 2 - 1], v[n / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_medians_are_sorted_medians() {
        // Odd and even counts, repeats, and a pair across a gap (0 and 255).
        let sets: [&[[u8; 3]]; 4] = [
            &[[7, 0, 255]],
            &[[0, 9, 3], [255, 9, 4]],
            &[[5, 1, 1], [5, 2, 200], [6, 2, 9], [1, 250, 9]],
            &[[3, 3, 3], [1, 2, 3], [9, 9, 9], [4, 5, 6], [4, 0, 6]],
        ];
        for set in sets {
            let mut counts = Counts::new();
            for &p in set {
                counts.add(p);
            }
            let sorted = [0, 1, 2].map(|c| median(set.iter().map(|p| f64::from(p[c])).collect()));
            assert_eq!(counts.median(), sorted, "{set:?}");
        }
    }
}
