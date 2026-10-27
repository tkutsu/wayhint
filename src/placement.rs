//! Move only colliding labels, preserving their target coordinates. Nearby
//! candidates must clear captured ink, controls, and every other label.

use crate::{
    geometry::{Point, Rect},
    space::Space,
};

#[derive(Clone, Copy)]
pub struct Label {
    pub rect: Rect,
    pub target: Option<Point>,
    pub bounds: Rect,
    /// Shared group placements reserve their targets without moving individually.
    pub fixed: bool,
}

/// Keep a window label near its corner while yielding to other targets.
pub fn window(rect: Rect, bounds: Rect, blocked: &[Rect]) -> Rect {
    let clear =
        |r: Rect| r.intersect(bounds) == Some(r) && blocked.iter().all(|&other| !touches(r, other));
    if clear(rect) {
        return rect;
    }
    let blocked: Vec<_> = blocked.iter().map(|&r| expand(r, 3)).collect();
    let space = Space::rectangles(bounds, &blocked);
    space
        .spot(bounds, (rect.w, rect.h), rect.center(), 1)
        .filter(|&r| space.ink(r) == 0)
        .unwrap_or(rect)
}

/// Deterministic, bounded search. Retaining an original position when no
/// legal gap exists is preferable to moving a hint onto text or controls.
pub fn resolve(labels: &[Label], space: &Space) -> Vec<Rect> {
    let mut placed: Vec<_> = labels.iter().map(|l| l.rect).collect();
    let mut gaps = std::collections::HashMap::new();
    // Preserve earlier labels first. If a later label is trapped, revisit the
    // remaining collisions in reverse so an earlier, movable label can yield.
    let order = (0..labels.len()).chain((0..labels.len()).rev());
    for (step, i) in order.enumerate() {
        let label = &labels[i];
        let end = if step < labels.len() { i } else { labels.len() };
        if label.fixed
            || label.target.is_none()
            || !placed[..end]
                .iter()
                .enumerate()
                .any(|(j, &r)| i != j && touches(placed[i], r))
        {
            continue;
        }
        if !*gaps
            .entry((label.bounds, label.rect.w, label.rect.h))
            .or_insert_with(|| {
                space.fits(expand(label.bounds, 2), label.rect.w + 4, label.rect.h + 4)
            })
        {
            continue;
        }
        let clear = |r: Rect| {
            r.intersect(label.bounds) == Some(r)
                && space.ink(expand(r, 2)) == 0
                && placed
                    .iter()
                    .enumerate()
                    .all(|(j, &other)| i == j || !touches(r, other))
                && labels
                    .iter()
                    .all(|other| other.target.is_none_or(|p| !expand(r, 5).contains(p)))
        };
        'search: for radius in (4..=256).step_by(4) {
            // Test the closest cardinal slots first, then each ring's edges.
            for (dx, dy) in [(0, -radius), (-radius, 0), (radius, 0), (0, radius)] {
                let r = label.rect.offset(Point::new(dx, dy));
                if clear(r) {
                    placed[i] = r;
                    break 'search;
                }
            }
            for shift in (-radius..=radius).step_by(4) {
                for (dx, dy) in [
                    (shift, -radius),
                    (-radius, shift),
                    (radius, shift),
                    (shift, radius),
                ] {
                    let r = label.rect.offset(Point::new(dx, dy));
                    if clear(r) {
                        placed[i] = r;
                        break 'search;
                    }
                }
            }
        }
    }
    placed
}

fn expand(r: Rect, by: i32) -> Rect {
    Rect::new(r.x - by, r.y - by, r.w + 2 * by, r.h + 2 * by)
}
/// Whether two labels are too close to read apart.
pub fn touches(a: Rect, b: Rect) -> bool {
    expand(a, 2).intersect(b).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trapped_later_label_can_be_cleared_by_moving_the_earlier_one() {
        let area = Rect::new(0, 0, 100, 80);
        let labels = [
            Label {
                fixed: false,
                rect: Rect::new(40, 20, 24, 19),
                target: Some(Point::new(90, 70)),
                bounds: area,
            },
            Label {
                fixed: false,
                rect: Rect::new(40, 30, 24, 19),
                target: Some(Point::new(55, 45)),
                bounds: Rect::new(40, 30, 24, 19),
            },
        ];
        let out = resolve(&labels, &Space::rectangles(area, &[]));
        assert!(!touches(out[0], out[1]));
        assert_eq!(out[1], labels[1].rect);
    }

    #[test]
    fn dense_blocked_layout_keeps_targets_and_reports_cost() {
        let area = Rect::new(0, 0, 800, 400);
        let labels: Vec<_> = (0..1000)
            .map(|i| Label {
                fixed: false,
                rect: Rect::new(i % 50 * 16, i / 50 * 16, 24, 19),
                target: Some(Point::new(i % 50 * 16 + 4, i / 50 * 16 + 4)),
                bounds: area,
            })
            .collect();
        let controls: Vec<_> = (0..1250)
            .map(|i| Rect::new(i % 50 * 16 + 3, i / 50 * 16 + 3, 9, 9))
            .collect();
        let space = Space::rectangles(area, &controls);
        let start = std::time::Instant::now();
        let out = resolve(&labels, &space);
        eprintln!(
            "dense blocked placement: {:.2}ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
        assert_eq!(out, labels.iter().map(|l| l.rect).collect::<Vec<_>>());
    }

    #[test]
    fn sidebar_row_and_close_keep_distinct_labels_and_targets() {
        let area = Rect::new(0, 0, 300, 600);
        let ink = [Rect::new(30, 180, 160, 15), Rect::new(246, 180, 26, 26)];
        let labels = [
            Label {
                fixed: false,
                rect: Rect::new(252, 192, 24, 19),
                target: Some(Point::new(148, 193)),
                bounds: area,
            },
            Label {
                fixed: false,
                rect: Rect::new(252, 190, 23, 19),
                target: Some(Point::new(259, 193)),
                bounds: area,
            },
            Label {
                fixed: false,
                rect: Rect::new(252, 220, 23, 19),
                target: Some(Point::new(259, 222)),
                bounds: area,
            },
        ];
        let space = Space::rectangles(area, &ink);
        let out = resolve(&labels, &space);
        assert_eq!(out[0], labels[0].rect);
        assert_ne!(out[1], labels[1].rect);
        assert_eq!(out[2], labels[2].rect);
        assert_eq!(space.ink(expand(out[1], 2)), 0);
        for (i, &a) in out.iter().enumerate() {
            for &b in &out[i + 1..] {
                assert!(!touches(a, b));
            }
        }
        assert_eq!(out, resolve(&labels, &space));
    }

    #[test]
    fn no_clear_gap_never_moves_a_label_onto_content() {
        let area = Rect::new(-200, -100, 100, 80);
        let labels = [Label {
            fixed: false,
            rect: Rect::new(-180, -80, 24, 19),
            target: Some(Point::new(-170, -70)),
            bounds: area,
        }; 2];
        assert_eq!(
            resolve(&labels, &Space::rectangles(area, &[area])),
            vec![labels[0].rect; 2]
        );
    }
}
