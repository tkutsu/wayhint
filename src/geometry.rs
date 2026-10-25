//! Points and rectangles in global logical pixels.
//!
//! Every coordinate wayhint keeps is global and logical: the compositor's
//! layout space, where the top-left monitor usually starts at (0, 0) and a
//! scale-2 monitor still counts its width in logical pixels. AT-SPI boxes
//! arrive relative to their window; the raw tree keeps them that way, and
//! `Snapshot::build` moves them to global once, so nothing after the
//! snapshot has to ask which space a number is in. The overlay subtracts
//! its monitor's origin at the last moment, to draw.

use serde::{Deserialize, Serialize};

/// A point. `i32` because global coordinates can be negative (a monitor to
/// the left of the primary one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// The same point moved by `by`, a distance written as a point:
    /// `(14, 0)` is 14 px to the right.
    #[must_use]
    pub const fn offset(self, by: Self) -> Self {
        Self::new(self.x + by.x, self.y + by.y)
    }
}

/// An axis-aligned rectangle: top-left corner plus size.
///
/// A width or height of zero or less is an empty rectangle. AT-SPI reports
/// those for objects it has no box for, so they're legal values, and
/// `is_empty` is how code asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub const fn center(self) -> Point {
        Point::new(self.x + self.w / 2, self.y + self.h / 2)
    }

    pub const fn origin(self) -> Point {
        Point::new(self.x, self.y)
    }

    /// Whether `point` is inside: the left and top edges count, the right
    /// and bottom ones are the next rectangle's.
    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// The point inside nearest to `point`: `point` itself if it's in.
    /// Only for a rectangle that isn't empty, such as a monitor's:
    /// `i32::clamp` panics when its minimum is above its maximum.
    pub fn clamp(self, point: Point) -> Point {
        Point::new(
            point.x.clamp(self.x, self.right() - 1),
            point.y.clamp(self.y, self.bottom() - 1),
        )
    }

    /// The same rectangle `by` px smaller on every side, kept at least 1 px
    /// across so it can still [`clamp`](Self::clamp).
    #[must_use]
    pub fn inset(self, by: i32) -> Self {
        let (w, h) = ((self.w - 2 * by).max(1), (self.h - 2 * by).max(1));
        Self::new(self.x + by, self.y + by, w, h)
    }

    /// The same rectangle moved by `by`.
    #[must_use]
    pub const fn offset(self, by: Point) -> Self {
        Self::new(self.x + by.x, self.y + by.y, self.w, self.h)
    }

    /// The overlap of two rectangles, or `None` when they don't touch.
    pub fn intersect(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = Self::new(
            x,
            y,
            self.right().min(other.right()) - x,
            self.bottom().min(other.bottom()) - y,
        );
        (!r.is_empty()).then_some(r)
    }

    /// Area as `i64`: 2560 x 1440 fits an `i32`, but sums of areas may not.
    pub fn area(self) -> i64 {
        if self.is_empty() {
            0
        } else {
            i64::from(self.w) * i64::from(self.h)
        }
    }

    /// Intersection over union: 1.0 for the same box, 0.0 for boxes that
    /// don't overlap. The filter uses it to spot one control reported twice.
    #[allow(clippy::cast_precision_loss)] // pixel areas are far below 2^52
    pub fn iou(self, other: Self) -> f64 {
        let inter = self.intersect(other).map_or(0, Self::area);
        let union = self.area() + other.area() - inter;
        if union <= 0 {
            0.0
        } else {
            inter as f64 / union as f64
        }
    }

    /// Whether every edge is within `tolerance` px of `other`'s.
    pub const fn near(self, other: Self, tolerance: i32) -> bool {
        (self.x - other.x).abs() <= tolerance
            && (self.y - other.y).abs() <= tolerance
            && (self.right() - other.right()).abs() <= tolerance
            && (self.bottom() - other.bottom()).abs() <= tolerance
    }

    /// The smallest rectangle holding every one of `rects`, or `None` for
    /// none. Used for the extent of all monitors together.
    pub fn bounding(rects: impl IntoIterator<Item = Self>) -> Option<Self> {
        rects.into_iter().reduce(|a, b| {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            Self::new(
                x,
                y,
                a.right().max(b.right()) - x,
                a.bottom().max(b.bottom()) - y,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_overlapping_and_apart() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersect(Rect::new(5, 5, 10, 10)),
            Some(Rect::new(5, 5, 5, 5))
        );
        assert_eq!(
            a.intersect(Rect::new(10, 0, 5, 5)),
            None,
            "touching edges don't overlap"
        );
    }

    #[test]
    fn iou_of_same_and_disjoint() {
        let a = Rect::new(0, 0, 10, 10);
        assert!((a.iou(a) - 1.0).abs() < f64::EPSILON);
        assert!(a.iou(Rect::new(20, 20, 5, 5)).abs() < f64::EPSILON);
        assert!(
            a.iou(Rect::new(0, 0, 0, 0)).abs() < f64::EPSILON,
            "empty boxes never match"
        );
    }

    #[test]
    fn bounding_spans_monitors_left_of_zero() {
        let left = Rect::new(-1920, 0, 1920, 1080);
        let main = Rect::new(0, 0, 2560, 1440);
        assert_eq!(
            Rect::bounding([left, main]),
            Some(Rect::new(-1920, 0, 4480, 1440))
        );
        assert_eq!(Rect::bounding([]), None);
    }

    #[test]
    fn clamp_keeps_a_point_inside() {
        let r = Rect::new(0, 0, 100, 50);
        assert_eq!(r.clamp(Point::new(40, 20)), Point::new(40, 20));
        assert_eq!(r.clamp(Point::new(-5, 70)), Point::new(0, 49));
        assert_eq!(
            r.clamp(Point::new(100, 0)),
            Point::new(99, 0),
            "the right edge is the next rectangle's"
        );
    }

    #[test]
    fn inset_shrinks_every_side() {
        let r = Rect::new(10, 20, 100, 50);
        assert_eq!(r.inset(5), Rect::new(15, 25, 90, 40));
        assert_eq!(Rect::new(0, 0, 6, 6).inset(5), Rect::new(5, 5, 1, 1));
    }

    #[test]
    fn near_tolerates_small_moves() {
        let r = Rect::new(10, 10, 50, 20);
        assert!(r.near(Rect::new(12, 9, 50, 21), 2));
        assert!(!r.near(Rect::new(13, 10, 50, 20), 2));
    }
}
