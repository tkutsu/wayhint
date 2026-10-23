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
    geometry::Point,
    overlay::font,
    style::{Color, Style},
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

/// Where a nudged command lands, `at`, and the point it was nudged from:
/// its tag's hit point, or the pointer for `Space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dot {
    pub from: Point,
    pub at: Point,
}

/// One tag to draw, in overlay coordinates (the monitor's top-left is 0, 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Where the tag's centre goes: the target's hit point.
    pub at: Point,
    pub text: String,
    /// How many leading letters are already typed (drawn dimmed).
    pub typed: usize,
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
        })
    }

    /// Clears `pixmap` and draws `frame` on it, in overlay coordinates.
    ///
    /// Lines go first, under the tags, so a line never crosses the letters:
    /// it comes out from under its own tag. Dots go last, over everything.
    pub fn paint(&mut self, pixmap: &mut Pixmap, frame: &Frame) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        #[allow(clippy::cast_precision_loss)] // monitor sizes are far below 2^24
        let area = (pixmap.width() as f32, pixmap.height() as f32);
        for &dot in &frame.dots {
            self.paint_line(pixmap, dot);
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
    fn paint_line(&self, pixmap: &mut Pixmap, dot: Dot) {
        let ((x0, y0), (x1, y1)) = (centre(dot.from), centre(dot.at));
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
        let Some(rect) = place(tag.at, size, area) else {
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

/// Where a tag of `size` centred on `at` goes, nudged to stay `EDGE` px
/// inside an area of `area`. `None` only if the area is too small to hold it.
#[allow(clippy::cast_precision_loss)] // screen coordinates are far below 2^24
fn place(at: Point, (w, h): (f32, f32), (area_w, area_h): (f32, f32)) -> Option<Rect> {
    let x = (at.x as f32 - w / 2.0)
        .round()
        .clamp(EDGE, (area_w - w - EDGE).max(EDGE));
    let y = (at.y as f32 - h / 2.0)
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
    fn tags_centre_on_their_point() {
        let r = place(Point::new(100, 50), (20.0, 10.0), (800.0, 600.0)).unwrap();
        assert_eq!((r.x(), r.y()), (90.0, 45.0));
    }

    #[test]
    fn tags_are_nudged_inside_the_edges() {
        let top_left = place(Point::new(0, 0), (20.0, 10.0), (800.0, 600.0)).unwrap();
        assert_eq!((top_left.x(), top_left.y()), (EDGE, EDGE));
        let bottom_right = place(Point::new(800, 600), (20.0, 10.0), (800.0, 600.0)).unwrap();
        assert_eq!(
            (bottom_right.right(), bottom_right.bottom()),
            (800.0 - EDGE, 600.0 - EDGE)
        );
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
