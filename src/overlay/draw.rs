//! Drawing tags into a pixel buffer, in software.
//!
//! `tiny-skia` paints the boxes (anti-aliased rounded rectangles) into a
//! `Pixmap`; `fontdue` rasterizes each letter into a coverage map (how much
//! of each pixel the glyph covers, 0-255), which becomes a small coloured
//! pixmap drawn on top. No GPU: a frame is a few dozen small shapes, and a
//! CPU draws that in well under a millisecond. Dots, for where a nudged
//! command lands, are two filled circles, each on a line back to its tag.

use std::collections::HashMap;

use anyhow::Result;
use tiny_skia::{
    FillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint, PremultipliedColorU8, Rect, Stroke,
    Transform,
};

use crate::{
    geometry::{Point, Rect as BoxRect},
    overlay::font,
    placement::{self, Label},
    space::Space,
    style::{Color, Position, Style},
};

/// Gap kept between a tag and the monitor's edge, in px.
const EDGE: f32 = 2.0;
/// A dot's radius, and the width of the ring around it, in px.
const DOT: f32 = 3.0;
const RING: f32 = 2.0;
/// The width of the line from a tag to its dot, in px. Its ring adds
/// `RING` px in all, half on each side.
const LINE: f32 = 1.0;

/// Everything one picture shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub tags: Vec<Tag>,
    /// Where nudged commands land: the tag says which, the dot says where,
    /// and the line between says which dot is whose.
    pub dots: Vec<Dot>,
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
    /// The click point; absent for the status pill. Placement never changes it.
    pub target: Option<Point>,
}

/// A rasterized letter: where it sits relative to the pen, and its pixels.
struct Glyph {
    metrics: fontdue::Metrics,
    /// `None` for glyphs with no ink (a space).
    pixmap: Option<Pixmap>,
}

/// Draws tags in one style. Keeps the font and every letter it has drawn,
/// so each letter is rasterized once per colour per run.
pub struct Painter {
    style: Style,
    font: fontdue::Font,
    /// Distance from a line's top to its baseline, and its full height.
    ascent: f32,
    line_height: f32,
    /// Keyed by letter and whether it's drawn in the typed colour.
    glyphs: HashMap<(char, bool), Glyph>,
    space: Option<Space>,
    regions: Vec<BoxRect>,
    placed: HashMap<(String, i32, i32), BoxRect>,
}

impl Painter {
    pub fn new(style: &Style) -> Result<Self> {
        let font = font::load(&style.font, style.size)?;
        let (ascent, line_height) = font
            .horizontal_line_metrics(style.size)
            .map_or((style.size, style.size * 1.2), |m| {
                (m.ascent, m.ascent - m.descent)
            });
        Ok(Self {
            style: style.clone(),
            font,
            ascent,
            line_height,
            glyphs: HashMap::new(),
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
                let rect = place(tag.at, tag.position, self.size(&tag.text), area).unwrap();
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
                let key = (tag.text.clone(), tag.at.x, tag.at.y);
                let bounds = tag
                    .target
                    .and_then(|p| self.regions.iter().rev().find(|r| r.contains(p)))
                    .and_then(|r| r.intersect(screen))
                    .unwrap_or(screen);
                Label {
                    rect: self.placed.get(&key).copied().unwrap_or(rect),
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
            self.placed
                .insert((tag.text.clone(), tag.at.x, tag.at.y), rect);
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

    /// Clears `pixmap` and draws `frame` on it, in overlay coordinates.
    ///
    /// Lines start at their tags' nearest edges and go under the tags.
    /// Dots go last, over everything.
    pub fn paint(&mut self, pixmap: &mut Pixmap, frame: &Frame) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        #[allow(clippy::cast_precision_loss)] // monitor sizes are far below 2^24
        let area = (pixmap.width() as f32, pixmap.height() as f32);
        let arranged = self.arrange(frame, area);
        let frame = &arranged;
        for &dot in &frame.dots {
            let from = if let Some(tag) = dot.tag.and_then(|i| frame.tags.get(i)) {
                let Some(rect) = place(tag.at, tag.position, self.size(&tag.text), area) else {
                    continue;
                };
                let Some(from) = nearest_edge(rect, self.style.radius, centre(dot.at)) else {
                    continue;
                };
                from
            } else {
                centre(dot.from)
            };
            self.paint_line(pixmap, from, centre(dot.at));
        }
        for tag in &frame.tags {
            self.paint_tag(pixmap, tag, area);
        }
        for &dot in &frame.dots {
            self.paint_dot(pixmap, dot.at);
        }
    }

    /// The line from where a command was nudged from to where it lands, in
    /// the dot's two colours, so it shows on light and dark windows alike.
    fn paint_line(&self, pixmap: &mut Pixmap, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) {
        let mut path = PathBuilder::new();
        path.move_to(x0, y0);
        path.line_to(x1, y1);
        let Some(path) = path.finish() else {
            return;
        };
        for (width, color) in [
            (LINE + RING, self.style.background),
            (LINE, self.style.text),
        ] {
            let stroke = Stroke {
                width,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
        }
    }

    /// A dot in the letters' colour inside a ring in the tags' background:
    /// the pair a tag is already legible in, so the dot shows on light and
    /// dark windows alike.
    fn paint_dot(&self, pixmap: &mut Pixmap, at: Point) {
        let (x, y) = centre(at);
        for (radius, color) in [(DOT + RING, self.style.background), (DOT, self.style.text)] {
            if let Some(circle) = PathBuilder::from_circle(x, y, radius) {
                pixmap.fill_path(
                    &circle,
                    &paint(color),
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }

    /// The tag's box size: its text plus padding on every side.
    fn size(&self, text: &str) -> (f32, f32) {
        let [pad_y, pad_x] = self.style.padding;
        let width: f32 = text
            .chars()
            .map(|c| self.font.metrics(c, self.style.size).advance_width)
            .sum();
        (
            (width + 2.0 * pad_x).ceil(),
            (self.line_height + 2.0 * pad_y).ceil(),
        )
    }

    fn paint_tag(&mut self, pixmap: &mut Pixmap, tag: &Tag, area: (f32, f32)) {
        let size = self.size(&tag.text);
        let Some(rect) = place(tag.at, tag.position, size, area) else {
            return;
        };
        let style = &self.style;

        // The box: fill, then the border stroked half a width inside, so the
        // whole line stays within the box.
        let inset = style.border_width / 2.0;
        if let Some(path) = rounded_rect(rect, style.radius) {
            pixmap.fill_path(
                &path,
                &paint(style.background),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if style.border_width > 0.0
            && let Some(inner) = Rect::from_ltrb(
                rect.left() + inset,
                rect.top() + inset,
                rect.right() - inset,
                rect.bottom() - inset,
            )
            && let Some(path) = rounded_rect(inner, (style.radius - inset).max(0.0))
        {
            let stroke = Stroke {
                width: style.border_width,
                ..Stroke::default()
            };
            pixmap.stroke_path(
                &path,
                &paint(style.border),
                &stroke,
                Transform::identity(),
                None,
            );
        }

        // The letters, on a baseline `ascent` below the padding.
        let [pad_y, pad_x] = style.padding;
        let baseline = rect.top() + pad_y + self.ascent;
        let mut pen = rect.left() + pad_x;
        for (i, letter) in tag.text.chars().enumerate() {
            let glyph = self.glyph(letter, i < tag.typed);
            let m = glyph.metrics;
            if let Some(ink) = &glyph.pixmap {
                // fontdue measures `ymin` up from the baseline to the glyph's
                // bottom edge; pixmaps count down from the top.
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let (x, y) = (
                    (pen + m.xmin as f32).round() as i32,
                    (baseline - m.ymin as f32 - m.height as f32).round() as i32,
                );
                pixmap.draw_pixmap(
                    x,
                    y,
                    ink.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
            pen += m.advance_width;
        }
    }

    /// A letter in the text or typed colour, rasterized on first use.
    fn glyph(&mut self, letter: char, typed: bool) -> &Glyph {
        let color = if typed {
            self.style.matched
        } else {
            self.style.text
        };
        let (font, size) = (&self.font, self.style.size);
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
        let mut painter = Painter::new(&style).unwrap();
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
        let mut painter = Painter::new(&style).unwrap();
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
        };
        painter.paint(&mut pixmap, &frame);
        let arranged = painter.arrange(&frame, (300.0, 600.0));
        let boxes: Vec<_> = arranged
            .tags
            .iter()
            .map(|t| place(t.at, t.position, painter.size(&t.text), (300.0, 600.0)).unwrap())
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
            dots: vec![],
        };
        let filtered = painter.arrange(&one, (300.0, 600.0));
        assert_eq!(
            filtered.tags[0].at, arranged.tags[1].at,
            "typing must keep displaced labels stable"
        );
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

    /// Render a captured snapshot with the real font, style and placement.
    #[test]
    #[ignore = "requires a saved snapshot and output path"]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss
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
        let frame = Frame { tags, dots: vec![] };
        let mut painter = Painter::new(&config.style).unwrap();
        let obstacles: Vec<_> = snapshot.obstacles.iter().map(|r| r.offset(shift)).collect();
        painter.set_space(area, &obstacles, vec![area]);
        let size = (area.w as f32, area.h as f32);
        let boxes = |p: &Painter, f: &Frame| {
            f.tags
                .iter()
                .map(|t| {
                    let r = place(t.at, t.position, p.size(&t.text), size).unwrap();
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
