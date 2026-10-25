//! The last look before clicking. Pure: a target from the snapshot and a
//! fresh read of the same object in, a [`Verdict`] out.
//!
//! Between drawing the tags and the key that picks one, the app may have
//! changed: a dialog opened, the list scrolled, the button went away. A
//! click at the old position would land on whatever is there now, so the
//! object is asked again and must still be the same thing in the same place.
//! The app can still change between this check and the click; the gap is
//! kept short and nothing is retried.

use std::fmt;

use crate::{
    desktop::tree::States,
    geometry::{Point, Rect},
    snapshot::Target,
};

/// How far a box may move and still count as the same place, in px.
const TOLERANCE: i32 = 2;

/// One object read again, window-relative as AT-SPI gives it.
#[derive(Debug, Clone)]
pub struct Fresh {
    pub role: String,
    pub name: String,
    pub states: States,
    pub bounds: Rect,
    /// The box of each scroll area in the target's `clips` now, in the
    /// same order; `None` for one that didn't answer.
    pub clips: Vec<Option<Rect>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same thing, same place: click. `editable` comes from the fresh read.
    Click { editable: bool },
    /// Don't click; rescan instead.
    Refuse(Why),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The object no longer answers.
    Gone,
    /// Its window is gone, or another window is over the spot now.
    Covered,
    Hidden,
    Disabled,
    /// Different role or name: the path now names something else.
    Replaced,
    Moved,
    /// The window shrank and the hit point is outside it now. AT-SPI can
    /// keep reporting the old box, and the click would land on whatever is
    /// next to the window.
    Outside,
    /// A scroll area above it no longer shows the hit point: a pane
    /// grew over it, say. GTK and Qt keep calling it "showing".
    Scrolled,
    /// A scroll area above it didn't answer, so what's in view is unknown.
    ScrollAreaUnread,
    /// A pixel target's window moved or changed size: the picture no
    /// longer says what's where.
    WindowChanged,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gone => "it's gone",
            Self::Covered => "its window is gone or covered",
            Self::Hidden => "it's hidden now",
            Self::Disabled => "it's disabled now",
            Self::Replaced => "something else is there now",
            Self::Moved => "it moved",
            Self::Outside => "its window no longer covers it",
            Self::Scrolled => "it's scrolled out of view",
            Self::ScrollAreaUnread => "a scroll area above it didn't answer",
            Self::WindowChanged => "its window moved or changed size",
        })
    }
}

/// `fresh` is `None` when the read failed; `window` is the window's box
/// now, in global coordinates, `None` if it's gone or something covers the
/// target (see `hyprland::uncovered`). Its origin puts the fresh box in
/// global coordinates too, so a moved window counts as moved.
pub fn verdict(target: &Target, fresh: Option<&Fresh>, window: Option<Rect>) -> Verdict {
    // `let ... else` names the happy path and leaves early otherwise.
    let Some(window) = window else {
        return Verdict::Refuse(Why::Covered);
    };
    let Some(fresh) = fresh else {
        return Verdict::Refuse(Why::Gone);
    };
    let now = fresh.bounds.offset(window.origin());
    let why = if !fresh.states.showing {
        Some(Why::Hidden)
    } else if !fresh.states.enabled && !fresh.states.sensitive {
        Some(Why::Disabled)
    } else if fresh.role != target.role || fresh.name != target.name {
        Some(Why::Replaced)
    } else if !now.near(target.rect, TOLERANCE) || !now.contains(target.hit) {
        // The hit point is the middle of the part that showed, which the
        // filter lets be 4 px wide: 2 px from an edge, as far as the
        // tolerance lets a box move. So the box must still hold it.
        Some(Why::Moved)
    } else if !window.contains(target.hit) {
        Some(Why::Outside)
    } else {
        clipped(&fresh.clips, window.origin(), target.hit)
    };
    match why {
        Some(why) => Verdict::Refuse(why),
        None => Verdict::Click {
            editable: fresh.states.editable,
        },
    }
}

/// The verdict on a target found in pixels, which has no object to ask:
/// its window is still where the picture was taken (`then`) and the same
/// size, with nothing over the spot (`now` is `None` otherwise, as for
/// [`verdict`]). What the app drew since can't be checked without another
/// picture. Nothing says whether it's a text field.
pub fn unread(then: Rect, now: Option<Rect>) -> Verdict {
    match now {
        None => Verdict::Refuse(Why::Covered),
        Some(now) if now != then => Verdict::Refuse(Why::WindowChanged),
        Some(_) => Verdict::Click { editable: false },
    }
}

/// Why a scroll area above the target hides `hit` now, if one does. The
/// filter's rule again (`filter::clip`): an empty box says nothing, one
/// that didn't answer hides everything. `origin` is the window's, which
/// puts the window-relative boxes in global coordinates.
fn clipped(clips: &[Option<Rect>], origin: Point, hit: Point) -> Option<Why> {
    clips.iter().find_map(|clip| match clip {
        None => Some(Why::ScrollAreaUnread),
        Some(area) if area.is_empty() => None,
        Some(area) => (!area.offset(origin).contains(hit)).then_some(Why::Scrolled),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::tree::ObjectId;

    /// The window: at (100, 50), big enough for the target at (150, 75).
    const WINDOW: Rect = Rect::new(100, 50, 400, 300);

    fn target() -> Target {
        Target {
            object: Some(ObjectId {
                bus: ":1.7".into(),
                path: "/ok".into(),
            }),
            window: 0,
            rect: Rect::new(110, 60, 80, 30),
            hit: Point::new(150, 75),
            role: "button".into(),
            name: "OK".into(),
            editable: false,
            clips: Vec::new(),
            opens: Vec::new(),
        }
    }

    #[test]
    fn a_pixel_target_clicks_while_its_window_stays_put() {
        assert_eq!(
            unread(WINDOW, Some(WINDOW)),
            Verdict::Click { editable: false }
        );
        assert_eq!(unread(WINDOW, None), Verdict::Refuse(Why::Covered));
        let taller = Rect { h: 310, ..WINDOW };
        assert_eq!(
            unread(WINDOW, Some(taller)),
            Verdict::Refuse(Why::WindowChanged)
        );
    }

    fn fresh() -> Fresh {
        let states = States {
            showing: true,
            visible: true,
            enabled: true,
            sensitive: true,
            ..States::default()
        };
        Fresh {
            role: "button".into(),
            name: "OK".into(),
            states,
            bounds: Rect::new(10, 10, 80, 30),
            clips: Vec::new(),
        }
    }

    #[test]
    fn same_thing_same_place_clicks() {
        assert_eq!(
            verdict(&target(), Some(&fresh()), Some(WINDOW)),
            Verdict::Click { editable: false }
        );
    }

    #[test]
    fn small_moves_are_tolerated_big_ones_refused() {
        let nudged = Fresh {
            bounds: Rect::new(12, 9, 80, 30),
            ..fresh()
        };
        assert!(matches!(
            verdict(&target(), Some(&nudged), Some(WINDOW)),
            Verdict::Click { .. }
        ));
        let scrolled = Fresh {
            bounds: Rect::new(10, 60, 80, 30),
            ..fresh()
        };
        assert_eq!(
            verdict(&target(), Some(&scrolled), Some(WINDOW)),
            Verdict::Refuse(Why::Moved)
        );
        // The window itself moved by 40 px: the target moved with it.
        assert_eq!(
            verdict(&target(), Some(&fresh()), Some(Rect { x: 140, ..WINDOW })),
            Verdict::Refuse(Why::Moved)
        );
    }

    #[test]
    fn a_small_move_that_leaves_the_hit_point_outside_refuses() {
        // A button clipped by the window's left edge: only x 100 to 104
        // showed, so the hit point is x 102, 2 px from the right edge.
        let clipped = Target {
            rect: Rect::new(4, 60, 100, 30),
            hit: Point::new(102, 75),
            ..target()
        };
        let nudged = Fresh {
            bounds: Rect::new(-98, 10, 100, 30),
            ..fresh()
        };
        assert_eq!(
            verdict(&clipped, Some(&nudged), Some(WINDOW)),
            Verdict::Refuse(Why::Moved)
        );
    }

    #[test]
    fn vanished_hidden_disabled_replaced_covered() {
        let t = target();
        assert_eq!(verdict(&t, None, Some(WINDOW)), Verdict::Refuse(Why::Gone));
        assert_eq!(
            verdict(&t, Some(&fresh()), None),
            Verdict::Refuse(Why::Covered)
        );
        let mut hidden = fresh();
        hidden.states.showing = false;
        assert_eq!(
            verdict(&t, Some(&hidden), Some(WINDOW)),
            Verdict::Refuse(Why::Hidden)
        );
        let mut off = fresh();
        off.states.enabled = false;
        off.states.sensitive = false;
        assert_eq!(
            verdict(&t, Some(&off), Some(WINDOW)),
            Verdict::Refuse(Why::Disabled)
        );
        let other = Fresh {
            name: "Cancel".into(),
            ..fresh()
        };
        assert_eq!(
            verdict(&t, Some(&other), Some(WINDOW)),
            Verdict::Refuse(Why::Replaced)
        );
    }

    #[test]
    fn a_shrunk_window_that_no_longer_covers_the_target_refuses() {
        // Same box as before (AT-SPI didn't update it), but the window is
        // now 40 px wide: the hit point at x 150 is past its right edge.
        let narrow = Rect { w: 40, ..WINDOW };
        assert_eq!(
            verdict(&target(), Some(&fresh()), Some(narrow)),
            Verdict::Refuse(Why::Outside)
        );
    }

    #[test]
    fn a_scroll_area_that_shrank_over_the_hit_point_refuses() {
        // The target's own box is unchanged and "showing", as GTK and Qt
        // report it; only its pane got shorter. The hit point is at window
        // y 25 (global 75).
        let pane = |h| Fresh {
            clips: vec![Some(Rect::new(0, 0, 400, h))],
            ..fresh()
        };
        assert!(matches!(
            verdict(&target(), Some(&pane(200)), Some(WINDOW)),
            Verdict::Click { .. }
        ));
        assert_eq!(
            verdict(&target(), Some(&pane(20)), Some(WINDOW)),
            Verdict::Refuse(Why::Scrolled)
        );
        let unread = Fresh {
            clips: vec![Some(Rect::default()), None],
            ..fresh()
        };
        assert_eq!(
            verdict(&target(), Some(&unread), Some(WINDOW)),
            Verdict::Refuse(Why::ScrollAreaUnread),
            "an empty box says nothing; no answer hides it"
        );
    }

    #[test]
    fn editable_comes_from_the_fresh_read() {
        let mut field = fresh();
        field.states.editable = true;
        assert_eq!(
            verdict(&target(), Some(&field), Some(WINDOW)),
            Verdict::Click { editable: true }
        );
    }
}
