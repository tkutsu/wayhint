//! Drawing tags into a pixel buffer, in software.
//!
//! `tiny-skia` paints the boxes (anti-aliased rounded rectangles) into a
//! `Pixmap`; `fontdue` rasterizes each letter into a coverage map (how much
//! of each pixel the glyph covers, 0-255), which becomes a small coloured
//! pixmap drawn on top. No GPU: a frame is a few dozen small shapes, and a
//! CPU draws that in well under a millisecond. Tiny arrows at the tags'
//! edges point toward their targets.
//!
//! Everything is laid out in logical px, as the rest of wayhint counts,
//! and drawn at the monitor's scale: shapes through a scaling transform,
//! letters rasterized at their size times the scale. So a tag is the same
//! size on every monitor, and sharp on a scaled one.
//!
//! The input box holds what's typed, and under it a message: an icon, a
//! glyph from a Nerd Font's symbols, then the words, both in the
//! message's colour.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use tiny_skia::{
    BlendMode, FillRule, GradientStop, IntRect, LinearGradient, Paint, Path, PathBuilder, Pixmap,
    PixmapPaint, PremultipliedColorU8, Rect, SpreadMode, Stroke, Transform,
};

use crate::{
    geometry::{Point, Rect as BoxRect},
    overlay::font,
    placement::{self, Label},
    space::Space,
    status::{Message, Status},
    style::{Color, Direction, Info, Mark, Marks, Position, StartMark, Style},
};

/// Gap kept between a tag and the monitor's edge, in px.
const EDGE: f32 = 2.0;
/// The most lines a message takes; the rest is cut, ending in `…`.
const MESSAGE_LINES: usize = 2;
/// The narrowest a message wraps at, in ems of the box's text: a terse
/// message keeps to one line, however little is typed above it.
const MESSAGE_WIDTH: f32 = 16.0;

/// Everything one picture shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub tags: Vec<Tag>,
    /// Overrides for arrows when a click moves or a tag is displaced.
    pub arrows: Vec<ArrowTarget>,
    /// Text mode's matches, under everything else.
    pub highlights: Vec<Highlight>,
    /// The first letter of each word, in `[words.first_character]`, under the
    /// matches: what typing finds.
    pub starts: Vec<BoxRect>,
    /// The input box: its first line, an armed verb, `drop`, the tag
    /// letters typed, text mode's query; and its second, a message,
    /// wrapped to fit. One with tabs, the key list, is shown whole, its
    /// columns lined up.
    pub status: Status,
}

/// A translucent box over one line of a match in text mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub rect: BoxRect,
    /// Part of the current match, drawn in `[words.matches.current]`.
    pub current: bool,
}

/// An arrow pointing toward `at`, from the tag's edge or `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrowTarget {
    pub from: Point,
    pub at: Point,
    /// The tag whose edge the arrow follows; `None` starts at `from`.
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
    /// Its target's box, for `Position::Auto`: the tag goes by its centre.
    pub control: Option<BoxRect>,
    pub text: String,
    /// How many leading letters are already typed (drawn dimmed).
    pub typed: usize,
    /// How many of those name the armed verb (drawn in its colour).
    pub verb: usize,
    /// The click point; absent for a box. Placement never changes it.
    pub target: Option<Point>,
    /// Distinct accessible objects can share the same box and click point.
    pub object: Option<crate::desktop::tree::ObjectId>,
    pub kind: Kind,
}

type PlacementKey = (Point, Point, Option<crate::desktop::tree::ObjectId>);

impl Tag {
    /// Keep placement tied to its object as labels shorten or gain a verb.
    fn placement_key(&self) -> Option<PlacementKey> {
        self.target.map(|hit| (self.at, hit, self.object.clone()))
    }
}

/// Which look a tag is drawn in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `[hints]`.
    #[default]
    Plain,
    /// Found in its window's pixels: `[hints.pixels]`.
    Pixels,
    /// A window focus hint, placed last without an arrow.
    Window,
}

/// A rasterized letter: where it sits relative to the pen, and its pixels.
struct Glyph {
    metrics: fontdue::Metrics,
    /// `None` for glyphs with no ink (a space).
    pixmap: Option<Pixmap>,
}

/// One line of a box: `text` in `color`, its first `verb` characters in
/// the box's verb colour and the rest of its first `dimmed` in its dimmed
/// colour, starting `indent` px in. A message's lines start past its icon,
/// which its first line has.
struct Row {
    text: String,
    verb: usize,
    dimmed: usize,
    color: Color,
    indent: f32,
    icon: Option<String>,
}

/// Draws tags, matches and the input box, caching each font's rasterized
/// glyphs.
pub struct Painter {
    tags: TextPainter,
    /// Tags found in pixels: `[hints.pixels]`.
    pixels: TextPainter,
    input: TextPainter,
    /// The messages' icons: `[messages] icon_font`, at the input box's size.
    icons: TextPainter,
    marks: Marks,
    /// The messages' icons and colours.
    info: Info,
    /// Buffer px per logical px: the monitor's scale.
    scale: f32,
    space: Option<Space>,
    /// Each target surface and the area its labels may use.
    regions: Vec<(BoxRect, BoxRect)>,
    /// Every target's box: what `Position::Auto` keeps its tags off.
    controls: Vec<BoxRect>,
    /// How far each tag was moved off its own box, keyed by its object,
    /// anchor and click point: a verb typed in front changes the text
    /// and the box's size, never where the tag points.
    placed: HashMap<PlacementKey, Point>,
    /// Which side of its control each `Auto` tag took, with the same key
    /// and for the same reason: a shorter tag must not hop to the left.
    sides: HashMap<PlacementKey, Side>,
    /// Shared placements stay anchored, including after filtering.
    shared: HashSet<PlacementKey>,
}

/// Where a `Position::Auto` tag goes by its control, tried in this order:
/// left, below, above, right, centred along each edge, then centred inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Below,
    Above,
    Right,
    Center,
}

/// The gap between a tag's near edge and its target's edge, in px.
const BESIDE: i32 = 3;

/// What an `Auto` tag must keep off: neighbouring controls, tags placed
/// before it, and anything outside `bounds`, its window on the screen.
struct Around<'a> {
    controls: &'a [BoxRect],
    tags: &'a [BoxRect],
    bounds: BoxRect,
}

impl Side {
    const ORDER: [Self; 5] = [
        Self::Left,
        Self::Below,
        Self::Above,
        Self::Right,
        Self::Center,
    ];

    /// The point by `control` this side's tag sits on, and which of the
    /// tag's own points goes there.
    const fn anchor(self, control: BoxRect) -> (Point, Position) {
        let middle = control.center();
        match self {
            Self::Left => (Point::new(control.x - BESIDE, middle.y), Position::Right),
            Self::Below => (
                Point::new(middle.x, control.bottom() + BESIDE),
                Position::Top,
            ),
            Self::Above => (Point::new(middle.x, control.y - BESIDE), Position::Bottom),
            Self::Right => (
                Point::new(control.right() + BESIDE, middle.y),
                Position::Left,
            ),
            Self::Center => (middle, Position::Center),
        }
    }
}

impl Around<'_> {
    /// Enclosing controls are containers, rather than neighbours to avoid.
    fn clear(&self, rect: BoxRect, control: BoxRect) -> bool {
        rect.intersect(self.bounds) == Some(rect)
            && rect.intersect(control).is_none()
            && self.controls.iter().all(|&other| {
                other.intersect(control) == Some(control) || rect.intersect(other).is_none()
            })
            && !self
                .tags
                .iter()
                .any(|&other| placement::touches(rect, other))
    }
}

/// Keep targets in line order, tolerating small overlaps in adjacent hit boxes.
fn line_members(candidates: &[(usize, BoxRect)], vertical: bool) -> Vec<(usize, BoxRect)> {
    let along = |r: BoxRect| {
        if vertical {
            (r.y, r.bottom())
        } else {
            (r.x, r.right())
        }
    };
    let mut line: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|&(i, r)| {
            !candidates
                .iter()
                .any(|&(j, other)| i != j && r != other && r.intersect(other) == Some(other))
        })
        .collect();
    line.sort_unstable_by_key(|&(i, r)| (along(r), i));
    if line
        .windows(2)
        .any(|pair| along(pair[0].1).1 - along(pair[1].1).0 > 4)
    {
        line.clear();
    }
    line
}

/// Layer surfaces can be thinner than a label; their labels may use the monitor.
pub(super) fn label_region(
    surface: BoxRect,
    kind: crate::desktop::Kind,
    screen: BoxRect,
) -> (BoxRect, BoxRect) {
    let labels = if matches!(kind, crate::desktop::Kind::Layer { .. }) {
        screen
    } else {
        surface
    };
    (surface, labels)
}

impl Painter {
    /// The tags in `style`, text mode's marks in `marks`, the input box in
    /// `input` with `info`'s icons and colours for its messages, drawn at
    /// `scale` buffer px per logical px.
    pub fn new(
        style: &Style,
        marks: &Marks,
        input: &Style,
        info: &Info,
        scale: f32,
    ) -> Result<Self> {
        let icons = Style {
            font: info.icon_font.clone(),
            ..input.clone()
        };
        Ok(Self {
            tags: TextPainter::new(style, scale)?,
            pixels: TextPainter::new(&style.for_pixels(), scale)?,
            input: TextPainter::new(input, scale)?,
            icons: TextPainter::new(&icons, scale)?,
            marks: *marks,
            info: info.clone(),
            scale,
            space: None,
            regions: Vec::new(),
            controls: Vec::new(),
            placed: HashMap::new(),
            sides: HashMap::new(),
            shared: HashSet::new(),
        })
    }

    /// The font and look `tag` is drawn in: `[hints]` or `[hints.pixels]`,
    /// by its kind.
    const fn look(&self, tag: &Tag) -> &TextPainter {
        match tag.kind {
            Kind::Plain | Kind::Window => &self.tags,
            Kind::Pixels => &self.pixels,
        }
    }

    /// A fresh scan invalidates placement; typing keeps the same positions.
    pub fn set_space(
        &mut self,
        area: BoxRect,
        obstacles: &[BoxRect],
        regions: Vec<(BoxRect, BoxRect)>,
        controls: Vec<BoxRect>,
    ) {
        self.space = Some(Space::rectangles(area, obstacles));
        self.regions = regions;
        self.controls = controls;
        self.placed.clear();
        self.sides.clear();
        self.shared.clear();
    }

    /// Rows and columns share an outward side. One obstruction is tolerated;
    /// two make the entire line try the remaining sides in default order.
    #[allow(clippy::cast_precision_loss)]
    fn line_sides(
        &self,
        frame: &Frame,
        bounds: &[BoxRect],
        controls: &[BoxRect],
        area: (f32, f32),
    ) -> Vec<Option<Side>> {
        let eligible: Vec<_> = frame
            .tags
            .iter()
            .enumerate()
            .filter_map(|(i, tag)| {
                (tag.position == Position::Auto)
                    .then_some(tag.control)
                    .flatten()
                    .map(|r| (i, r))
            })
            .collect();
        if eligible.iter().all(|&(i, _)| {
            frame.tags[i]
                .placement_key()
                .is_some_and(|key| self.sides.contains_key(&key))
        }) {
            return vec![None; frame.tags.len()];
        }
        let mut lines = Vec::new();
        // Match starts, centres or ends, allowing four logical pixels of jitter.
        for vertical in [true, false] {
            for alignment in 0..3 {
                let cross = |r: BoxRect| {
                    if vertical {
                        r.x + r.w * alignment / 2
                    } else {
                        r.y + r.h * alignment / 2
                    }
                };
                let mut ordered = eligible.clone();
                ordered.sort_unstable_by_key(|&(i, r)| {
                    let b = bounds[i];
                    (b.x, b.y, b.w, b.h, cross(r), i)
                });
                let mut remaining = ordered.as_slice();
                while let Some(&(first, anchor)) = remaining.first() {
                    let end = remaining.partition_point(|&(i, r)| {
                        bounds[i] == bounds[first] && cross(r) - cross(anchor) <= 4
                    });
                    let line = line_members(&remaining[..end], vertical);
                    remaining = &remaining[end..];
                    if line.len() < 2 {
                        continue;
                    }
                    let centre = line
                        .iter()
                        .map(|&(_, r)| {
                            if vertical {
                                r.center().x as f32 / area.0
                            } else {
                                r.center().y as f32 / area.1
                            }
                        })
                        .sum::<f32>()
                        / line.len() as f32;
                    let preferred = match (vertical, centre <= 0.5) {
                        (true, true) => Side::Left,
                        (true, false) => Side::Right,
                        (false, true) => Side::Above,
                        (false, false) => Side::Below,
                    };
                    lines.push((centre.min(1.0 - centre), line, preferred));
                }
            }
        }
        // Where a grid has both axes, the line nearer a screen edge wins.
        lines.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.len().cmp(&a.1.len())));
        let mut sides = vec![None; frame.tags.len()];
        for (_, line, preferred) in lines {
            if line.iter().any(|&(i, _)| sides[i].is_some()) {
                continue;
            }
            let side = std::iter::once(preferred)
                .chain(Side::ORDER)
                .find(|&side| {
                    side == Side::Center
                        || line
                            .iter()
                            .filter(|&&(i, control)| {
                                let around = Around {
                                    controls,
                                    tags: &[],
                                    bounds: bounds[i],
                                };
                                !around.clear(
                                    self.side_rect(&frame.tags[i], control, side, area),
                                    control,
                                )
                            })
                            .take(2)
                            .count()
                            < 2
                })
                .unwrap();
            for (i, _) in line {
                sides[i] = Some(side);
            }
        }
        sides
    }

    /// The final candidate includes the configured offset before clearance is checked.
    #[allow(clippy::cast_precision_loss)]
    fn side_rect(&self, tag: &Tag, control: BoxRect, side: Side, area: (f32, f32)) -> BoxRect {
        let (at, position) = side.anchor(control);
        let [x, y] = self.look(tag).style.offset;
        let at = at.offset(Point::new(x, y));
        let dimensions @ (w, h) = self.look(tag).size(&tag.text);
        if side == Side::Center {
            return whole_px(place(at, position, dimensions, area).unwrap());
        }
        let (across, down) = position.halves();
        // Check the requested side before monitor-edge clamping can hide it.
        whole_px(
            Rect::from_xywh(
                (at.x as f32 - w * across as f32 / 2.0).round(),
                (at.y as f32 - h * down as f32 / 2.0).round(),
                w,
                h,
            )
            .unwrap(),
        )
    }

    #[allow(clippy::cast_possible_truncation)]
    fn arrange(&mut self, frame: &Frame, area: (f32, f32)) -> Frame {
        let screen = BoxRect::new(0, 0, area.0 as i32, area.1 as i32).inset(2);
        // Where each tag may go: its target's window, on the screen.
        let bounds: Vec<_> = frame
            .tags
            .iter()
            .map(|tag| self.label_bounds(tag, screen))
            .collect();
        // Text mode's matches are controls too: tags keep off them.
        let controls: Vec<_> = self
            .controls
            .iter()
            .copied()
            .chain(frame.highlights.iter().map(|h| h.rect))
            .collect();
        let sides = self.line_sides(frame, &bounds, &controls, area);
        // Each tag's own spot, clear of the tags placed before it.
        let mut originals = Vec::with_capacity(frame.tags.len());
        let mut ordinary_originals = Vec::with_capacity(frame.tags.len());
        for ((tag, &bounds), side) in frame.tags.iter().zip(&bounds).zip(sides) {
            let rect = match tag.control.filter(|_| tag.position == Position::Auto) {
                Some(control) => {
                    let around = Around {
                        controls: &controls,
                        tags: &ordinary_originals,
                        bounds,
                    };
                    self.beside(tag, control, side, &around, area)
                }
                None => whole_px(
                    place(tag.at, tag.position, self.look(tag).size(&tag.text), area).unwrap(),
                ),
            };
            originals.push(rect);
            if tag.kind != Kind::Window {
                ordinary_originals.push(rect);
            }
        }
        let labels: Vec<_> = frame
            .tags
            .iter()
            .zip(&originals)
            .zip(bounds.iter().copied())
            .filter(|((tag, _), _)| tag.kind != Kind::Window)
            .map(|((tag, &rect), bounds)| {
                let moved = tag.placement_key().and_then(|key| self.placed.get(&key));
                Label {
                    rect: moved.map_or(rect, |&by| rect.offset(by)),
                    target: tag.target,
                    bounds,
                    fixed: tag
                        .placement_key()
                        .is_some_and(|key| self.shared.contains(&key)),
                }
            })
            .collect();
        let space = self
            .space
            .get_or_insert_with(|| Space::rectangles(screen, &[]));
        let mut ordinary = placement::resolve(&labels, space).into_iter();
        let mut boxes: Vec<_> = frame
            .tags
            .iter()
            .map(|tag| {
                if tag.kind == Kind::Window {
                    BoxRect::default()
                } else {
                    ordinary.next().unwrap()
                }
            })
            .collect();
        self.window_boxes(frame, &originals, &bounds, &controls, &mut boxes);
        let mut result = frame.clone();
        for (i, ((tag, &original), rect)) in
            frame.tags.iter().zip(&originals).zip(boxes).enumerate()
        {
            // Each object keeps its own displacement through filtering.
            if let Some(key) = tag.placement_key() {
                let by = Point::new(rect.x - original.x, rect.y - original.y);
                self.placed.entry(key).or_insert(by);
            }
            result.tags[i].at = rect.origin();
            result.tags[i].position = Position::TopLeft;
            if rect != original
                && tag.kind != Kind::Window
                && let Some(at) = tag.target
                && !result.arrows.iter().any(|d| d.tag == Some(i))
            {
                result.arrows.push(ArrowTarget {
                    from: rect.center(),
                    at: tag.control.map_or(at, BoxRect::center),
                    tag: Some(i),
                });
            }
        }
        result
    }

    /// Keep labels on their window's exposed region, clipped to the monitor.
    fn label_bounds(&self, tag: &Tag, screen: BoxRect) -> BoxRect {
        if tag.kind == Kind::Window {
            return tag
                .control
                .and_then(|r| r.intersect(screen))
                .unwrap_or(screen);
        }
        tag.target
            .and_then(|p| {
                self.regions
                    .iter()
                    .rev()
                    .find(|(surface, _)| surface.contains(p))
            })
            .and_then(|(_, labels)| labels.intersect(screen))
            .unwrap_or(screen)
    }

    /// Window tags yield to every ordinary label and small control.
    fn window_boxes(
        &self,
        frame: &Frame,
        originals: &[BoxRect],
        bounds: &[BoxRect],
        controls: &[BoxRect],
        boxes: &mut [BoxRect],
    ) {
        for (i, tag) in frame
            .tags
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == Kind::Window)
        {
            let mut blocked: Vec<_> = controls
                .iter()
                .copied()
                .filter(|r| r.w <= 192 && r.h <= 96)
                .collect();
            blocked.extend(boxes.iter().copied().filter(|r| !r.is_empty()));
            let original = originals[i];
            let kept = tag
                .placement_key()
                .and_then(|key| self.placed.get(&key))
                .map_or(original, |&by| original.offset(by));
            boxes[i] = placement::window(kept, bounds[i], &blocked);
        }
    }

    /// An `Auto` tag by its `control`: on the first side of
    /// `Side::ORDER` that stays in its bounds and clear of controls and
    /// earlier tags, or on its line's shared side. The look's offset is
    /// included in clearance checks. Each tag keeps its side until the next scan.
    fn beside(
        &mut self,
        tag: &Tag,
        control: BoxRect,
        shared: Option<Side>,
        around: &Around,
        area: (f32, f32),
    ) -> BoxRect {
        let rect = |side: Side| self.side_rect(tag, control, side, area);
        let clear = |side: Side| side == Side::Center || around.clear(rect(side), control);
        let key = tag.placement_key();
        let kept = key.as_ref().and_then(|key| self.sides.get(key)).copied();
        let side = kept
            .or(shared)
            .unwrap_or_else(|| Side::ORDER.into_iter().find(|&s| clear(s)).unwrap());
        let result = rect(side);
        if let Some(key) = key {
            if shared.is_some() {
                self.shared.insert(key.clone());
            }
            self.sides.insert(key, side);
        }
        result
    }

    /// Clears `pixmap` and draws `frame` on it, in overlay coordinates.
    ///
    /// Words' first letters and highlights go under arrows and tags.
    pub fn paint(&mut self, pixmap: &mut Pixmap, frame: &Frame) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let scaled = Transform::from_scale(self.scale, self.scale);
        #[allow(clippy::cast_precision_loss)] // monitor sizes are far below 2^24
        let area = (
            pixmap.width() as f32 / self.scale,
            pixmap.height() as f32 / self.scale,
        );
        for &letter in &frame.starts {
            self.paint_start(pixmap, letter, scaled, area);
        }
        for highlight in &frame.highlights {
            self.paint_highlight(pixmap, *highlight, scaled, area);
        }
        let arranged = self.arrange(frame, area);
        let tags = &arranged;
        let mut arrows = Vec::new();
        for (i, tag) in tags.tags.iter().enumerate() {
            if tag.kind == Kind::Window {
                continue;
            }
            let at = tags
                .arrows
                .iter()
                .find(|d| d.tag == Some(i))
                .map(|d| d.at)
                .or_else(|| tag.control.map(BoxRect::center));
            let Some(at) = at else { continue };
            let look = self.look(tag);
            let rect = place(tag.at, tag.position, look.size(&tag.text), area).unwrap();
            let control = tag.control.filter(|r| at == r.center());
            let to = if let Some(control) = control {
                let Some(target) = padded(control, [0.0, 0.0]) else {
                    continue;
                };
                // Centered fallbacks and clamped hints need no arrow inside a control.
                if rect.intersect(&target).is_some() {
                    continue;
                }
                let middle = (
                    rect.x() + rect.width() / 2.0,
                    rect.y() + rect.height() / 2.0,
                );
                let Some(edge) = nearest_edge(target, 0.0, middle) else {
                    continue;
                };
                edge
            } else {
                centre(at)
            };
            if let Some(from) = nearest_edge(rect, look.style.radius, to) {
                arrows.extend(Self::arrow(from, to, &look.style, control.is_some()));
            }
        }
        for arrow in tags.arrows.iter().filter(|d| d.tag.is_none()) {
            arrows.extend(Self::arrow(
                centre(arrow.from),
                centre(arrow.at),
                &self.tags.style,
                false,
            ));
        }
        paint_layer(pixmap, &arrows, self.scale);
        for tag in &tags.tags {
            let look = match tag.kind {
                Kind::Plain | Kind::Window => &mut self.tags,
                Kind::Pixels => &mut self.pixels,
            };
            look.paint_tag(pixmap, tag, area);
        }
        self.paint_input(pixmap, frame, area);
    }

    /// The input box where `[input]` puts it on the screen: what's typed,
    /// then the message. Nothing when neither is there.
    #[allow(clippy::cast_possible_truncation)] // logical sizes fit an i32
    fn paint_input(&mut self, pixmap: &mut Pixmap, frame: &Frame, (w, h): (f32, f32)) {
        let rows = self.rows(frame, w);
        if rows.is_empty() {
            return;
        }
        let style = &self.input.style;
        let [x, y] = style.offset;
        let at = style
            .position
            .on(BoxRect::new(0, 0, w as i32, h as i32))
            .offset(Point::new(x, y));
        let Some(rect) = place(at, style.position, self.input.size_of(&rows), (w, h)) else {
            return;
        };
        self.input
            .draw(pixmap, rect, &rows, Some(&mut self.icons), false);
    }

    /// The input box's lines: what's typed, then the message, wrapped
    /// within a screen `width` wide.
    fn rows(&self, frame: &Frame, width: f32) -> Vec<Row> {
        let text = &self.input;
        let mut rows = Vec::new();
        let mut typed = 0.0;
        if let Some((line, dimmed)) = frame.status.line() {
            typed = text.width(&line);
            rows.push(text.row(&line, frame.status.verb_len(), dimmed));
        }
        if let Some(Message {
            kind,
            text: message,
        }) = &frame.status.info
        {
            let badge = self.info.badge(*kind);
            // An icon the font doesn't have would be a box: none instead.
            let icon = Some(&badge.icon).filter(|i| !i.is_empty() && self.icons.has(i));
            let indent = icon.map_or(0.0, |i| self.icons.width(i) + text.style.size * 0.4);
            let screen = width - 2.0 * (EDGE + text.style.padding[1]);
            let room = screen.min(typed.max(MESSAGE_WIDTH * text.style.size)) - indent;
            let lines = if message.contains('\t') {
                message.split('\n').map(str::to_owned).collect()
            } else {
                wrap(message, room, MESSAGE_LINES, |c| text.advance(c))
            };
            rows.extend(lines.into_iter().enumerate().map(|(i, line)| Row {
                text: line,
                verb: 0,
                dimmed: 0,
                color: badge.color,
                indent,
                icon: icon.filter(|_| i == 0).cloned(),
            }));
        }
        rows
    }

    /// A short triangle touches a target edge, or marks a point override without a tail.
    fn arrow(
        (x, y): (f32, f32),
        (to_x, to_y): (f32, f32),
        style: &Style,
        touch_edge: bool,
    ) -> Vec<Part> {
        let arrow = &style.arrow;
        let (color, outline) = arrow.colors(style);
        let distance = (to_x - x).hypot(to_y - y);
        let length = arrow
            .length
            .min(if touch_edge { distance } else { distance - 1.0 });
        if length <= 0.0 {
            return Vec::new();
        }
        let (dx, dy) = ((to_x - x) / distance, (to_y - y) / distance);
        let (x, y, tip_x, tip_y) = if touch_edge {
            (to_x - dx * length, to_y - dy * length, to_x, to_y)
        } else {
            (x, y, x + dx * length, y + dy * length)
        };
        let half = length * 0.65;
        let mut path = PathBuilder::new();
        path.move_to(x - dy * half, y + dx * half);
        path.line_to(tip_x, tip_y);
        path.line_to(x + dy * half, y - dx * half);
        path.close();
        let path = path.finish().unwrap();
        let mut parts = Vec::new();
        if arrow.outline_width > 0.0 {
            parts.push(Part {
                path: path.clone(),
                width: Some(arrow.outline_width),
                color: outline,
            });
        }
        parts.push(Part {
            path,
            width: None,
            color,
        });
        parts
    }

    /// One line of a match: its box grown by the mark's padding, filled
    /// and bordered as a tag is, with no letters.
    fn paint_highlight(
        &self,
        pixmap: &mut Pixmap,
        highlight: Highlight,
        scaled: Transform,
        area: (f32, f32),
    ) {
        let mark = if highlight.current {
            self.marks.current
        } else {
            self.marks.all
        };
        if let Some(rect) = padded(highlight.rect, mark.padding) {
            paint_box(pixmap, rect, &mark, scaled, area);
        }
    }

    /// A word's first letter, at `letter`, as `[words.first_character]` marks it: a
    /// line along the bottom of its box, a box over it, or a dot below it.
    #[allow(clippy::cast_precision_loss)] // screen coordinates are far below 2^24
    fn paint_start(
        &self,
        pixmap: &mut Pixmap,
        letter: BoxRect,
        scaled: Transform,
        area: (f32, f32),
    ) {
        let starts = self.marks.starts;
        let (rect, mark) = match starts.mark {
            StartMark::Off => return,
            StartMark::Underline => {
                let line = Mark {
                    border_width: 0.0,
                    radius: starts.thickness / 2.0,
                    ..starts.look
                };
                let (x, w) = (letter.x as f32, letter.w as f32);
                let y = letter.bottom() as f32 - starts.thickness;
                (Rect::from_xywh(x, y, w, starts.thickness), line)
            }
            StartMark::Box => (padded(letter, starts.look.padding), starts.look),
            StartMark::Dot => {
                let diameter = starts.thickness;
                let x = letter.x as f32 + (letter.w as f32 - diameter) / 2.0;
                let y = letter.bottom() as f32 + 1.0;
                let dot = Mark {
                    border_width: 0.0,
                    radius: diameter / 2.0,
                    ..starts.look
                };
                (Rect::from_xywh(x, y, diameter, diameter), dot)
            }
        };
        if let Some(rect) = rect {
            paint_box(pixmap, rect, &mark, scaled, area);
        }
    }
}

/// `r` grown by `[vertical, horizontal]` px each side, as a mark pads its
/// text.
#[allow(clippy::cast_precision_loss)] // screen coordinates are far below 2^24
fn padded(r: BoxRect, [pad_y, pad_x]: [f32; 2]) -> Option<Rect> {
    Rect::from_xywh(
        r.x as f32 - pad_x,
        r.y as f32 - pad_y,
        r.w as f32 + 2.0 * pad_x,
        r.h as f32 + 2.0 * pad_y,
    )
}

/// A font and glyph cache for one look; the tags and the input box have
/// their own.
struct TextPainter {
    style: Style,
    font: fontdue::Font,
    /// Buffer px per logical px: glyphs are rasterized this much bigger.
    scale: f32,
    /// Distance from a line's top to its baseline, and its full height,
    /// in logical px.
    ascent: f32,
    line_height: f32,
    /// Keyed by letter and colour.
    glyphs: HashMap<(char, Color), Glyph>,
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

    /// How wide `text` is, in logical px.
    fn width(&self, text: &str) -> f32 {
        text.chars().map(|c| self.advance(c)).sum()
    }

    /// Whether the font has every letter of `text`.
    fn has(&self, text: &str) -> bool {
        text.chars().all(|c| self.font.lookup_glyph_index(c) != 0)
    }

    /// The gap between two columns: the box's side padding, twice.
    fn gap(&self) -> f32 {
        2.0 * self.style.padding[1]
    }

    /// Each column's width: the widest of its cells in the rows with
    /// tabs. A row without one is a line of its own.
    fn columns(&self, rows: &[Row]) -> Vec<f32> {
        let mut widths: Vec<f32> = Vec::new();
        for row in rows.iter().filter(|r| r.text.contains('\t')) {
            for (i, cell) in row.text.split('\t').enumerate() {
                let width = self.width(cell);
                match widths.get_mut(i) {
                    Some(widest) => *widest = widest.max(width),
                    None => widths.push(width),
                }
            }
        }
        widths
    }

    /// A tag's box size: its text plus padding on every side.
    fn size(&self, text: &str) -> (f32, f32) {
        self.size_of(&[self.row(text, 0, 0)])
    }

    /// A box's size: its widest row and every row's height, plus padding
    /// on every side.
    #[allow(clippy::cast_precision_loss)] // a box has a few rows
    fn size_of(&self, rows: &[Row]) -> (f32, f32) {
        let [pad_y, pad_x] = self.style.padding;
        let columns = self.columns(rows);
        let width = rows
            .iter()
            .map(|row| {
                let cells = row.text.split('\t').count();
                let text = if cells > 1 {
                    columns.iter().take(cells).sum::<f32>() + (cells - 1) as f32 * self.gap()
                } else {
                    self.width(&row.text)
                };
                row.indent + text
            })
            .fold(0.0, f32::max);
        (
            (width + 2.0 * pad_x).ceil(),
            (self.line_height * rows.len() as f32 + 2.0 * pad_y).ceil(),
        )
    }

    /// A row of `text` in the text colour, its first `typed` letters
    /// dimmed, the first `verb` of those in the verb colour: a tag, or
    /// what's typed in the input box.
    fn row(&self, text: &str, verb: usize, typed: usize) -> Row {
        Row {
            text: text.to_owned(),
            verb,
            dimmed: typed,
            color: self.style.text,
            indent: 0.0,
            icon: None,
        }
    }

    fn paint_tag(&mut self, pixmap: &mut Pixmap, tag: &Tag, area: (f32, f32)) {
        let row = self.row(&tag.text, tag.verb, tag.typed);
        let size = self.size_of(std::slice::from_ref(&row));
        if let Some(rect) = place(tag.at, tag.position, size, area) {
            self.draw(pixmap, rect, &[row], None, true);
        }
    }

    /// The box at `rect` (logical px) and its rows: a message's icon in
    /// `icons`' font, then the letters, on a baseline `ascent` below the
    /// row's top. Each column starts past the widest cell of the one
    /// before.
    fn draw(
        &mut self,
        pixmap: &mut Pixmap,
        rect: Rect,
        rows: &[Row],
        mut icons: Option<&mut Self>,
        gradients: bool,
    ) {
        let s = self.scale;
        let scaled = Transform::from_scale(s, s);
        let style = &self.style;
        #[allow(clippy::cast_precision_loss)] // monitor sizes are far below 2^24
        let area = (pixmap.width() as f32 / s, pixmap.height() as f32 / s);
        let background = gradient(
            style.background,
            style.background_end.filter(|_| gradients),
            style.background_direction,
            area,
        );
        let text = style.text_end.filter(|_| gradients).map(|end| {
            let mut paint = gradient(style.text, Some(end), style.text_direction, area);
            paint.shader.transform(scaled);
            paint
        });
        paint_box_fill(
            pixmap,
            rect,
            &Mark {
                background: style.background,
                background_end: None,
                background_direction: style.background_direction,
                border: style.border,
                border_width: style.border_width,
                radius: style.radius,
                padding: style.padding,
            },
            scaled,
            &background,
        );
        let [pad_y, pad_x] = style.padding;
        let (verb, dimmed) = (style.verb, style.dimmed);
        let (columns, gap) = (self.columns(rows), self.gap());
        let mut top = rect.top() + pad_y;
        for row in rows {
            let left = rect.left() + pad_x;
            let baseline = top + self.ascent;
            if let (Some(icon), Some(icons)) = (&row.icon, icons.as_deref_mut()) {
                let mut pen = left;
                for letter in icon.chars() {
                    pen += icons.letter(pixmap, (pen, baseline), letter, row.color, None);
                }
            }
            let mut start = left + row.indent;
            let mut index = 0;
            for (i, cell) in row.text.split('\t').enumerate() {
                let mut pen = start;
                for letter in cell.chars() {
                    let (color, gradient) = if index < row.verb {
                        (verb, None)
                    } else if index < row.dimmed {
                        (dimmed, None)
                    } else {
                        (row.color, text.as_ref())
                    };
                    pen += self.letter(pixmap, (pen, baseline), letter, color, gradient);
                    index += 1;
                }
                // The tab counts as a character.
                index += 1;
                start += columns.get(i).copied().unwrap_or_default() + gap;
            }
            top += self.line_height;
        }
    }

    /// Draws `letter` in `color` with the pen at `(pen, baseline)`, in
    /// logical px, and says how far the pen moves.
    fn letter(
        &mut self,
        pixmap: &mut Pixmap,
        (pen, baseline): (f32, f32),
        letter: char,
        color: Color,
        gradient: Option<&Paint<'_>>,
    ) -> f32 {
        let s = self.scale;
        let advance = self.advance(letter);
        // Gradient glyphs cache only opaque coverage; their screen position
        // changes the paint, never the glyph cache key.
        let glyph = self.glyph(
            letter,
            gradient.map_or(color, |_| Color::rgba(255, 255, 255, 255)),
        );
        let m = glyph.metrics;
        if let Some(ink) = &glyph.pixmap {
            // fontdue measures `ymin` up from the baseline to the glyph's
            // bottom edge; pixmaps count down from the top.
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let (x, y) = (
                (pen * s + m.xmin as f32).round() as i32,
                (baseline * s - m.ymin as f32 - m.height as f32).round() as i32,
            );
            let tinted = gradient.map(|paint| {
                let mut tinted = ink.clone();
                let mut paint = paint.clone();
                paint.blend_mode = BlendMode::SourceIn;
                #[allow(clippy::cast_precision_loss)] // buffer coordinates are far below 2^24
                paint
                    .shader
                    .transform(Transform::from_translate(-x as f32, -y as f32));
                #[allow(clippy::cast_precision_loss)] // glyph sizes are far below 2^24
                let rect = Rect::from_xywh(0.0, 0.0, tinted.width() as f32, tinted.height() as f32)
                    .unwrap();
                tinted.fill_rect(rect, &paint, Transform::identity(), None);
                tinted
            });
            pixmap.draw_pixmap(
                x,
                y,
                tinted.as_ref().unwrap_or(ink).as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        advance
    }

    /// A letter in `color`, rasterized on first use at the buffer's size.
    fn glyph(&mut self, letter: char, color: Color) -> &Glyph {
        let (font, size) = (&self.font, self.style.size * self.scale);
        // `entry().or_insert_with` looks the key up once and runs the closure
        // only when it's missing: the cache in one line.
        self.glyphs.entry((letter, color)).or_insert_with(|| {
            let (metrics, coverage) = font.rasterize(letter, size);
            Glyph {
                metrics,
                pixmap: tint(&coverage, metrics.width, metrics.height, color),
            }
        })
    }
}

/// `text` in at most `most` lines no wider than `room`, broken at spaces,
/// or inside a word too wide for a line. What doesn't fit is cut, and the
/// last line ends in `…`.
fn wrap(text: &str, room: f32, most: usize, advance: impl Fn(char) -> f32) -> Vec<String> {
    let width = |s: &str| s.chars().map(&advance).sum::<f32>();
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let joined = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if width(&joined) <= room {
            line = joined;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        for letter in word.chars() {
            if !line.is_empty() && width(&line) + advance(letter) > room {
                lines.push(std::mem::take(&mut line));
            }
            line.push(letter);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > most {
        lines.truncate(most);
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && width(last.trim_end()) + advance('…') > room {
                last.pop();
            }
            last.truncate(last.trim_end().len());
            last.push('…');
        }
    }
    lines
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

/// A drawn box in whole px, as placement counts.
#[allow(clippy::cast_possible_truncation)] // logical sizes fit an i32
fn whole_px(rect: Rect) -> BoxRect {
    BoxRect::new(
        rect.x() as i32,
        rect.y() as i32,
        rect.width() as i32,
        rect.height() as i32,
    )
}

/// A box filled and bordered in `look`'s colours: the fill, then the border
/// stroked half a width inside, so the whole line stays within the box.
/// Its padding is the caller's business. `rect` is in logical px, which
/// `transform` takes to the buffer's.
fn paint_box(pixmap: &mut Pixmap, rect: Rect, look: &Mark, transform: Transform, area: (f32, f32)) {
    let fill = gradient(
        look.background,
        look.background_end,
        look.background_direction,
        area,
    );
    paint_box_fill(pixmap, rect, look, transform, &fill);
}

/// Fill a box with a solid colour or screen-wide gradient, keeping its border solid.
fn paint_box_fill(
    pixmap: &mut Pixmap,
    rect: Rect,
    look: &Mark,
    transform: Transform,
    fill: &Paint<'_>,
) {
    let inset = look.border_width / 2.0;
    if let Some(path) = rounded_rect(rect, look.radius) {
        pixmap.fill_path(&path, fill, FillRule::Winding, transform, None);
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

/// The closest point on a rounded tag's outline. A target inside the tag
/// needs no arrow through its background and letters.
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

/// One part of an arrow: `path` stroked `width` px wide, or filled when
/// `width` is `None`.
struct Part {
    path: Path,
    width: Option<f32>,
    color: Color,
}

/// Paints `parts` on a layer of their own, each replacing what's under it,
/// then lays the layer over `pixmap`. A translucent arrow then shows
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

/// One gradient over the monitor in logical px; drawing shapes clips its visibility.
fn gradient(
    start: Color,
    end: Option<Color>,
    direction: Direction,
    (w, h): (f32, f32),
) -> Paint<'static> {
    let Some(end) = end else {
        return paint(start);
    };
    let ((x0, y0), (x1, y1)) = direction.ends();
    let color = |c: Color| tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a);
    let shader = LinearGradient::new(
        tiny_skia::Point::from_xy(x0 * w, y0 * h),
        tiny_skia::Point::from_xy(x1 * w, y1 * h),
        vec![
            GradientStop::new(0.0, color(start)),
            GradientStop::new(1.0, color(end)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    );
    Paint {
        shader: shader.unwrap_or_else(|| paint(start).shader),
        ..Paint::default()
    }
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
    use crate::style::Notice;

    fn gradient_tag(x: i32, y: i32, text: &str) -> Tag {
        Tag {
            at: Point::new(x, y),
            position: Position::TopLeft,
            control: None,
            text: text.into(),
            typed: 0,
            verb: 0,
            target: None,
            object: None,
            kind: Kind::Plain,
        }
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn background_gradients_span_the_monitor_and_stay_inside_tags() {
        for scale in [1.0, 1.25, 2.0] {
            for direction in [
                Direction::Right,
                Direction::Left,
                Direction::Bottom,
                Direction::Top,
                Direction::BottomRight,
                Direction::TopLeft,
                Direction::TopRight,
                Direction::BottomLeft,
            ] {
                let style = Style {
                    background: Color::rgba(255, 0, 0, 255),
                    background_end: Some(Color::rgba(0, 0, 255, 255)),
                    background_direction: direction,
                    border_width: 0.0,
                    radius: 4.0,
                    padding: [5.0, 10.0],
                    ..Style::default()
                };
                let mut painter = TextPainter::new(&style, scale).unwrap();
                let mut map = Pixmap::new((400.0 * scale) as u32, (200.0 * scale) as u32).unwrap();
                for (x, y) in [(20, 20), (200, 80), (340, 140)] {
                    painter.paint_tag(&mut map, &gradient_tag(x, y, " "), (400.0, 200.0));
                    let (px, py) = (
                        ((x + 8) as f32 * scale) as u32,
                        ((y + 8) as f32 * scale) as u32,
                    );
                    let (fx, fy) = ((px as f32 + 0.5) / scale, (py as f32 + 0.5) / scale);
                    let t = match direction {
                        Direction::Right => fx / 400.0,
                        Direction::Left => 1.0 - fx / 400.0,
                        Direction::Bottom => fy / 200.0,
                        Direction::Top => 1.0 - fy / 200.0,
                        Direction::BottomRight => (fx * 400.0 + fy * 200.0) / 200_000.0,
                        Direction::TopLeft => 1.0 - (fx * 400.0 + fy * 200.0) / 200_000.0,
                        Direction::TopRight => (fx * 400.0 + (200.0 - fy) * 200.0) / 200_000.0,
                        Direction::BottomLeft => ((400.0 - fx) * 400.0 + fy * 200.0) / 200_000.0,
                    };
                    let pixel = map.pixel(px, py).unwrap();
                    assert_eq!(pixel.alpha(), 255);
                    assert!(
                        (f32::from(pixel.blue()) - t * 255.0).abs() < 2.0,
                        "{direction:?} at {scale}x: {pixel:?}, expected {t}"
                    );
                    assert!((f32::from(pixel.red()) - (1.0 - t) * 255.0).abs() < 2.0);
                }
                assert_eq!(map.pixel(0, 0).unwrap().alpha(), 0);
                assert_eq!(
                    map.pixel((100.0 * scale) as u32, (100.0 * scale) as u32)
                        .unwrap()
                        .alpha(),
                    0
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn text_gradients_follow_screen_pixels_preserve_coverage_and_special_colours() {
        for scale in [1.0, 1.25, 2.0] {
            for direction in [Direction::Right, Direction::Bottom, Direction::TopLeft] {
                let style = Style {
                    text: Color::rgba(255, 0, 0, 255),
                    text_end: Some(Color::rgba(0, 0, 255, 0)),
                    text_direction: direction,
                    dimmed: Color::rgba(0, 255, 0, 180),
                    verb: Color::rgba(255, 255, 0, 255),
                    background: Color::rgba(0, 0, 0, 0),
                    border_width: 0.0,
                    ..Style::default()
                };
                let solid = Style {
                    text: Color::rgba(255, 255, 255, 255),
                    text_end: None,
                    ..style.clone()
                };
                let mut painter = TextPainter::new(&style, scale).unwrap();
                let mut reference = TextPainter::new(&solid, scale).unwrap();
                let mut map = Pixmap::new((400.0 * scale) as u32, (160.0 * scale) as u32).unwrap();
                let mut coverage = map.clone();
                for x in [20, 180, 320] {
                    let tag = gradient_tag(x, 40, "MMM");
                    painter.paint_tag(&mut map, &tag, (400.0, 160.0));
                    reference.paint_tag(&mut coverage, &tag, (400.0, 160.0));
                }
                let mut checked = 0;
                for (i, (pixel, mask)) in map.pixels().iter().zip(coverage.pixels()).enumerate() {
                    if mask.alpha() == 0 {
                        assert_eq!(pixel.alpha(), 0);
                        continue;
                    }
                    let fx = ((i % map.width() as usize) as f32 + 0.5) / scale;
                    let fy = ((i / map.width() as usize) as f32 + 0.5) / scale;
                    let t = match direction {
                        Direction::Bottom => fy / 160.0,
                        Direction::TopLeft => 1.0 - (fx * 400.0 + fy * 160.0) / 185_600.0,
                        _ => fx / 400.0,
                    };
                    let alpha = f32::from(mask.alpha()) * (1.0 - t);
                    // tiny-skia's integer pipeline rounds interpolation and
                    // coverage separately, within three channel levels of f32.
                    assert!((f32::from(pixel.alpha()) - alpha).abs() <= 3.0);
                    assert!((f32::from(pixel.red()) - alpha * (1.0 - t)).abs() <= 3.0);
                    assert!(
                        (f32::from(pixel.blue()) - alpha * t).abs() <= 3.0,
                        "scale={scale}, t={t}, pixel={pixel:?}, mask={mask:?}, expected alpha={alpha}"
                    );
                    checked += 1;
                }
                assert!(checked > 100);
                assert_eq!(
                    painter.glyphs.len(),
                    1,
                    "screen position must not grow the glyph cache"
                );
                let mut tag = gradient_tag(20, 90, "MMM");
                tag.typed = 2;
                tag.verb = 1;
                painter.paint_tag(&mut map, &tag, (400.0, 160.0));
                assert!(
                    map.pixels()
                        .iter()
                        .any(|p| p.red() == 255 && p.green() == 255 && p.blue() == 0)
                );
                assert!(map.pixels().iter().any(|p| p.red() == 0
                    && p.green() == 180
                    && p.blue() == 0
                    && p.alpha() == 180));
            }
        }
    }

    #[test]
    fn query_has_its_own_font_colours_and_box_above_the_marks() {
        let config = crate::config::Config::parse(
            r##"
[hints]
size = 8
text = "#ff0000"
text_end = "#0000ff"
background_end = "#ffffff"
[words.matches]
background = "#0000ff"
padding = [0, 0]
radius = 0
[words.matches.current]
background = "#ff00ff"
[words.input]
font = "serif"
size = 28
text = "#00ff00"
verb = "#00ffff"
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
            &config.info,
            1.0,
        )
        .unwrap();
        let mut pixmap = Pixmap::new(300, 160).unwrap();
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
            status: Status::dimmed("A", "B"),
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
            "the verb's colour"
        );
        frame.status = Status::default();
        painter.paint(&mut pixmap, &frame);
        assert_eq!(
            rgb(&pixmap, 24, 24),
            (0, 0, 255),
            "no query left when returning to hints"
        );
    }

    #[test]
    fn an_empty_placeholder_hides_the_box_until_input_or_a_message() {
        let style = Style::default();
        let input = crate::style::Input {
            placeholder: String::new(),
            ..crate::style::Input::new(&style)
        };
        let mut painter = Painter::new(
            &style,
            &Marks::new(&style),
            &input.style,
            &Info::default(),
            1.0,
        )
        .unwrap();
        let mut pixmap = Pixmap::new(800, 600).unwrap();
        for (query, message, visible) in [
            (None, false, false),
            (Some(""), false, false),
            (Some("x"), false, true),
            (Some(""), false, false),
            (Some(""), true, true),
            (None, true, true),
            (Some(""), false, false),
        ] {
            let frame = Frame {
                status: Status {
                    info: message.then(|| Message::new(crate::style::Notice::NoMatch, "no match")),
                    ..Status::text(None, false, query, None, &input)
                },
                ..Frame::default()
            };
            assert_eq!(
                painter.rows(&frame, 800.0).len(),
                usize::from(visible),
                "query {query:?}, message {message}: no empty row above a warning"
            );
            painter.paint(&mut pixmap, &frame);
            assert_eq!(
                pixmap.pixels().iter().any(|pixel| pixel.alpha() > 0),
                visible,
                "query {query:?}, message {message}"
            );
        }
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
                Painter::new(&style, &Marks::new(&style), &input, &Info::default(), 1.0).unwrap();
            for text in ["a", "a longer query  1/2"] {
                let frame = Frame {
                    status: Status::plain(text),
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
        let mut style = Style {
            text: Color::rgba(240, 20, 30, 255),
            background: Color::rgba(30, 30, 30, 255),
            radius: 0.0,
            border_width: 0.0,
            padding: [4.0, 6.0],
            ..Style::default()
        };
        style.arrow.color = Some(Color::rgba(255, 255, 255, 255));
        style.arrow.outline = Some(Color::rgba(255, 255, 255, 255));
        let mut marks = Marks::new(&style);
        marks.all.background = Color::rgba(0, 0, 255, 255);
        marks.all.border_width = 0.0;
        marks.all.radius = 0.0;
        marks.all.padding = [0.0, 0.0];
        let frame = Frame {
            tags: vec![Tag {
                at: Point::new(20, 20),
                position: Position::TopLeft,
                control: None,
                text: "AB".into(),
                typed: 0,
                target: None,
                object: None,
                verb: 0,
                kind: Kind::Plain,
            }],
            highlights: vec![Highlight {
                rect: BoxRect::new(20, 100, 40, 20),
                current: false,
            }],
            arrows: vec![ArrowTarget {
                from: Point::new(150, 20),
                at: Point::new(150, 100),
                tag: None,
            }],
            ..Frame::default()
        };
        let render = |scale: f32| {
            let mut painter =
                Painter::new(&style, &marks, &style, &Info::default(), scale).unwrap();
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

        // The arrow stays at its anchor at either scale; no tail or target dot.
        for (image, scale) in [(&one, 1.0), (&big, 1.5)] {
            let at = |x: f32, y: f32| {
                image
                    .pixel((x * scale) as u32, (y * scale) as u32)
                    .unwrap()
                    .alpha()
            };
            assert!(at(150.0, 21.0) > 0);
            assert_eq!(at(150.0, 60.0), 0);
            assert_eq!(at(150.0, 100.0), 0);
        }
    }

    #[test]
    fn messages_wrap_to_two_lines_and_say_when_cut() {
        let wrap = |text, room| wrap(text, room, 2, |_| 10.0);
        assert_eq!(wrap("scanning", 100.0), ["scanning"]);
        assert_eq!(wrap("no tag starts with x", 100.0), ["no tag", "starts…"]);
        assert_eq!(
            wrap("abcdefghijklmno", 100.0),
            ["abcdefghij", "klmno"],
            "a word wider than a line breaks inside"
        );
        assert_eq!(
            wrap("aaaaaaaaaa bbbbbbbbbb cc", 100.0),
            ["aaaaaaaaaa", "bbbbbbbbb…"],
            "the cut mark fits in the line"
        );
    }

    /// A painter whose input box sits `offset` from the top-left corner,
    /// white on opaque grey.
    fn boxed(offset: [i32; 2]) -> Painter {
        let tags = Style::default();
        let input = Style {
            text: Color::rgba(255, 255, 255, 255),
            background: Color::rgba(60, 60, 60, 255),
            border_width: 0.0,
            position: Position::TopLeft,
            offset,
            ..tags.clone()
        };
        Painter::new(&tags, &Marks::new(&tags), &input, &Info::default(), 1.0).unwrap()
    }

    #[test]
    fn the_input_box_has_what_is_typed_then_the_message() {
        let mut painter = boxed([10, 10]);
        let red = Info::default().badges.no_match.color;
        let is = |c: Color| {
            move |p: PremultipliedColorU8| {
                (p.red(), p.green(), p.blue(), p.alpha()) == (c.r, c.g, c.b, 255)
            }
        };
        let mut pixmap = Pixmap::new(800, 200).unwrap();
        painter.paint(&mut pixmap, &Frame::default());
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0), "no box");

        let frame = Frame {
            status: Status {
                info: Some(Message::new(Notice::NoMatch, "no tag starts with q")),
                ..Status::plain("abc")
            },
            ..Frame::default()
        };
        painter.paint(&mut pixmap, &frame);
        let typed = bounds(&pixmap, is(Color::rgba(255, 255, 255, 255)));
        let message = bounds(&pixmap, is(red));
        assert!(message[1] >= typed[3], "the message is under what's typed");
        assert!(
            (message[0] - typed[0]).abs() <= 2,
            "the icon starts where the typed text does"
        );
        let shown = bounds(&pixmap, |p| p.alpha() > 0);
        assert!(shown[0] >= 10 && shown[1] >= 10, "where [input] puts it");

        // A long message on a narrow screen: two lines, inside the screen.
        let mut narrow = Pixmap::new(160, 200).unwrap();
        let long = Frame {
            status: Status::saying(
                Notice::NoMatch,
                "no text on this monitor matches what you typed, so the key was dropped",
            ),
            ..Frame::default()
        };
        painter.paint(&mut narrow, &long);
        let shown = bounds(&narrow, |p| p.alpha() > 0);
        assert!(shown[2] <= 160 - 2, "inside the screen: {shown:?}");
        let (_, two) = painter.input.size_of(&painter.rows(&long, 160.0));
        assert_eq!(painter.rows(&long, 160.0).len(), 2);
        #[allow(clippy::cast_possible_truncation)]
        let height = two as i32;
        assert_eq!(shown[3] - shown[1], height);
    }

    #[test]
    fn each_message_has_its_own_icon_and_colour() {
        let mut painter = boxed([10, 10]);
        let badges = Info::default().badges;
        let mut icons = Vec::new();
        for (notice, badge) in [
            (Notice::Scanning, &badges.scanning),
            (Notice::NoTargets, &badges.no_targets),
            (Notice::NoMatch, &badges.no_match),
            (Notice::Help, &badges.help),
        ] {
            assert!(
                painter.icons.has(&badge.icon),
                "the default icons need the Symbols Nerd Font installed"
            );
            let frame = Frame {
                status: Status::saying(notice, "words"),
                ..Frame::default()
            };
            let mut pixmap = Pixmap::new(400, 100).unwrap();
            painter.paint(&mut pixmap, &frame);
            // The icon is left of where the words start; its edges blend
            // into the box, so near the colour will do.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let words = (10.0
                + painter.input.style.padding[1]
                + painter.rows(&frame, 400.0)[0].indent) as usize;
            let color = badge.color;
            let near = |a: u8, b: u8| a.abs_diff(b) <= 24;
            let ink: Vec<usize> = pixmap
                .pixels()
                .iter()
                .enumerate()
                .filter(|&(i, p)| {
                    i % 400 < words
                        && near(p.red(), color.r)
                        && near(p.green(), color.g)
                        && near(p.blue(), color.b)
                })
                .map(|(i, _)| i % 400)
                .collect();
            assert!(ink.len() > 4, "{notice:?} draws its icon in its colour");
            icons.push(ink);
        }
        for (i, a) in icons.iter().enumerate() {
            for b in &icons[i + 1..] {
                assert_ne!(a, b, "two messages share an icon");
            }
        }

        // No icon, or one the font lacks: the words start at the edge.
        let frame = Frame {
            status: Status::saying(Notice::NoMatch, "words"),
            ..Frame::default()
        };
        for (font, icon) in [("Symbols Nerd Font", ""), ("sans-serif", "\u{f057}")] {
            let mut info = Info {
                icon_font: font.into(),
                ..Info::default()
            };
            info.badges.no_match.icon = icon.into();
            let tags = Style::default();
            let bare = Painter::new(&tags, &Marks::new(&tags), &tags, &info, 1.0).unwrap();
            let rows = bare.rows(&frame, 400.0);
            assert_eq!(
                (rows[0].indent, &rows[0].icon),
                (0.0, &None),
                "{font} {icon:?}"
            );
        }
    }

    #[test]
    fn the_key_list_lines_up_its_columns_across_lines() {
        let mut painter = boxed([10, 10]);
        let legend = "a\tx\nlonger\tx";
        let frame = Frame {
            status: Status::saying(Notice::Help, legend),
            ..Frame::default()
        };
        assert_eq!(
            painter.rows(&frame, 400.0).len(),
            2,
            "shown whole, unwrapped"
        );
        let mut pixmap = Pixmap::new(400, 200).unwrap();
        painter.paint(&mut pixmap, &frame);
        // Each line ends in the same letter, in the same column: the same
        // right edge, though "a" is far shorter than "longer".
        let pad = painter.input.style.padding[0];
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let split = (10.0 + pad + painter.input.line_height).round() as u32;
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
            // Letters in the help colour, over the grey box.
            bounds(&band, |p| p.blue() > 160)[2]
        };
        assert_eq!(right(0..split), right(split..200));
    }

    #[test]
    fn tags_in_pixels_have_their_own_look() {
        let style = Style {
            text: Color::rgba(255, 0, 0, 255),
            background: Color::rgba(0, 0, 0, 255),
            position: Position::TopLeft,
            ..Style::default()
        };
        let teal = style.for_pixels().text;
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        let tag = |x, kind| Tag {
            at: Point::new(x, 20),
            position: Position::TopLeft,
            control: None,
            text: "WW".into(),
            typed: 0,
            verb: 0,
            target: None,
            object: None,
            kind,
        };
        let frame = Frame {
            tags: vec![tag(20, Kind::Plain), tag(120, Kind::Pixels)],
            ..Frame::default()
        };
        let mut pixmap = Pixmap::new(300, 60).unwrap();
        painter.paint(&mut pixmap, &frame);
        let is = |c: Color| {
            move |p: PremultipliedColorU8| (p.red(), p.green(), p.blue()) == (c.r, c.g, c.b)
        };
        let red = bounds(&pixmap, is(style.text));
        let pixels = bounds(&pixmap, is(teal));
        assert!(red[2] <= 100, "[hints] letters only on the first: {red:?}");
        assert!(
            (120..200).contains(&pixels[0]) && pixels[2] <= 200,
            "teal letters only on the second: {pixels:?}"
        );
    }

    #[test]
    fn a_verb_in_front_of_the_letters_has_its_own_colour() {
        let style = Style {
            text: Color::rgba(255, 0, 0, 255),
            dimmed: Color::rgba(0, 0, 255, 255),
            background: Color::rgba(0, 0, 0, 255),
            position: Position::TopLeft,
            ..Style::default()
        };
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        // `,` armed, `W` typed, `M` still to type.
        let frame = Frame {
            tags: vec![Tag {
                at: Point::new(20, 20),
                position: Position::TopLeft,
                control: None,
                text: ",WM".into(),
                typed: 2,
                verb: 1,
                target: None,
                object: None,
                kind: Kind::Plain,
            }],
            ..Frame::default()
        };
        let mut pixmap = Pixmap::new(200, 60).unwrap();
        painter.paint(&mut pixmap, &frame);
        let is = |c: Color| {
            move |p: PremultipliedColorU8| (p.red(), p.green(), p.blue()) == (c.r, c.g, c.b)
        };
        let verb = bounds(&pixmap, is(style.verb));
        let dimmed = bounds(&pixmap, is(style.dimmed));
        let text = bounds(&pixmap, is(style.text));
        assert!(verb[2] <= dimmed[0], "the verb first: {verb:?} {dimmed:?}");
        assert!(
            dimmed[2] <= text[0],
            "then the typed letter: {dimmed:?} {text:?}"
        );
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
        for (point, edge) in [
            ((5.0, 38.0), (20.0, 38.0)),
            ((75.0, 42.0), (60.0, 42.0)),
            ((35.0, 5.0), (35.0, 30.0)),
            ((45.0, 75.0), (45.0, 50.0)),
        ] {
            assert_eq!(nearest_edge(rect, 4.0, point), Some(edge));
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
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn tiny_arrows_follow_all_four_tag_edges_without_a_tail_or_dot() {
        let style = Style {
            padding: [8.0, 20.0],
            radius: 6.0,
            ..Style::default()
        };
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        let mut pixmap = Pixmap::new(240, 200).unwrap();
        let mut frame = Frame {
            tags: vec![Tag {
                at: Point::new(100, 80),
                position: Position::TopLeft,
                control: None,
                text: "AA".into(),
                typed: 0,
                target: None,
                object: None,
                verb: 0,
                kind: Kind::Plain,
            }],
            arrows: vec![ArrowTarget {
                from: Point::new(100, 80),
                at: Point::default(),
                tag: Some(0),
            }],
            ..Frame::default()
        };
        let rect = whole_px(
            place(
                frame.tags[0].at,
                Position::TopLeft,
                painter.tags.size("AA"),
                (240.0, 200.0),
            )
            .unwrap(),
        );
        let middle = rect.center();
        for (target, near, gap) in [
            (
                Point::new(60, middle.y),
                Point::new(rect.x - 1, middle.y),
                Point::new(80, middle.y),
            ),
            (
                Point::new(middle.x, 40),
                Point::new(middle.x, rect.y - 1),
                Point::new(middle.x, 60),
            ),
            (
                Point::new(220, middle.y),
                Point::new(rect.right() + 1, middle.y),
                Point::new(210, middle.y),
            ),
            (
                Point::new(middle.x, 180),
                Point::new(middle.x, rect.bottom() + 1),
                Point::new(middle.x, 160),
            ),
        ] {
            frame.arrows[0].at = target;
            painter.paint(&mut pixmap, &frame);
            assert!(
                pixmap.pixel(near.x as u32, near.y as u32).unwrap().alpha() > 0,
                "arrow toward {target:?}"
            );
            for at in [gap, target] {
                assert_eq!(
                    pixmap.pixel(at.x as u32, at.y as u32).unwrap().alpha(),
                    0,
                    "no tail or dot at {at:?}"
                );
            }
        }
        // Clamping uses the drawn box, rather than its offscreen anchor.
        frame.tags[0].at = Point::new(-100, 80);
        frame.arrows[0].at = Point::new(25, 180);
        painter.paint(&mut pixmap, &frame);
        assert!(
            pixmap
                .pixel(25, (rect.bottom() + 1) as u32)
                .unwrap()
                .alpha()
                > 0
        );
    }

    #[test]
    fn a_translucent_arrow_shows_the_window_through_its_fill() {
        let mut style = Style::default();
        style.arrow.color = Some(Color::rgba(255, 0, 0, 128));
        style.arrow.outline = Some(Color::rgba(0, 0, 255, 255));
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        let mut pixmap = Pixmap::new(200, 200).unwrap();
        let frame = Frame {
            arrows: vec![ArrowTarget {
                from: Point::new(100, 20),
                at: Point::new(100, 160),
                tag: None,
            }],
            ..Frame::default()
        };
        painter.paint(&mut pixmap, &frame);
        let p = pixmap.pixel(100, 21).unwrap();
        assert_eq!((p.red(), p.green(), p.blue(), p.alpha()), (128, 0, 0, 128));
        assert_eq!(pixmap.pixel(100, 90).unwrap().alpha(), 0);
        assert_eq!(pixmap.pixel(100, 160).unwrap().alpha(), 0);
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

    const AREA: BoxRect = BoxRect::new(0, 0, 400, 300);
    const BUTTON: BoxRect = BoxRect::new(100, 100, 60, 20);

    /// A painter over `AREA`, one window, with these controls.
    fn among(controls: &[BoxRect]) -> Painter {
        let style = Style::default();
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        painter.set_space(AREA, &[], vec![(AREA, AREA)], controls.to_vec());
        painter
    }

    /// Where `painter` puts an `Auto` tag reading `text` on `control`, and
    /// whether placement added an arrow target.
    fn auto(painter: &mut Painter, control: BoxRect, text: &str) -> (BoxRect, bool) {
        let tag = auto_tag(control, text);
        let frame = Frame {
            tags: vec![tag],
            ..Frame::default()
        };
        let arranged = painter.arrange(&frame, (400.0, 300.0));
        let placed = &arranged.tags[0];
        let wh = painter.tags.size(text);
        let rect = whole_px(place(placed.at, placed.position, wh, (400.0, 300.0)).unwrap());
        (rect, !arranged.arrows.is_empty())
    }

    fn auto_tag(control: BoxRect, text: &str) -> Tag {
        Tag {
            at: Position::Auto.on(control),
            position: Position::Auto,
            control: Some(control),
            text: text.into(),
            typed: 0,
            verb: 0,
            target: Some(control.center()),
            object: None,
            kind: Kind::Plain,
        }
    }

    #[test]
    fn window_label_yields_to_controls_and_ordinary_tags_without_an_arrow() {
        let area = BoxRect::new(0, 0, 400, 300);
        let close = BoxRect::new(350, 5, 40, 25);
        let regular = auto_tag(close, "A");
        let window = Tag {
            at: Point::new(390, 10),
            position: Position::TopRight,
            control: Some(area),
            target: Some(Point::new(200, 150)),
            kind: Kind::Window,
            text: "W".into(),
            ..regular.clone()
        };
        let mut painter = among(&[close]);
        let frame = Frame {
            tags: vec![window, regular.clone()],
            ..Frame::default()
        };
        let placed = painter.arrange(&frame, (400.0, 300.0));
        let rect = |tag: &Tag| {
            whole_px(
                place(
                    tag.at,
                    tag.position,
                    painter.look(tag).size(&tag.text),
                    (400.0, 300.0),
                )
                .unwrap(),
            )
        };
        let (window_box, control_box) = (rect(&placed.tags[0]), rect(&placed.tags[1]));
        assert!(!placement::touches(window_box, control_box));
        assert!(!placement::touches(window_box, close));
        assert_eq!(window_box.intersect(area.inset(2)), Some(window_box));
        assert!(placed.arrows.iter().all(|a| a.tag != Some(0)));
        let only = painter.arrange(
            &Frame {
                tags: vec![regular],
                ..Frame::default()
            },
            (400.0, 300.0),
        );
        assert_eq!(placed.tags[1].at, only.tags[0].at);
    }

    /// The box `side` gives a tag reading `text` on `control`.
    fn on(side: Side, control: BoxRect, text: &str) -> BoxRect {
        let (at, position) = side.anchor(control);
        let wh = among(&[]).tags.size(text);
        whole_px(place(at, position, wh, (400.0, 300.0)).unwrap())
    }

    #[test]
    fn first_characters_share_the_fill_for_underlines_boxes_and_dots() {
        let style = Style::default();
        let letter = BoxRect::new(20, 20, 10, 16);
        let blue = Color::rgba(0, 0, 255, 255);
        for (mark, middle, bottom, below) in [
            (StartMark::Off, None, None, None),
            (StartMark::Underline, None, Some(blue), None),
            (StartMark::Box, Some(blue), Some(blue), None),
            (StartMark::Dot, None, None, Some(blue)),
        ] {
            let mut marks = Marks::new(&style);
            marks.starts.mark = mark;
            marks.starts.thickness = 6.0;
            marks.starts.look.background = blue;
            let mut painter = Painter::new(&style, &marks, &style, &Info::default(), 1.0).unwrap();
            let mut pixmap = Pixmap::new(60, 60).unwrap();
            let frame = Frame {
                starts: vec![letter],
                ..Frame::default()
            };
            painter.paint(&mut pixmap, &frame);
            let at = |x, y| {
                let p = pixmap.pixel(x, y).unwrap();
                (p.alpha() > 0).then(|| Color::rgba(p.red(), p.green(), p.blue(), p.alpha()))
            };
            assert_eq!(at(25, 26), middle, "{mark:?}: the letter's middle");
            assert_eq!(at(25, 35), bottom, "{mark:?}: its bottom row");
            assert_eq!(at(25, 40), below, "{mark:?}: centered below the character");
            assert_eq!(at(21, 40), None, "{mark:?}: outside the dot diameter");
        }
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn word_gradients_span_the_monitor_at_every_scale_and_stay_inside_marks() {
        for (table, mark, field) in [
            ("words.matches", "", "background"),
            ("words.matches.current", "", "background"),
            ("words.first_character", "mark = 'box'", "color"),
            (
                "words.first_character",
                "mark = 'dot'\nthickness = 6",
                "color",
            ),
            (
                "words.first_character",
                "mark = 'underline'\nthickness = 6",
                "color",
            ),
        ] {
            for direction in ["right", "left"] {
                for alpha in ["ff", "00"] {
                    let direction_field = if field == "color" {
                        "direction"
                    } else {
                        "background_direction"
                    };
                    let config = crate::config::Config::parse(
                        &format!("[{table}]\n{mark}\n{field} = '#ff0000'\n{field}_end = '#0000ff{alpha}'\n{direction_field} = '{direction}'\nradius = 0\npadding = [0, 0]"),
                        &crate::keys::Layout::named("us", ""),
                    ).unwrap();
                    let rects = [BoxRect::new(10, 20, 60, 20), BoxRect::new(130, 20, 60, 20)];
                    let mut frame = Frame::default();
                    if table == "words.first_character" {
                        frame.starts = rects.to_vec();
                    } else {
                        frame.highlights = rects
                            .iter()
                            .map(|&rect| Highlight {
                                rect,
                                current: table.ends_with("current"),
                            })
                            .collect();
                    }
                    let mut previous: Option<[[u8; 4]; 2]> = None;
                    for scale in [1.0, 1.5, 2.0] {
                        let mut painter = Painter::new(
                            &config.style,
                            &config.marks,
                            &config.input.style,
                            &config.info,
                            scale,
                        )
                        .unwrap();
                        let mut map =
                            Pixmap::new((200.0 * scale) as u32, (100.0 * scale) as u32).unwrap();
                        painter.paint(&mut map, &frame);
                        let sample = |x: f32, y: f32| {
                            map.pixel((x * scale) as u32, (y * scale) as u32).unwrap()
                        };
                        let (xs, y) = match config.marks.starts.mark {
                            StartMark::Dot => ([40.0, 160.0], 44.0),
                            StartMark::Underline => ([30.0, 150.0], 37.0),
                            _ => ([30.0, 150.0], 30.0),
                        };
                        let samples = xs.map(|x| sample(x, y));
                        let (start, end) = if direction == "right" {
                            (samples[0], samples[1])
                        } else {
                            (samples[1], samples[0])
                        };
                        assert!(
                            start.red() > end.red(),
                            "{table}, {direction}, {scale}: gradient spans both marks"
                        );
                        if alpha == "ff" {
                            assert!(end.blue() > start.blue());
                            assert_eq!((start.alpha(), end.alpha()), (255, 255));
                        } else {
                            assert!(
                                start.alpha() > end.alpha(),
                                "endpoint alpha is interpolated"
                            );
                        }
                        for (x, y) in [(5.0, 30.0), (100.0, 30.0), (30.0, 45.0), (195.0, 30.0)] {
                            assert_eq!(sample(x, y).alpha(), 0, "outside the marks");
                        }
                        let channels = samples.map(|p| [p.red(), p.green(), p.blue(), p.alpha()]);
                        if let Some(previous) = previous {
                            assert!(
                                channels
                                    .iter()
                                    .zip(previous)
                                    .all(|(a, b)| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 2)),
                                "same logical position across scales"
                            );
                        }
                        previous = Some(channels);
                    }
                }
            }
        }
    }

    #[test]
    fn auto_centers_along_the_control_edge_even_when_the_click_is_elsewhere() {
        let mut painter = among(&[BUTTON]);
        let mut tag = auto_tag(BUTTON, "EA");
        tag.target = Some(Point::new(155, 115));
        let frame = Frame {
            tags: vec![tag],
            ..Frame::default()
        };
        let arranged = painter.arrange(&frame, (400.0, 300.0));
        let rect = whole_px(
            place(
                arranged.tags[0].at,
                Position::TopLeft,
                painter.tags.size("EA"),
                (400.0, 300.0),
            )
            .unwrap(),
        );
        assert_eq!(rect.right(), BUTTON.x - BESIDE);
        assert_eq!(rect.center().y, BUTTON.center().y);
        assert_eq!(arranged.tags[0].target, Some(Point::new(155, 115)));
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn auto_arrows_touch_all_four_target_edges_without_entering_the_controls() {
        for (vertical, near) in [(true, true), (true, false), (false, true), (false, false)] {
            let controls: Vec<_> = (0..3)
                .map(|i| {
                    if vertical {
                        BoxRect::new(if near { 50 } else { 320 }, 60 + i * 60, 24, 20)
                    } else {
                        BoxRect::new(90 + i * 70, if near { 40 } else { 240 }, 24, 20)
                    }
                })
                .collect();
            let mut painter = among(&controls);
            painter.tags.style.arrow.outline_width = 0.0;
            let frame = Frame {
                tags: controls.iter().map(|&r| auto_tag(r, "EA")).collect(),
                ..Frame::default()
            };
            let arranged = painter.arrange(&frame, (400.0, 300.0));
            let mut pixmap = Pixmap::new(400, 300).unwrap();
            painter.paint(&mut pixmap, &frame);
            for (tag, control) in arranged.tags.iter().zip(controls) {
                let rect = whole_px(
                    place(
                        tag.at,
                        tag.position,
                        painter.tags.size(&tag.text),
                        (400.0, 300.0),
                    )
                    .unwrap(),
                );
                assert!(
                    rect.intersect(control).is_none(),
                    "hint must sit outside its own control"
                );
                let middle = control.center();
                let touch = match (vertical, near) {
                    (true, true) => Point::new(control.x - 1, middle.y),
                    (true, false) => Point::new(control.right(), middle.y),
                    (false, true) => Point::new(middle.x, control.y - 1),
                    (false, false) => Point::new(middle.x, control.bottom()),
                };
                assert!(
                    pixmap
                        .pixel(touch.x as u32, touch.y as u32)
                        .unwrap()
                        .alpha()
                        > 0,
                    "arrow touches {touch:?}"
                );
                assert_eq!(
                    pixmap
                        .pixel(middle.x as u32, middle.y as u32)
                        .unwrap()
                        .alpha(),
                    0,
                    "no arrow toward the target center"
                );
            }
        }
    }

    #[test]
    fn a_boxed_in_shared_row_stays_centered_inside_its_targets() {
        let controls = [
            BoxRect::new(60, 120, 24, 20),
            BoxRect::new(180, 120, 24, 20),
        ];
        let mut painter = among(&controls);
        // Other controls surround both centers, blocking every side.
        painter.controls.extend(controls.iter().flat_map(|r| {
            let p = r.center();
            [
                BoxRect::new(p.x - 45, p.y - 35, 44, 70),
                BoxRect::new(p.x + 1, p.y - 35, 44, 70),
                BoxRect::new(p.x - 45, p.y - 35, 90, 34),
                BoxRect::new(p.x - 45, p.y + 1, 90, 34),
            ]
        }));
        let frame = Frame {
            tags: controls.iter().map(|&r| auto_tag(r, "EA")).collect(),
            ..Frame::default()
        };
        let arranged = painter.arrange(&frame, (400.0, 300.0));
        for (tag, control) in arranged.tags.iter().zip(controls) {
            let rect = whole_px(
                place(
                    tag.at,
                    Position::TopLeft,
                    painter.tags.size("EA"),
                    (400.0, 300.0),
                )
                .unwrap(),
            );
            assert_eq!(rect.center(), control.center());
        }
    }

    #[test]
    fn shared_side_survives_a_collision_and_filtering() {
        let controls = [BoxRect::new(100, 80, 2, 12), BoxRect::new(100, 96, 2, 12)];
        let mut painter = among(&controls);
        let frame = Frame {
            tags: controls.iter().map(|&r| auto_tag(r, "EA")).collect(),
            ..Frame::default()
        };
        let arranged = painter.arrange(&frame, (400.0, 300.0));
        for (tag, control) in arranged.tags.iter().zip(controls) {
            assert_eq!(tag.at, on(Side::Left, control, "EA").origin());
        }
        let filtered = Frame {
            tags: vec![frame.tags[1].clone()],
            ..Frame::default()
        };
        assert_eq!(
            painter.arrange(&filtered, (400.0, 300.0)).tags[0].at,
            arranged.tags[1].at
        );
    }

    #[test]
    fn auto_tags_go_left_then_under_then_over_then_right_then_center() {
        let left = BoxRect::new(80, 109, 1, 2);
        let below = BoxRect::new(129, 133, 2, 1);
        let above = BoxRect::new(129, 85, 2, 1);
        // A list round the button is no neighbour.
        let around = BoxRect::new(50, 50, 200, 200);
        for (others, side) in [
            (vec![around], Side::Left),
            (vec![left], Side::Below),
            (vec![left, below], Side::Above),
            (vec![left, below, above], Side::Right),
            (
                vec![left, below, above, BoxRect::new(177, 109, 1, 2)],
                Side::Center,
            ),
        ] {
            let mut painter = among(&[[BUTTON].as_slice(), &others].concat());
            let (got, moved) = auto(&mut painter, BUTTON, "EA");
            assert_eq!(got, on(side, BUTTON, "EA"), "{side:?}");
            assert!(!moved, "{side:?}: its own spot, no arrow");
        }
        let (left, _) = auto(&mut among(&[BUTTON]), BUTTON, "EA");
        assert_eq!(
            left.right(),
            BUTTON.x - BESIDE,
            "ends outside the left edge"
        );
        assert_eq!(left.center().y, BUTTON.center().y, "middles in line");
    }

    #[test]
    fn auto_uses_right_when_left_bottom_and_top_are_blocked() {
        let controls = [
            BUTTON,
            BoxRect::new(80, 109, 1, 2),
            BoxRect::new(129, 133, 2, 1),
            BoxRect::new(129, 85, 2, 1),
        ];
        let (got, moved) = auto(&mut among(&controls), BUTTON, "EA");
        assert_eq!(got.x, BUTTON.right() + BESIDE);
        assert_eq!(got.center().y, BUTTON.center().y);
        assert!(!moved);
    }

    #[test]
    fn auto_aligned_targets_share_the_side_toward_the_screen_edge() {
        for (vertical, near) in [(true, true), (true, false), (false, true), (false, false)] {
            let controls: Vec<_> = (0..3)
                .map(|i| {
                    if vertical {
                        BoxRect::new(if near { 50 } else { 320 }, 60 + i * 60, 24, 20)
                    } else {
                        BoxRect::new(90 + i * 70, if near { 40 } else { 240 }, 24, 20)
                    }
                })
                .collect();
            let mut painter = among(&controls);
            let frame = Frame {
                tags: controls.iter().map(|&r| auto_tag(r, "EA")).collect(),
                ..Frame::default()
            };
            let arranged = painter.arrange(&frame, (400.0, 300.0));
            for (tag, control) in arranged.tags.iter().zip(&controls) {
                let wh = painter.tags.size(&tag.text);
                let rect = whole_px(place(tag.at, tag.position, wh, (400.0, 300.0)).unwrap());
                match (vertical, near) {
                    (true, true) => assert_eq!(rect.right(), control.x - BESIDE),
                    (true, false) => assert_eq!(rect.x, control.right() + BESIDE),
                    (false, true) => assert_eq!(rect.bottom(), control.y - BESIDE),
                    (false, false) => assert_eq!(rect.y, control.bottom() + BESIDE),
                }
                assert_eq!(tag.target, Some(control.center()));
            }
        }
    }

    #[test]
    fn auto_flips_a_line_only_when_two_members_are_blocked() {
        let column = [
            BoxRect::new(100, 60, 24, 20),
            BoxRect::new(100, 120, 24, 20),
            BoxRect::new(100, 180, 24, 20),
        ];
        let blockers = [BoxRect::new(65, 60, 34, 20), BoxRect::new(65, 120, 34, 20)];
        for blocked in 0..=2 {
            let mut painter = among(&[column.as_slice(), &blockers[..blocked]].concat());
            let frame = Frame {
                tags: column.iter().map(|&r| auto_tag(r, "EA")).collect(),
                ..Frame::default()
            };
            let arranged = painter.arrange(&frame, (400.0, 300.0));
            for (i, (tag, &control)) in arranged.tags.iter().zip(&column).enumerate() {
                let side = if blocked == 2 {
                    Side::Below
                } else {
                    Side::Left
                };
                assert_eq!(
                    tag.at,
                    on(side, control, "EA").origin(),
                    "{blocked} blocked, member {i}"
                );
            }
            let filtered = Frame {
                tags: vec![frame.tags[2].clone()],
                ..Frame::default()
            };
            assert_eq!(
                painter.arrange(&filtered, (400.0, 300.0)).tags[0].at,
                arranged.tags[2].at
            );
        }
    }

    #[test]
    fn auto_lines_allow_varied_sizes_but_do_not_join_windows_or_scattered_targets() {
        let column = [
            BoxRect::new(300, 60, 24, 20),
            BoxRect::new(302, 120, 40, 20),
            BoxRect::new(301, 180, 60, 20),
        ];
        let scattered = [
            BoxRect::new(280, 40, 24, 20),
            BoxRect::new(330, 110, 24, 20),
            BoxRect::new(250, 220, 24, 20),
        ];
        let windows = [BoxRect::new(90, 40, 24, 20), BoxRect::new(230, 40, 24, 20)];
        for (controls, regions, side) in [
            (column.as_slice(), vec![AREA], Side::Right),
            (scattered.as_slice(), vec![AREA], Side::Left),
            (
                windows.as_slice(),
                vec![BoxRect::new(0, 0, 200, 300), BoxRect::new(200, 0, 200, 300)],
                Side::Left,
            ),
        ] {
            let mut painter = among(controls);
            painter.set_space(
                AREA,
                &[],
                regions.into_iter().map(|r| (r, r)).collect(),
                controls.to_vec(),
            );
            let frame = Frame {
                tags: controls.iter().rev().map(|&r| auto_tag(r, "EA")).collect(),
                ..Frame::default()
            };
            let arranged = painter.arrange(&frame, (400.0, 300.0));
            for (tag, &control) in arranged.tags.iter().zip(controls.iter().rev()) {
                let (at, position) = side.anchor(control);
                let expected =
                    whole_px(place(at, position, painter.tags.size("EA"), (400.0, 300.0)).unwrap());
                assert_eq!(tag.at, expected.origin());
            }
        }
    }

    #[test]
    fn auto_checks_clearance_after_applying_the_offset() {
        let blocker = BoxRect::new(98, 108, 1, 4);
        let mut painter = among(&[BUTTON, blocker]);
        painter.tags.style.offset = [8, 0];
        let (got, _) = auto(&mut painter, BUTTON, "EA");
        assert_eq!(got.y, BUTTON.bottom() + BESIDE);
        assert_eq!(got.x, on(Side::Below, BUTTON, "EA").x + 8);
    }

    #[test]
    fn auto_top_bar_keeps_one_row_beside_the_surface() {
        let style = Style {
            size: 10.0,
            padding: [0.0, 2.0],
            ..Style::default()
        };
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        let screen = BoxRect::new(0, 0, 2560, 1440);
        let bar = BoxRect::new(0, 0, 2560, 38);
        let controls = [
            BoxRect::new(10, 4, 20, 30),
            BoxRect::new(30, 4, 20, 30),
            BoxRect::new(50, 4, 20, 30),
        ];
        painter.set_space(
            screen,
            &controls,
            vec![label_region(
                bar,
                crate::desktop::Kind::Layer { level: 1 },
                screen,
            )],
            controls.to_vec(),
        );
        let frame = Frame {
            tags: controls.iter().map(|&r| auto_tag(r, "EA")).collect(),
            ..Frame::default()
        };
        let arranged = painter.arrange(&frame, (2560.0, 1440.0));
        let wh = painter.tags.size("EA");
        for (tag, control) in arranged.tags.iter().zip(controls) {
            let rect = whole_px(place(tag.at, tag.position, wh, (2560.0, 1440.0)).unwrap());
            assert_eq!(
                rect.y,
                control.bottom() + BESIDE,
                "top bar hints must share the clear row below"
            );
        }
    }

    #[test]
    fn auto_slightly_overlapping_hit_boxes_still_share_a_row() {
        let controls = [
            BoxRect::new(90, 240, 31, 30),
            BoxRect::new(120, 240, 31, 30),
            BoxRect::new(150, 240, 31, 30),
        ];
        let mut painter = among(&controls);
        let frame = Frame {
            tags: controls.iter().map(|&r| auto_tag(r, "EA")).collect(),
            ..Frame::default()
        };
        let arranged = painter.arrange(&frame, (400.0, 300.0));
        assert!(
            arranged.tags.iter().all(|tag| tag.at.y == 273),
            "one-pixel hit-box overlaps must not split the row"
        );
    }

    #[test]
    fn coincident_accessible_objects_keep_separate_placements_when_filtered() {
        let config =
            crate::config::Config::parse("", &crate::keys::Layout::named("us", "")).unwrap();
        let targets: Vec<_> = ["/first", "/second"]
            .into_iter()
            .map(|path| crate::snapshot::Target {
                object: Some(crate::desktop::tree::ObjectId {
                    bus: ":1.2".into(),
                    path: path.into(),
                }),
                window: 0,
                rect: BUTTON,
                visible: BUTTON,
                hit: BUTTON.center(),
                role: "section".into(),
                name: String::new(),
                editable: false,
                clips: vec![],
                opens: vec![],
            })
            .collect();
        let frame = Frame {
            tags: targets
                .iter()
                .zip(["EA", "ET"])
                .map(|(t, text)| crate::pick::tag(t, text.into(), 0, &config))
                .collect(),
            ..Frame::default()
        };
        let mut painter = among(&[BUTTON]);
        let arranged = painter.arrange(&frame, (400.0, 300.0));
        assert_ne!(arranged.tags[0].at, arranged.tags[1].at);
        let filtered = Frame {
            tags: vec![frame.tags[1].clone()],
            ..Frame::default()
        };
        assert_eq!(
            painter.arrange(&filtered, (400.0, 300.0)).tags[0].at,
            arranged.tags[1].at
        );
    }

    #[test]
    fn auto_tags_stay_in_their_window_and_keep_their_side() {
        // At the window's left edge: no room on the left.
        let edge = BoxRect::new(3, 100, 34, 20);
        let (got, _) = auto(&mut among(&[edge]), edge, "EA");
        assert_eq!(got, on(Side::Below, edge, "EA"));
        // Room on the left for one letter, not two: the tag goes under, and
        // stays there as typing shortens it.
        let one = among(&[]).tags.size("A").0;
        let two = among(&[]).tags.size("EA").0;
        #[allow(clippy::cast_possible_truncation)]
        let gap = f32::midpoint(one, two) as i32 + BESIDE;
        let neighbour = BoxRect::new(0, 100, BUTTON.x - gap, 20);
        let mut painter = among(&[BUTTON, neighbour]);
        assert_eq!(
            auto(&mut painter, BUTTON, "EA").0,
            on(Side::Below, BUTTON, "EA")
        );
        assert_eq!(
            auto(&mut painter, BUTTON, "A").0,
            on(Side::Below, BUTTON, "A")
        );
        assert_eq!(
            auto(&mut among(&[BUTTON, neighbour]), BUTTON, "A").0,
            on(Side::Left, BUTTON, "A"),
            "fresh, one letter fits"
        );
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn production_painter_separates_sidebar_row_and_close() {
        let style = Style {
            size: 11.0,
            offset: [3, 3],
            ..Style::default()
        };
        let mut painter =
            Painter::new(&style, &Marks::new(&style), &style, &Info::default(), 1.0).unwrap();
        let mut pixmap = Pixmap::new(300, 600).unwrap();
        painter.set_space(
            BoxRect::new(0, 0, 300, 600),
            &[
                BoxRect::new(30, 180, 160, 15),
                BoxRect::new(246, 180, 26, 26),
            ],
            vec![],
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
                object: None,
                position: Position::BottomRight,
                control: None,
                text: text.into(),
                typed: 0,
                verb: 0,
                kind: Kind::Plain,
            })
            .collect(),
            ..Frame::default()
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
        assert_eq!(arranged.arrows.len(), 1);
        assert_eq!(arranged.arrows[0].at, Point::new(259, 193));
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
        // the row it would cover, and keeps its arrow; the other gets none.
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
        assert_eq!(moved.arrows.len(), 1, "still pointing back at its target");
        let kept = painter.arrange(&with_verb(&frame.tags[0]), (300.0, 600.0));
        assert_eq!(kept.arrows, [], "a tag never moved needs no arrow");
        let nudged = Frame {
            arrows: vec![ArrowTarget {
                from: frame.tags[1].at,
                at: Point::new(269, 203),
                tag: Some(1),
            }],
            ..frame.clone()
        };
        let nudged = painter.arrange(&nudged, (300.0, 600.0));
        assert_eq!(nudged.arrows.len(), 1);
        assert_eq!(nudged.arrows[0].at, Point::new(269, 203));
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
            &config.info,
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
        assert!(!snapshot.windows.is_empty());
        let config = std::env::var("WAYHINT_STYLE_FILE")
            .ok()
            .map_or_else(String::new, |p| std::fs::read_to_string(p).unwrap());
        let config =
            crate::config::Config::parse(&config, &crate::keys::Layout::named("us", "")).unwrap();
        let window = &snapshot.windows[0].window;
        let viewport = std::env::var("WAYHINT_LAYOUT_MONITOR")
            .ok()
            .map_or(window.rect, |s| {
                serde_json::from_str::<BoxRect>(&s).unwrap()
            });
        let origin = viewport.origin();
        let shift = Point::new(-origin.x, -origin.y);
        let area = viewport.offset(shift);
        let selector = crate::selection::Selector::new(
            snapshot.targets.len(),
            config.alphabet.len(),
            config.verbs.default_action(),
        );
        let tags = snapshot
            .targets
            .iter()
            .zip(selector.labels())
            .map(|(target, label)| {
                let text = label
                    .iter()
                    .map(|&i| config.style.case.of(config.alphabet.letters()[i].label))
                    .collect();
                let tag = crate::pick::tag(target, text, 0, &config);
                Tag {
                    at: tag.at.offset(shift),
                    control: tag.control.map(|r| r.offset(shift)),
                    target: tag.target.map(|p| p.offset(shift)),
                    ..tag
                }
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
            &config.info,
            1.0,
        )
        .unwrap();
        let obstacles: Vec<_> = snapshot.obstacles.iter().map(|r| r.offset(shift)).collect();
        let controls = snapshot
            .targets
            .iter()
            .map(|t| t.shown().offset(shift))
            .collect();
        painter.set_space(
            area,
            &obstacles,
            snapshot
                .windows
                .iter()
                .map(|w| label_region(w.window.rect.offset(shift), w.window.kind, area))
                .collect(),
            controls,
        );
        let size = (area.w as f32, area.h as f32);
        let boxes = |p: &Painter, f: &Frame| {
            f.tags
                .iter()
                .map(|t| {
                    let r = place(t.at, t.position, p.look(t).size(&t.text), size).unwrap();
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
        let filtered = Frame {
            tags: frame.tags.iter().step_by(2).cloned().collect(),
            ..Frame::default()
        };
        let filtered = painter.arrange(&filtered, size);
        for (index, (kept, original)) in filtered
            .tags
            .iter()
            .zip(placed.tags.iter().step_by(2))
            .enumerate()
        {
            assert_eq!(
                kept.at,
                original.at,
                "filtering moved hint {} with control {:?}",
                index * 2,
                kept.control
            );
        }
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
        // A tag moved off its own spot, `auto`'s side or its position, has
        // an arrow target.
        let moved = placed.arrows.len();
        let covered = placed
            .arrows
            .iter()
            .filter_map(|d| d.tag)
            .filter(|&i| painter.space.as_ref().unwrap().ink(after[i]) != 0)
            .count();
        assert_eq!(covered, 0, "a moved label covers captured content");
        let mut pixmap = Pixmap::new(area.w as u32, area.h as u32).unwrap();
        painter.paint(&mut pixmap, &frame);
        if let Ok(background) = std::env::var("WAYHINT_LAYOUT_BACKGROUND") {
            let mut picture = Pixmap::load_png(background).unwrap();
            assert_eq!(
                (picture.width(), picture.height()),
                (pixmap.width(), pixmap.height())
            );
            picture.draw_pixmap(
                0,
                0,
                pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
            picture.save_png(&output).unwrap();
        } else {
            pixmap.save_png(&output).unwrap();
        }
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
