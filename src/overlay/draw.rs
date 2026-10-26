//! Drawing tags into a pixel buffer, in software.
//!
//! `tiny-skia` paints the boxes (anti-aliased rounded rectangles) into a
//! `Pixmap`; `fontdue` rasterizes each letter into a coverage map (how much
//! of each pixel the glyph covers, 0-255), which becomes a small coloured
//! pixmap drawn on top. No GPU: a frame is a few dozen small shapes, and a
//! CPU draws that in well under a millisecond. Dots, for where a nudged
//! command lands, are two filled circles, each on a line back to its tag.
//!
//! Everything is laid out in logical px, as the rest of wayhint counts,
//! and drawn at the monitor's scale: shapes through a scaling transform,
//! letters rasterized at their size times the scale. So a tag is the same
//! size on every monitor, and sharp on a scaled one.

use std::collections::HashMap;

use anyhow::Result;
use tiny_skia::{
    BlendMode, FillRule, IntRect, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    PremultipliedColorU8, Rect, Stroke, Transform,
};

use crate::{
    geometry::{Point, Rect as BoxRect},
    overlay::font,
    placement::{self, Label},
    space::Space,
    style::{Color, Mark, Marks, Position, Style},
};

/// Gap kept between a tag and the monitor's edge, in px.
const EDGE: f32 = 2.0;

/// Everything one picture shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub tags: Vec<Tag>,
    /// Where nudged commands land: the tag says which, the dot says where,
    /// and the line between says which dot is whose.
    pub dots: Vec<Dot>,
    /// Text mode's matches, under everything else.
    pub highlights: Vec<Highlight>,
    /// The input box: an armed verb, `drop`, text mode's query.
    pub input: Option<Note>,
    /// The info box: scanning, no targets, a dropped key, the key legend.
    pub info: Option<Note>,
}

impl Frame {
    /// Whether the tags, lines, dots and marks are all gone: the boxes
    /// aside, a blank screen.
    pub fn is_bare(&self) -> bool {
        self.tags.is_empty() && self.dots.is_empty() && self.highlights.is_empty()
    }
}

/// What a box on the screen says. Where it goes is its table's: `[input]`
/// or `[info]`. Lines are split at `\n`, and a line's columns at `\t`,
/// lined up across lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    /// How many leading characters are drawn dimmed: a verb's name, a
    /// placeholder.
    pub typed: usize,
}

impl Note {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            typed: 0,
        }
    }
}

/// The boxes apart from the tags, each with its own look and spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Input,
    Info,
}

/// A translucent box over one line of a match in text mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub rect: BoxRect,
    /// Part of the current match, drawn in `[text.matches.current]`.
    pub current: bool,
}

/// Where a nudged command lands, `at`, and its source: a tag's outline,
/// or `from` at the pointer for `Space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dot {
    pub from: Point,
    pub at: Point,
    /// The tag whose outline the line follows; `None` starts at `from`.
    pub tag: Option<usize>,
}

/// One tag to draw, in overlay coordinates (the monitor's top-left is 0, 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Where the tag goes: that point on its target's box.
    pub at: Point,
    /// Which of the tag's own points sits on `at`: its bottom-right corner
    /// for `BottomRight`, so the tag lies up and left of it.
    pub position: Position,
    pub text: String,
    /// How many leading letters are already typed (drawn dimmed).
    pub typed: usize,
    /// The click point; absent for a box. Placement never changes it.
    pub target: Option<Point>,
}

/// A rasterized letter: where it sits relative to the pen, and its pixels.
struct Glyph {
    metrics: fontdue::Metrics,
    /// `None` for glyphs with no ink (a space).
    pixmap: Option<Pixmap>,
}

/// Draws tags, matches and the boxes, caching each font's rasterized
/// glyphs.
pub struct Painter {
    tags: TextPainter,
    input: TextPainter,
    info: TextPainter,
    marks: Marks,
    /// Buffer px per logical px: the monitor's scale.
    scale: f32,
    space: Option<Space>,
    regions: Vec<BoxRect>,
    /// How far each tag was moved off its own box, keyed by its anchor and
    /// click point: a verb typed in front of the letters changes the text
    /// and the box's size, never where the tag points.
    placed: HashMap<(Point, Point), Point>,
}

impl Painter {
    /// The tags in `style`, text mode's marks in `marks`, the boxes in
    /// `input` and `info`, drawn at `scale` buffer px per logical px.
    pub fn new(
        style: &Style,
        marks: &Marks,
        input: &Style,
        info: &Style,
        scale: f32,
    ) -> Result<Self> {
        Ok(Self {
            tags: TextPainter::new(style, scale)?,
            input: TextPainter::new(input, scale)?,
            info: TextPainter::new(info, scale)?,
            marks: *marks,
            scale,
            space: None,
            regions: Vec::new(),
            placed: HashMap::new(),
        })
    }

    /// A fresh scan invalidates placement; typing keeps the same positions.
    pub fn set_space(&mut self, area: BoxRect, obstacles: &[BoxRect], regions: Vec<BoxRect>) {
        self.space = Some(Space::rectangles(area, obstacles));
        self.regions = regions;
        self.placed.clear();
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn arrange(&mut self, frame: &Frame, area: (f32, f32)) -> Frame {
        let screen = BoxRect::new(0, 0, area.0 as i32, area.1 as i32).inset(2);
        let originals: Vec<_> = frame
            .tags
            .iter()
            .map(|tag| {
                let rect = place(tag.at, tag.position, self.tags.size(&tag.text), area).unwrap();
                BoxRect::new(
                    rect.x() as i32,
                    rect.y() as i32,
                    rect.width() as i32,
                    rect.height() as i32,
                )
            })
            .collect();
        let labels: Vec<_> = frame
            .tags
            .iter()
            .zip(&originals)
            .map(|(tag, &rect)| {
                let moved = tag.target.and_then(|hit| self.placed.get(&(tag.at, hit)));
                let bounds = tag
                    .target
                    .and_then(|p| self.regions.iter().rev().find(|r| r.contains(p)))
                    .and_then(|r| r.intersect(screen))
                    .unwrap_or(screen);
                Label {
                    rect: moved.map_or(rect, |&by| rect.offset(by)),
                    target: tag.target,
                    bounds,
                }
            })
            .collect();
        let space = self
            .space
            .get_or_insert_with(|| Space::rectangles(screen, &[]));
        let boxes = placement::resolve(&labels, space);
        let mut result = frame.clone();
        for (i, ((tag, &original), rect)) in
            frame.tags.iter().zip(&originals).zip(boxes).enumerate()
        {
            // The first placement after a scan holds: two tags with one
            // anchor and click point then start from the same spot each
            // time, and resolve the same way.
            if let Some(hit) = tag.target {
                let by = Point::new(rect.x - original.x, rect.y - original.y);
                self.placed.entry((tag.at, hit)).or_insert(by);
            }
            result.tags[i].at = rect.origin();
            result.tags[i].position = Position::TopLeft;
            if rect != original
                && let Some(at) = tag.target
                && !result.dots.iter().any(|d| d.tag == Some(i))
            {
                result.dots.push(Dot {
                    from: rect.center(),
                    at,
                    tag: Some(i),
                });
            }
        }
        result
    }

    /// `pixmap`'s size in logical px.
    #[allow(clippy::cast_precision_loss)] // monitor sizes are far below 2^24
    fn area(&self, pixmap: &Pixmap) -> (f32, f32) {
        (
            pixmap.width() as f32 / self.scale,
            pixmap.height() as f32 / self.scale,
        )
    }

    /// Draws `frame` on a cleared `pixmap`, boxes and all, at full
    /// opacity.
    pub fn paint(&mut self, pixmap: &mut Pixmap, frame: &Frame) {
        self.paint_board(pixmap, frame);
        if let Some(note) = &frame.input {
            self.paint_note(pixmap, Panel::Input, note, 1.0);
        }
        if let Some(note) = &frame.info {
            self.paint_note(pixmap, Panel::Info, note, 1.0);
        }
    }

    /// Clears `pixmap` and draws `frame`'s tags, lines, dots and marks on
    /// it, in overlay coordinates; not its boxes.
    ///
    /// Highlights go first, under everything. Lines start at their tags'
    /// nearest edges and go under the tags. Dots follow.
    pub fn paint_board(&mut self, pixmap: &mut Pixmap, frame: &Frame) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let scaled = Transform::from_scale(self.scale, self.scale);
        for highlight in &frame.highlights {
            self.paint_highlight(pixmap, *highlight, scaled);
        }
        let area = self.area(pixmap);
        let arranged = self.arrange(frame, area);
        let frame = &arranged;
        let mut lines = Vec::new();
        for &dot in &frame.dots {
            let from = if let Some(tag) = dot.tag.and_then(|i| frame.tags.get(i)) {
                let Some(rect) = place(tag.at, tag.position, self.tags.size(&tag.text), area)
                else {
                    continue;
                };
                let Some(from) = nearest_edge(rect, self.tags.style.radius, centre(dot.at)) else {
                    continue;
                };
                from
            } else {
                centre(dot.from)
            };
            lines.extend(self.line(from, centre(dot.at)));
        }
        paint_layer(pixmap, &lines, self.scale);
        for tag in &frame.tags {
            self.tags.paint_tag(pixmap, tag, area, 1.0);
        }
        let dots: Vec<Part> = frame.dots.iter().flat_map(|dot| self.dot(dot.at)).collect();
        paint_layer(pixmap, &dots, self.scale);
    }

    /// Draws one box over `pixmap` at `opacity`, where its table puts it:
    /// its position on the screen, moved by its offset.
    #[allow(clippy::cast_possible_truncation)] // logical sizes fit an i32
    pub fn paint_note(&mut self, pixmap: &mut Pixmap, panel: Panel, note: &Note, opacity: f32) {
        let (w, h) = self.area(pixmap);
        let painter = match panel {
            Panel::Input => &mut self.input,
            Panel::Info => &mut self.info,
        };
        let style = &painter.style;
        let [x, y] = style.offset;
        let tag = Tag {
            at: style
                .position
                .on(BoxRect::new(0, 0, w as i32, h as i32))
                .offset(Point::new(x, y)),
            position: style.position,
            text: note.text.clone(),
            typed: note.typed,
            target: None,
        };
        painter.paint_tag(pixmap, &tag, (w, h), opacity);
    }

    /// The line from a tag to where it clicks, inside its outline, so it
    /// shows on light and dark windows alike: `[style.leader]`, by default
    /// the tags' letters on their background. It stops where its outline
    /// meets the dot's, so a translucent dot shows no line through it.
    fn line(&self, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> Vec<Part> {
        let style = &self.tags.style;
        let leader = &style.leader;
        let (line, _, outline) = leader.colors(style);
        let reach = leader.radius + leader.outline_width;
        let half = f32::midpoint(leader.width, leader.outline_width);
        let stop = (reach * reach - half * half).max(0.0).sqrt();
        let length = (x1 - x0).hypot(y1 - y0);
        if length <= stop {
            return Vec::new();
        }
        let keep = 1.0 - stop / length;
        let mut path = PathBuilder::new();
        path.move_to(x0, y0);
        path.line_to(x0 + (x1 - x0) * keep, y0 + (y1 - y0) * keep);
        let Some(path) = path.finish() else {
            return Vec::new();
        };
        // tiny-skia draws a 0 px stroke a pixel wide, so a part set to 0
        // is left out, not stroked.
        [
            (
                leader.width + leader.outline_width,
                outline,
                leader.outline_width > 0.0,
            ),
            (leader.width, line, leader.width > 0.0),
        ]
        .into_iter()
        .filter(|&(_, _, shown)| shown)
        .map(|(width, color, _)| Part {
            path: path.clone(),
            width: Some(width),
            color,
        })
        .collect()
    }

    /// The dot where a tag clicks, inside its outline: `[style.leader]`.
    fn dot(&self, at: Point) -> Vec<Part> {
        let style = &self.tags.style;
        let leader = &style.leader;
        let (_, dot, outline) = leader.colors(style);
        let (x, y) = centre(at);
        [
            (leader.radius + leader.outline_width, outline),
            (leader.radius, dot),
        ]
        .into_iter()
        .filter_map(|(radius, color)| {
            Some(Part {
                path: PathBuilder::from_circle(x, y, radius)?,
                width: None,
                color,
            })
        })
        .collect()
    }

    /// One line of a match: its box grown by the mark's padding, filled
    /// and bordered as a tag is, with no letters.
    #[allow(clippy::cast_precision_loss)] // screen coordinates are far below 2^24
    fn paint_highlight(&self, pixmap: &mut Pixmap, highlight: Highlight, scaled: Transform) {
        let mark = if highlight.current {
            self.marks.current
        } else {
            self.marks.all
        };
        let [pad_y, pad_x] = mark.padding;
        let r = highlight.rect;
        let Some(rect) = Rect::from_xywh(
            r.x as f32 - pad_x,
            r.y as f32 - pad_y,
            r.w as f32 + 2.0 * pad_x,
            r.h as f32 + 2.0 * pad_y,
        ) else {
            return;
        };
        paint_box(pixmap, rect, &mark, scaled);
    }
}

/// A font and glyph cache for one look; tags and each box have their own.
struct TextPainter {
    style: Style,
    font: fontdue::Font,
    /// Buffer px per logical px: glyphs are rasterized this much bigger.
    scale: f32,
    /// Distance from a line's top to its baseline, and its full height,
    /// in logical px.
    ascent: f32,
    line_height: f32,
    /// Keyed by letter and whether it's drawn in the typed colour.
    glyphs: HashMap<(char, bool), Glyph>,
}

impl TextPainter {
    fn new(style: &Style, scale: f32) -> Result<Self> {
        let font = font::load(&style.font, style.size * scale)?;
        let (ascent, line_height) = font
            .horizontal_line_metrics(style.size)
            .map_or((style.size, style.size * 1.2), |m| {
                (m.ascent, m.ascent - m.descent)
            });
        Ok(Self {
            style: style.clone(),
            font,
            scale,
            ascent,
            line_height,
            glyphs: HashMap::new(),
        })
    }

    /// How far a letter moves the pen, in logical px.
    fn advance(&self, letter: char) -> f32 {
        self.font.metrics(letter, self.style.size).advance_width
    }

    /// Each column's width, the widest of its cells in every line.
    fn columns(&self, text: &str) -> Vec<f32> {
        let mut widths: Vec<f32> = Vec::new();
        for line in text.split('\n') {
            for (i, cell) in line.split('\t').enumerate() {
                let width = cell.chars().map(|c| self.advance(c)).sum();
                match widths.get_mut(i) {
                    Some(widest) => *widest = widest.max(width),
                    None => widths.push(width),
                }
            }
        }
        widths
    }

    /// The gap between two columns: the box's side padding, twice.
    fn gap(&self) -> f32 {
        2.0 * self.style.padding[1]
    }

    /// The box's size: its text plus padding on every side.
    #[allow(clippy::cast_precision_loss)] // a box has a few lines
    fn size(&self, text: &str) -> (f32, f32) {
        let [pad_y, pad_x] = self.style.padding;
        let columns = self.columns(text);
        let gaps = columns.len().saturating_sub(1) as f32 * self.gap();
        let width: f32 = columns.iter().sum::<f32>() + gaps;
        let lines = text.split('\n').count() as f32;
        (
            (width + 2.0 * pad_x).ceil(),
            (self.line_height * lines + 2.0 * pad_y).ceil(),
        )
    }

    /// Draws `tag` on `pixmap` at `opacity`. A box fading in or out is
    /// drawn alone first, then laid over at that opacity, so its letters
    /// don't show its background through them.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn paint_tag(&mut self, pixmap: &mut Pixmap, tag: &Tag, area: (f32, f32), opacity: f32) {
        let size = self.size(&tag.text);
        let Some(rect) = place(tag.at, tag.position, size, area) else {
            return;
        };
        if opacity >= 1.0 {
            self.draw(pixmap, rect, &tag.text, tag.typed, (0.0, 0.0));
            return;
        }
        let s = self.scale;
        let (left, top) = ((rect.left() * s).floor(), (rect.top() * s).floor());
        let (right, bottom) = ((rect.right() * s).ceil(), (rect.bottom() * s).ceil());
        let Some(mut layer) = Pixmap::new((right - left) as u32 + 1, (bottom - top) as u32 + 1)
        else {
            return;
        };
        self.draw(&mut layer, rect, &tag.text, tag.typed, (left, top));
        let paint = PixmapPaint {
            opacity: opacity.clamp(0.0, 1.0),
            ..PixmapPaint::default()
        };
        pixmap.draw_pixmap(
            left as i32,
            top as i32,
            layer.as_ref(),
            &paint,
            Transform::identity(),
            None,
        );
    }

    /// The box at `rect` (logical px) and its letters, on `target`, whose
    /// top-left corner is `shift` buffer px into the overlay.
    fn draw(
        &mut self,
        target: &mut Pixmap,
        rect: Rect,
        text: &str,
        typed: usize,
        shift: (f32, f32),
    ) {
        let s = self.scale;
        let transform = Transform::from_scale(s, s).post_translate(-shift.0, -shift.1);
        let style = &self.style;
        paint_box(
            target,
            rect,
            &Mark {
                background: style.background,
                border: style.border,
                border_width: style.border_width,
                radius: style.radius,
                padding: style.padding,
            },
            transform,
        );
        // The letters, on a baseline `ascent` below the padding, a line
        // height further for each line; each column starts past the
        // widest cell of the one before.
        let [pad_y, pad_x] = style.padding;
        let columns = self.columns(text);
        let gap = self.gap();
        let mut index = 0;
        let mut baseline = rect.top() + pad_y + self.ascent;
        for line in text.split('\n') {
            let mut start = rect.left() + pad_x;
            for (cell, width) in line.split('\t').zip(&columns) {
                let mut pen = start;
                for letter in cell.chars() {
                    let advance = self.advance(letter);
                    let glyph = self.glyph(letter, index < typed);
                    let m = glyph.metrics;
                    if let Some(ink) = &glyph.pixmap {
                        // fontdue measures `ymin` up from the baseline to the
                        // glyph's bottom edge; pixmaps count down from the top.
                        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                        let (x, y) = (
                            (pen * s - shift.0 + m.xmin as f32).round() as i32,
                            (baseline * s - shift.1 - m.ymin as f32 - m.height as f32).round()
                                as i32,
                        );
                        target.draw_pixmap(
                            x,
                            y,
                            ink.as_ref(),
                            &PixmapPaint::default(),
                            Transform::identity(),
                            None,
                        );
                    }
                    pen += advance;
                    index += 1;
                }
                // The tab or line break counts as a character typed.
                index += 1;
                start += width + gap;
            }
            baseline += self.line_height;
        }
    }

    /// A letter in the text or typed colour, rasterized on first use at
    /// the buffer's size.
    fn glyph(&mut self, letter: char, typed: bool) -> &Glyph {
        let color = if typed {
            self.style.dimmed
        } else {
            self.style.text
        };
        let (font, size) = (&self.font, self.style.size * self.scale);
        // `entry().or_insert_with` looks the key up once and runs the closure
        // only when it's missing: the cache in one line.
        self.glyphs.entry((letter, typed)).or_insert_with(|| {
            let (metrics, coverage) = font.rasterize(letter, size);
            Glyph {
                metrics,
                pixmap: tint(&coverage, metrics.width, metrics.height, color),
            }
        })
    }
}

/// Where a tag of `size` goes with its `position` point on `at`, nudged to
/// stay `EDGE` px inside an area of `area`. `None` only if the area is too
/// small to hold it.
#[allow(clippy::cast_precision_loss)] // screen coordinates are far below 2^24
fn place(
    at: Point,
    position: Position,
    (w, h): (f32, f32),
    (area_w, area_h): (f32, f32),
) -> Option<Rect> {
    let (across, down) = position.halves();
    let x = (at.x as f32 - w * across as f32 / 2.0)
        .round()
        .clamp(EDGE, (area_w - w - EDGE).max(EDGE));
    let y = (at.y as f32 - h * down as f32 / 2.0)
        .round()
        .clamp(EDGE, (area_h - h - EDGE).max(EDGE));
    Rect::from_xywh(x, y, w, h)
}

/// A box filled and bordered in `look`'s colours: the fill, then the border
/// stroked half a width inside, so the whole line stays within the box.
/// Its padding is the caller's business. `rect` is in logical px, which
/// `transform` takes to the buffer's.
fn paint_box(pixmap: &mut Pixmap, rect: Rect, look: &Mark, transform: Transform) {
    let inset = look.border_width / 2.0;
    if let Some(path) = rounded_rect(rect, look.radius) {
        pixmap.fill_path(
            &path,
            &paint(look.background),
            FillRule::Winding,
            transform,
            None,
        );
    }
    if look.border_width > 0.0
        && let Some(inner) = Rect::from_ltrb(
            rect.left() + inset,
            rect.top() + inset,
            rect.right() - inset,
            rect.bottom() - inset,
        )
        && let Some(path) = rounded_rect(inner, (look.radius - inset).max(0.0))
    {
        let stroke = Stroke {
            width: look.border_width,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint(look.border), &stroke, transform, None);
    }
}

/// A point as tiny-skia coordinates: the centre of its pixel, not the
/// corner.
#[allow(clippy::cast_precision_loss)] // screen coordinates are far below 2^24
fn centre(at: Point) -> (f32, f32) {
    (at.x as f32 + 0.5, at.y as f32 + 0.5)
}

/// The closest point on a rounded tag's outline. A dot inside the tag
/// needs no line, which would run through its background and letters.
fn nearest_edge(rect: Rect, radius: f32, (x, y): (f32, f32)) -> Option<(f32, f32)> {
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let inner_x = x.clamp(rect.left() + radius, rect.right() - radius);
    let inner_y = y.clamp(rect.top() + radius, rect.bottom() - radius);
    let (dx, dy) = (x - inner_x, y - inner_y);
    let distance = dx.hypot(dy);
    if distance <= radius {
        return None;
    }
    Some((
        inner_x + dx * radius / distance,
        inner_y + dy * radius / distance,
    ))
}

/// A rectangle with rounded corners, as a path. Each corner is a cubic
/// Bézier curve; `KAPPA` is the control-point distance that makes a cubic
/// the closest match to a quarter circle.
fn rounded_rect(rect: Rect, radius: f32) -> Option<Path> {
    const KAPPA: f32 = 0.552_284_8;
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let (left, top, right, bottom) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    // How far each control point sits from the corner itself.
    let pull = radius * (1.0 - KAPPA);
    let mut path = PathBuilder::new();
    path.move_to(left + radius, top);
    path.line_to(right - radius, top);
    path.cubic_to(right - pull, top, right, top + pull, right, top + radius);
    path.line_to(right, bottom - radius);
    path.cubic_to(
        right,
        bottom - pull,
        right - pull,
        bottom,
        right - radius,
        bottom,
    );
    path.line_to(left + radius, bottom);
    path.cubic_to(
        left + pull,
        bottom,
        left,
        bottom - pull,
        left,
        bottom - radius,
    );
    path.line_to(left, top + radius);
    path.cubic_to(left, top + pull, left + pull, top, left + radius, top);
    path.close();
    path.finish()
}

/// One part of a leader: `path` stroked `width` px wide, or filled when
/// `width` is `None`.
struct Part {
    path: Path,
    width: Option<f32>,
    color: Color,
}

/// Paints `parts` on a layer of their own, each replacing what's under it,
/// then lays the layer over `pixmap`. A translucent line or dot then shows
/// the window behind it, not the outline it sits in. The parts are in
/// logical px, drawn at `scale` buffer px each.
fn paint_layer(pixmap: &mut Pixmap, parts: &[Part], scale: f32) {
    let corners: Vec<_> = parts
        .iter()
        .flat_map(|part| {
            // Half the stroke, and a buffer pixel for the anti-aliased edge.
            let grow = part.width.unwrap_or(0.0) / 2.0 + 1.0 / scale;
            let b = part.path.bounds();
            [
                tiny_skia::Point::from_xy((b.left() - grow) * scale, (b.top() - grow) * scale),
                tiny_skia::Point::from_xy((b.right() + grow) * scale, (b.bottom() + grow) * scale),
            ]
        })
        .collect();
    let Some(area) = Rect::from_points(&corners)
        .and_then(|r| r.round_out())
        .zip(IntRect::from_xywh(0, 0, pixmap.width(), pixmap.height()))
        .and_then(|(r, screen)| r.intersect(&screen))
    else {
        return;
    };
    let Some(mut layer) = Pixmap::new(area.width(), area.height()) else {
        return;
    };
    #[allow(clippy::cast_precision_loss)] // monitor sizes are far below 2^24
    let shift =
        Transform::from_scale(scale, scale).post_translate(-area.left() as f32, -area.top() as f32);
    for part in parts {
        let mut paint = paint(part.color);
        paint.blend_mode = BlendMode::Source;
        match part.width {
            Some(width) => {
                let stroke = Stroke {
                    width,
                    ..Stroke::default()
                };
                layer.stroke_path(&part.path, &paint, &stroke, shift, None);
            }
            None => layer.fill_path(&part.path, &paint, FillRule::Winding, shift, None),
        }
    }
    pixmap.draw_pixmap(
        area.left(),
        area.top(),
        layer.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

fn paint(color: Color) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint
}

/// A glyph's coverage map as a pixmap in `color`. Pixmaps store
/// premultiplied colour (each channel already multiplied by alpha), which is
/// what makes blending a single multiply-add.
fn tint(coverage: &[u8], width: usize, height: usize, color: Color) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(u32::try_from(width).ok()?, u32::try_from(height).ok()?)?;
    let scale = |channel: u8, by: u8| {
        u8::try_from(u16::from(channel) * u16::from(by) / 255).unwrap_or(u8::MAX)
    };
    for (pixel, &cover) in pixmap.pixels_mut().iter_mut().zip(coverage) {
        let alpha = scale(color.a, cover);
        let premultiplied = PremultipliedColorU8::from_rgba(
            scale(color.r, alpha),
            scale(color.g, alpha),
            scale(color.b, alpha),
            alpha,
        );
        *pixel = premultiplied.unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
    Some(pixmap)
}

/// Copies a pixmap into a `wl_shm` buffer. tiny-skia stores bytes as R, G,
/// B, A; Wayland's ARGB8888 is a little-endian 32-bit number, which in
/// memory is B, G, R, A. Both are premultiplied, so only red and blue swap.
pub fn to_argb8888(pixmap: &Pixmap, out: &mut [u8]) {
    // `as_chunks::<4>` views the bytes as `[u8; 4]` arrays, one per pixel,
    // so each can be taken apart by name; the second half of the pair is
    // the leftover bytes, none here.
    let (pixels, _) = pixmap.data().as_chunks::<4>();
    let (slots, _) = out.as_chunks_mut::<4>();
    for (&[r, g, b, a], slot) in pixels.iter().zip(slots) {
        *slot = [b, g, r, a];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_has_its_own_font_colours_and_box_above_the_marks() {
        let config = crate::config::Config::parse(
            r##"
[style]
size = 8
text = "#ff0000"
[text.matches]
background = "#0000ff"
padding = [0, 0]
radius = 0
[text.matches.current]
background = "#ff00ff"
[text.input]
font = "serif"
size = 28
text = "#00ff00"
dimmed = "#00ffff"
background = "#111111"
border = "#ffff00"
border_width = 2
radius = 0
padding = [8, 12]
position = "top-left"
offset = [20, 20]
"##,
            &crate::keys::Layout::named("us", ""),
        )
        .unwrap();
        let mut painter = Painter::new(
            &config.style,
            &config.marks,
            &config.input.style,
            &config.info.style,
            1.0,
        )
        .unwrap();
        let mut pixmap = Pixmap::new(300, 160).unwrap();
        let tag = Note {
            text: "AB".into(),
            typed: 1,
        };
        assert!(painter.input.size("AB").0 > painter.tags.size("AB").0);
        let mut frame = Frame {
            highlights: vec![
                Highlight {
                    rect: BoxRect::new(0, 0, 300, 160),
                    current: false,
                },
                Highlight {
                    rect: BoxRect::new(200, 100, 50, 20),
                    current: true,
                },
            ],
            input: Some(tag),
            ..Frame::default()
        };
        painter.paint(&mut pixmap, &frame);
        let rgb = |map: &Pixmap, x, y| {
            let p = map.pixel(x, y).unwrap();
            (p.red(), p.green(), p.blue())
        };
        assert_eq!(rgb(&pixmap, 290, 150), (0, 0, 255), "ordinary mark");
        assert_eq!(rgb(&pixmap, 220, 110), (255, 0, 255), "selected mark");
        assert_eq!(rgb(&pixmap, 20, 20), (255, 255, 0), "input border");
        assert_eq!(rgb(&pixmap, 24, 24), (17, 17, 17), "input background");
        assert!(
            pixmap
                .pixels()
                .iter()
                .any(|p| p.green() > 200 && p.red() < 30 && p.blue() < 30),
            "input text"
        );
        assert!(
            pixmap
                .pixels()
                .iter()
                .any(|p| p.green() > 200 && p.blue() > 200 && p.red() < 30),
            "dimmed verb colour"
        );
        frame.input = None;
        painter.paint(&mut pixmap, &frame);
        assert_eq!(
            rgb(&pixmap, 24, 24),
            (0, 0, 255),
            "no query left when returning to hints"
        );
    }

    #[test]
    fn a_box_sits_at_its_position_and_signed_offset_as_its_text_grows() {
        let style = Style::default();
        let mut pixmap = Pixmap::new(800, 600).unwrap();
        for position in [
            Position::TopLeft,
            Position::Top,
            Position::TopRight,
            Position::Left,
            Position::Center,
            Position::Right,
            Position::BottomLeft,
            Position::Bottom,
            Position::BottomRight,
        ] {
            let (across, down) = position.halves();
            let offset = Point::new(
                if across == 2 { -12 } else { 12 },
                if down == 2 { -16 } else { 16 },
            );
            let input = Style {
                radius: 0.0,
                background: Color::rgba(30, 30, 30, 255),
                position,
                offset: [offset.x, offset.y],
                ..style.clone()
            };
            let mut painter =
                Painter::new(&style, &Marks::new(&style), &input, &style, 1.0).unwrap();
            for text in ["a", "a longer query  1/2"] {
                let frame = Frame {
                    input: Some(Note::new(text)),
                    ..Frame::default()
                };
                painter.paint(&mut pixmap, &frame);
                let ink: Vec<_> = pixmap
                    .pixels()
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.alpha() > 0)
                    .map(|(i, _)| (i % 800, i / 800))
                    .collect();
                let left = i32::try_from(ink.iter().map(|p| p.0).min().unwrap()).unwrap();
                let right = i32::try_from(ink.iter().map(|p| p.0).max().unwrap()).unwrap() + 1;
                let top = i32::try_from(ink.iter().map(|p| p.1).min().unwrap()).unwrap();
                let bottom = i32::try_from(ink.iter().map(|p| p.1).max().unwrap()).unwrap() + 1;
                let near = |got: i32, want: i32| {
                    assert!((got - want).abs() <= 1, "{position:?}: {got} != {want}");
                };
                near(
                    match across {
                        0 => left,
                        1 => left.midpoint(right),
                        _ => right,
                    },
                    400 * across + offset.x,
                );
                near(
                    match down {
                        0 => top,
                        1 => top.midpoint(bottom),
                        _ => bottom,
                    },
                    300 * down + offset.y,
                );
            }
        }
    }

    /// The smallest box around the pixels `keep` picks, as left, top,
    /// right, bottom; right and bottom one past the last.
    fn bounds(pixmap: &Pixmap, keep: impl Fn(PremultipliedColorU8) -> bool) -> [i32; 4] {
        let width = pixmap.width() as usize;
        let mut out = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for (i, &p) in pixmap.pixels().iter().enumerate() {
            if keep(p) {
                let (x, y) = (
                    i32::try_from(i % width).unwrap(),
                    i32::try_from(i / width).unwrap(),
                );
                out = [
                    out[0].min(x),
                    out[1].min(y),
                    out[2].max(x + 1),
                    out[3].max(y + 1),
                ];
            }
        }
        out
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn a_scaled_monitor_shows_the_same_picture_bigger() {
        let style = Style {
            text: Color::rgba(240, 20, 30, 255),
            background: Color::rgba(30, 30, 30, 255),
            radius: 0.0,
            border_width: 0.0,
            padding: [4.0, 6.0],
            ..Style::default()
        };
        let mut marks = Marks::new(&style);
        marks.all.background = Color::rgba(0, 0, 255, 255);
        marks.all.border_width = 0.0;
        marks.all.radius = 0.0;
        marks.all.padding = [0.0, 0.0];
        let frame = Frame {
            tags: vec![Tag {
                at: Point::new(20, 20),
                position: Position::TopLeft,
                text: "AB".into(),
                typed: 0,
                target: None,
            }],
            highlights: vec![Highlight {
                rect: BoxRect::new(20, 100, 40, 20),
                current: false,
            }],
            dots: vec![Dot {
                from: Point::new(150, 20),
                at: Point::new(150, 100),
                tag: None,
            }],
            ..Frame::default()
        };
        let render = |scale: f32| {
            let mut painter = Painter::new(&style, &marks, &style, &style, scale).unwrap();
            let mut pixmap = Pixmap::new((200.0 * scale) as u32, (150.0 * scale) as u32).unwrap();
            painter.paint(&mut pixmap, &frame);
            pixmap
        };
        let (one, big) = (render(1.0), render(1.5));
        let rgba = |p: PremultipliedColorU8| (p.red(), p.green(), p.blue(), p.alpha());
        let colour = |rgb: (u8, u8, u8)| {
            move |p: PremultipliedColorU8| rgba(p) == (rgb.0, rgb.1, rgb.2, 255)
        };

        let mark = |m: &Pixmap| bounds(m, colour((0, 0, 255)));
        assert_eq!(mark(&one), [20, 100, 60, 120]);
        assert_eq!(mark(&big), [30, 150, 90, 180], "logical px times 1.5");

        let tag = |m: &Pixmap| bounds(m, colour((30, 30, 30)));
        for (small, large) in tag(&one).into_iter().zip(tag(&big)) {
            assert!(
                (small as f32 * 1.5 - large as f32).abs() <= 1.0,
                "{small} {large}"
            );
        }
        // The letters are rasterized at 1.5 times their size: as tall again
        // and a half, not stretched pixels.
        let letters = |m: &Pixmap| {
            let [left, top, right, bottom] = tag(m);
            let ink = bounds(m, |p| p.red() > 120 && p.green() < 60);
            assert!(ink[0] >= left && ink[2] <= right && ink[1] >= top && ink[3] <= bottom);
            (ink[3] - ink[1]) as f32
        };
        assert!((letters(&one) * 1.5 - letters(&big)).abs() <= 2.0);

        // The line and the dot land on the same spots, scaled.
        for ((x, y), what) in [((150, 60), "the line"), ((150, 100), "the dot")] {
            let small = rgba(one.pixel(x, y).unwrap());
            let large = rgba(big.pixel(x * 3 / 2, y * 3 / 2).unwrap());
            assert_eq!(small.3, 255, "{what}");
            assert_eq!(small, large, "{what}");
        }
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn a_fading_box_is_the_whole_box_at_part_opacity() {
        let style = Style {
            text: Color::rgba(240, 20, 30, 255),
            background: Color::rgba(30, 90, 30, 255),
            border: Color::rgba(200, 200, 0, 255),
            border_width: 2.0,
            position: Position::Center,
            ..Style::default()
        };
        for scale in [1.0, 1.5] {
            let mut painter =
                Painter::new(&style, &Marks::new(&style), &style, &style, scale).unwrap();
            let size = ((300.0 * scale) as u32, (100.0 * scale) as u32);
            let note = Note {
                text: "fade me".into(),
                typed: 2,
            };
            let mut at = |opacity| {
                let mut pixmap = Pixmap::new(size.0, size.1).unwrap();
                painter.paint_note(&mut pixmap, Panel::Info, &note, opacity);
                pixmap
            };
            let (full, half, gone) = (at(1.0), at(0.5), at(0.0));
            assert!(gone.pixels().iter().all(|p| p.alpha() == 0), "opacity 0");
            assert!(full.pixels().iter().any(|p| p.alpha() == 255));
            // Each pixel is half the opaque box's: the letters cover the
            // background, so none of it shows through them.
            for (f, h) in full.pixels().iter().zip(half.pixels()) {
                for (f, h) in [
                    (f.red(), h.red()),
                    (f.green(), h.green()),
                    (f.blue(), h.blue()),
                    (f.alpha(), h.alpha()),
                ] {
                    assert!(
                        (i16::from(f) - 2 * i16::from(h)).abs() <= 2,
                        "scale {scale}: {f} at full is {h} at half"
                    );
                }
            }
        }
    }

    #[test]
    fn the_legend_lines_up_its_columns_across_lines() {
        let style = Style {
            text: Color::rgba(240, 20, 30, 255),
            background: Color::rgba(0, 0, 0, 0),
            border_width: 0.0,
            position: Position::TopLeft,
            offset: [10, 10],
            ..Style::default()
        };
        let mut painter = Painter::new(&style, &Marks::new(&style), &style, &style, 1.0).unwrap();
        let legend = "a\tx\nlonger\tx";
        let (_, one_line) = painter.info.size("a");
        let (_, two_lines) = painter.info.size(legend);
        assert!(two_lines > one_line + painter.info.line_height - 1.0);
        let mut pixmap = Pixmap::new(400, 200).unwrap();
        painter.paint_note(&mut pixmap, Panel::Info, &Note::new(legend), 1.0);
        // Each line ends in the same letter, in the same column: the same
        // right edge, though "a" is far shorter than "longer".
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let split = (10.0 + style.padding[0] + painter.info.line_height).round() as u32;
        let right = |rows: std::ops::Range<u32>| {
            let mut band = Pixmap::new(400, rows.end - rows.start).unwrap();
            band.draw_pixmap(
                0,
                -i32::try_from(rows.start).unwrap(),
                pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
            bounds(&band, |p| p.alpha() > 128)[2]
        };
        assert_eq!(right(0..split), right(split..200));
    }

    #[test]
    fn tags_put_their_position_on_their_point() {
        let at = |position| {
            let r = place(Point::new(100, 50), position, (20.0, 10.0), (800.0, 600.0)).unwrap();
            (r.x(), r.y())
        };
        assert_eq!(at(Position::Center), (90.0, 45.0));
        assert_eq!(at(Position::TopLeft), (100.0, 50.0));
        assert_eq!(at(Position::Bottom), (90.0, 40.0));
        assert_eq!(at(Position::BottomRight), (80.0, 40.0));
    }

    #[test]
    fn tags_are_nudged_inside_the_edges() {
        let place = |at| place(at, Position::Center, (20.0, 10.0), (800.0, 600.0)).unwrap();
        let top_left = place(Point::new(0, 0));
        assert_eq!((top_left.x(), top_left.y()), (EDGE, EDGE));
        let bottom_right = place(Point::new(800, 600));
        assert_eq!(
            (bottom_right.right(), bottom_right.bottom()),
            (800.0 - EDGE, 600.0 - EDGE)
        );
    }

    #[test]
    fn connection_follows_the_closest_edge_and_rounded_corner() {
        let rect = Rect::from_xywh(20.0, 30.0, 40.0, 20.0).unwrap();
        for (dot, edge) in [
            ((5.0, 38.0), (20.0, 38.0)),
            ((75.0, 42.0), (60.0, 42.0)),
            ((35.0, 5.0), (35.0, 30.0)),
            ((45.0, 75.0), (45.0, 50.0)),
        ] {
            assert_eq!(nearest_edge(rect, 4.0, dot), Some(edge));
        }
        let corner = nearest_edge(rect, 4.0, (12.0, 22.0)).unwrap();
        let expected = 4.0 / 2.0_f32.sqrt();
        assert!((corner.0 - (24.0 - expected)).abs() < 0.001);
        assert!((corner.1 - (34.0 - expected)).abs() < 0.001);
        assert_eq!(nearest_edge(rect, 4.0, (35.0, 40.0)), None);
        assert_eq!(nearest_edge(rect, 0.0, (20.0, 30.0)), None);
        assert_eq!(nearest_edge(rect, 0.0, (5.0, 5.0)), Some((20.0, 30.0)));
        assert_eq!(nearest_edge(rect, 100.0, (40.0, 5.0)), Some((40.0, 30.0)));
    }

    #[test]
    fn rendered_line_moves_between_edges_and_uses_the_clamped_tag_box() {
        let style = Style {
            padding: [8.0, 20.0],
            text: Color::rgba(240, 20, 30, 255),
            background: Color::rgba(30, 30, 30, 128),
            radius: 6.0,
            ..Style::default()
        };
        let mut painter = Painter::new(&style, &Marks::new(&style), &style, &style, 1.0).unwrap();
        let mut pixmap = Pixmap::new(240, 200).unwrap();
        let mut frame = Frame {
            tags: vec![Tag {
                at: Point::new(100, 80),
                position: Position::TopLeft,
                text: "AA".into(),
                typed: 0,
                target: None,
            }],
            dots: vec![Dot {
                from: Point::new(100, 80),
                at: Point::default(),
                tag: Some(0),
            }],
            highlights: vec![],
            input: None,
            info: None,
        };
        for (dot, (x, y)) in [
            (Point::new(60, 90), (90, 90)),
            (Point::new(110, 40), (110, 70)),
            (Point::new(210, 90), (190, 90)),
            (Point::new(110, 160), (110, 140)),
        ] {
            frame.dots[0].at = dot;
            painter.paint(&mut pixmap, &frame);
            let pixel = pixmap.pixel(x, y).unwrap();
            assert_eq!(
                (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
                (240, 20, 30, 255),
                "dot at {dot:?}: the closest edge gives a straight connection"
            );
        }

        // The tag is clamped to x=2; its stored anchor stays offscreen.
        frame.tags[0].at = Point::new(-100, 80);
        frame.dots[0].from = frame.tags[0].at;
        frame.dots[0].at = Point::new(25, 160);
        painter.paint(&mut pixmap, &frame);
        assert_eq!(pixmap.pixel(25, 140).unwrap().red(), 240);

        // Space has no tag even if the pointer shares a tag's anchor.
        frame.tags[0].at = Point::new(100, 80);
        frame.dots[0] = Dot {
            from: Point::new(100, 80),
            at: Point::new(100, 160),
            tag: None,
        };
        painter.paint(&mut pixmap, &frame);
        assert_eq!(pixmap.pixel(100, 140).unwrap().red(), 240);
    }

    #[test]
    fn a_translucent_leader_shows_the_window_not_its_outline() {
        let mut style = Style::default();
        style.leader.line = Some(Color::rgba(255, 0, 0, 128));
        style.leader.dot = Some(Color::rgba(0, 255, 0, 128));
        style.leader.outline = Some(Color::rgba(0, 0, 255, 255));
        let mut painter = Painter::new(&style, &Marks::new(&style), &style, &style, 1.0).unwrap();
        let mut pixmap = Pixmap::new(200, 200).unwrap();
        let frame = Frame {
            dots: vec![Dot {
                from: Point::new(100, 20),
                at: Point::new(100, 160),
                tag: None,
            }],
            ..Frame::default()
        };
        painter.paint(&mut pixmap, &frame);
        let rgba = |x, y| {
            let p = pixmap.pixel(x, y).unwrap();
            (p.red(), p.green(), p.blue(), p.alpha())
        };
        // Premultiplied: half of full red is 128 red at 128 alpha.
        assert_eq!(rgba(100, 90), (128, 0, 0, 128), "the line");
        assert_eq!(rgba(100, 160), (0, 128, 0, 128), "the dot");
        assert_eq!(rgba(100, 158), (0, 128, 0, 128), "no line under the dot");
        assert_eq!(rgba(101, 90).2, 255, "the outline beside the line");
    }

    #[test]
    fn argb_swaps_red_and_blue() {
        let mut pixmap = Pixmap::new(1, 1).unwrap();
        pixmap.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let mut out = [0u8; 4];
        to_argb8888(&pixmap, &mut out);
        assert_eq!(out, [0, 0, 255, 255], "B, G, R, A");
    }

    #[test]
    fn coverage_becomes_premultiplied_colour() {
        let half = tint(&[128], 1, 1, Color::rgba(255, 255, 255, 255)).unwrap();
        let p = half.pixels()[0];
        assert_eq!((p.red(), p.alpha()), (128, 128));
    }

    #[test]
    fn rounded_rect_never_exceeds_half_the_box() {
        let rect = Rect::from_xywh(0.0, 0.0, 10.0, 4.0).unwrap();
        let path = rounded_rect(rect, 50.0).unwrap();
        let bounds = path.bounds();
        assert!(bounds.width() <= 10.0 && bounds.height() <= 4.0);
    }
}

#[cfg(test)]
mod collision_tests {
    use super::*;
    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn production_painter_separates_sidebar_row_and_close() {
        let style = Style {
            size: 11.0,
            offset: [3, 3],
            ..Style::default()
        };
        let mut painter = Painter::new(&style, &Marks::new(&style), &style, &style, 1.0).unwrap();
        let mut pixmap = Pixmap::new(300, 600).unwrap();
        painter.set_space(
            BoxRect::new(0, 0, 300, 600),
            &[
                BoxRect::new(30, 180, 160, 15),
                BoxRect::new(246, 180, 26, 26),
            ],
            vec![],
        );
        let frame = Frame {
            tags: [
                (Point::new(276, 211), Point::new(148, 193), "EA"),
                (Point::new(275, 209), Point::new(259, 193), "ET"),
            ]
            .into_iter()
            .map(|(at, hit, text)| Tag {
                at,
                target: Some(hit),
                position: Position::BottomRight,
                text: text.into(),
                typed: 0,
            })
            .collect(),
            dots: vec![],
            highlights: vec![],
            input: None,
            info: None,
        };
        painter.paint(&mut pixmap, &frame);
        let arranged = painter.arrange(&frame, (300.0, 600.0));
        let boxes: Vec<_> = arranged
            .tags
            .iter()
            .map(|t| place(t.at, t.position, painter.tags.size(&t.text), (300.0, 600.0)).unwrap())
            .collect();
        assert!(
            boxes[0].intersect(&boxes[1]).is_none(),
            "sidebar row and close must have readable labels"
        );
        assert_eq!(arranged.dots.len(), 1);
        assert_eq!(arranged.dots[0].at, Point::new(259, 193));
        let moved = BoxRect::new(
            boxes[1].x() as i32,
            boxes[1].y() as i32,
            boxes[1].width() as i32,
            boxes[1].height() as i32,
        );
        assert_eq!(painter.space.as_ref().unwrap().ink(moved), 0);
        let one = Frame {
            tags: vec![frame.tags[1].clone()],
            ..Frame::default()
        };
        let filtered = painter.arrange(&one, (300.0, 600.0));
        assert_eq!(
            filtered.tags[0].at, arranged.tags[1].at,
            "typing must keep displaced labels stable"
        );
        // A verb typed in front of the letters: the displaced tag stays off
        // the row it would cover, and keeps its leader; the other gets none.
        let with_verb = |tag: &Tag| Frame {
            tags: vec![Tag {
                text: format!("r{}", tag.text),
                typed: 2,
                ..tag.clone()
            }],
            ..Frame::default()
        };
        let moved = painter.arrange(&with_verb(&frame.tags[1]), (300.0, 600.0));
        assert_eq!(moved.tags[0].at.y, arranged.tags[1].at.y);
        assert_eq!(moved.dots.len(), 1, "still pointing back at its target");
        let kept = painter.arrange(&with_verb(&frame.tags[0]), (300.0, 600.0));
        assert_eq!(kept.dots, [], "a tag never moved needs no leader");
        let nudged = Frame {
            dots: vec![Dot {
                from: frame.tags[1].at,
                at: Point::new(269, 203),
                tag: Some(1),
            }],
            ..frame.clone()
        };
        let nudged = painter.arrange(&nudged, (300.0, 600.0));
        assert_eq!(nudged.dots.len(), 1);
        assert_eq!(nudged.dots[0].at, Point::new(269, 203));
    }
}

#[cfg(test)]
mod capture_replay {
    use super::*;
    use std::{path::Path, time::Instant};

    /// Text mode's marks over a saved picture, for looking at: the boxes
    /// in `WAYHINT_MARKS_RECTS` (`x,y,w,h;...`, the first one current) over
    /// the PNG `WAYHINT_MARKS_IMAGE`, in the marks of `WAYHINT_STYLE_FILE`
    /// if given, saved to `WAYHINT_MARKS_OUTPUT`.
    #[test]
    #[ignore = "requires a picture, boxes and an output path"]
    fn render_text_marks() {
        let var = |name| std::env::var(name).unwrap();
        let mut picture = Pixmap::load_png(var("WAYHINT_MARKS_IMAGE")).unwrap();
        let config = std::env::var("WAYHINT_STYLE_FILE")
            .ok()
            .map_or_else(String::new, |p| std::fs::read_to_string(p).unwrap());
        let config =
            crate::config::Config::parse(&config, &crate::keys::Layout::named("us", "")).unwrap();
        let number = |n: &str| n.trim().parse::<i32>().unwrap();
        let highlights = var("WAYHINT_MARKS_RECTS")
            .split(';')
            .enumerate()
            .map(|(i, rect)| {
                let v: Vec<i32> = rect.split(',').map(number).collect();
                Highlight {
                    rect: BoxRect::new(v[0], v[1], v[2], v[3]),
                    current: i == 0,
                }
            })
            .collect();
        let frame = Frame {
            highlights,
            ..Frame::default()
        };
        let mut painter = Painter::new(
            &config.style,
            &config.marks,
            &config.input.style,
            &config.info.style,
            1.0,
        )
        .unwrap();
        let mut marks = Pixmap::new(picture.width(), picture.height()).unwrap();
        painter.paint(&mut marks, &frame);
        picture.draw_pixmap(
            0,
            0,
            marks.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        picture.save_png(var("WAYHINT_MARKS_OUTPUT")).unwrap();
    }

    /// Render a captured snapshot with the real font, style and placement.
    #[test]
    #[ignore = "requires a saved snapshot and output path"]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::too_many_lines
    )]
    fn render_layout_capture() {
        let snapshot = std::env::var("WAYHINT_LAYOUT_SNAPSHOT").unwrap();
        let output = std::env::var("WAYHINT_LAYOUT_OUTPUT").unwrap();
        let snapshot = crate::snapshot::Snapshot::load(Path::new(&snapshot)).unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        let config = std::env::var("WAYHINT_STYLE_FILE")
            .ok()
            .map_or_else(String::new, |p| std::fs::read_to_string(p).unwrap());
        let config =
            crate::config::Config::parse(&config, &crate::keys::Layout::named("us", "")).unwrap();
        let window = &snapshot.windows[0].window;
        let origin = window.rect.origin();
        let shift = Point::new(-origin.x, -origin.y);
        let area = window.rect.offset(shift);
        let selector = crate::selection::Selector::new(
            snapshot.targets.len(),
            config.alphabet.len(),
            config.verbs.default_action(),
        );
        let tags = snapshot
            .targets
            .iter()
            .zip(selector.labels())
            .map(|(target, label)| Tag {
                at: config
                    .style
                    .position
                    .on(target.shown())
                    .offset(Point::new(config.style.offset[0], config.style.offset[1]))
                    .offset(shift),
                position: config.style.position,
                target: Some(target.hit.offset(shift)),
                typed: 0,
                text: label
                    .iter()
                    .map(|&i| config.alphabet.letters()[i].label.to_ascii_uppercase())
                    .collect(),
            })
            .collect();
        let frame = Frame {
            tags,
            ..Frame::default()
        };
        let mut painter = Painter::new(
            &config.style,
            &config.marks,
            &config.input.style,
            &config.info.style,
            1.0,
        )
        .unwrap();
        let obstacles: Vec<_> = snapshot.obstacles.iter().map(|r| r.offset(shift)).collect();
        painter.set_space(area, &obstacles, vec![area]);
        let size = (area.w as f32, area.h as f32);
        let boxes = |p: &Painter, f: &Frame| {
            f.tags
                .iter()
                .map(|t| {
                    let r = place(t.at, t.position, p.tags.size(&t.text), size).unwrap();
                    BoxRect::new(
                        r.x() as i32,
                        r.y() as i32,
                        r.width() as i32,
                        r.height() as i32,
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = boxes(&painter, &frame);
        let started = Instant::now();
        let placed = painter.arrange(&frame, size);
        let layout_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = boxes(&painter, &placed);
        let overlaps = |rs: &[BoxRect]| {
            rs.iter()
                .enumerate()
                .map(|(i, a)| {
                    rs[i + 1..]
                        .iter()
                        .filter(|b| a.intersect(**b).is_some())
                        .count()
                })
                .sum::<usize>()
        };
        let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        let covered = before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a != b && painter.space.as_ref().unwrap().ink(**b) != 0)
            .count();
        assert_eq!(covered, 0, "a moved label covers captured content");
        let mut pixmap = Pixmap::new(area.w as u32, area.h as u32).unwrap();
        painter.paint(&mut pixmap, &frame);
        pixmap.save_png(&output).unwrap();
        let data = serde_json::json!({"labels":after,"original":before,"targets":frame.tags.iter().map(|t|t.target).collect::<Vec<_>>(),
            "overlaps_before":overlaps(&before),"overlaps_after":overlaps(&after),"moved":moved,"moved_over_ink":covered,"layout_ms":layout_ms});
        std::fs::write(
            format!("{output}.json"),
            serde_json::to_vec_pretty(&data).unwrap(),
        )
        .unwrap();
        eprintln!(
            "targets={} overlaps={}->{} moved={moved} ink_hits={covered} layout={layout_ms:.2}ms",
            frame.tags.len(),
            overlaps(&before),
            overlaps(&after)
        );
    }
}
