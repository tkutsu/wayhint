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
    /// Another window has focus now.
    Unfocused,
    Hidden,
    Disabled,
    /// Different role or name: the path now names something else.
    Replaced,
    Moved,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gone => "it's gone",
            Self::Unfocused => "its window lost focus",
            Self::Hidden => "it's hidden now",
            Self::Disabled => "it's disabled now",
            Self::Replaced => "something else is there now",
            Self::Moved => "it moved",
        })
    }
}

/// `fresh` is `None` when the read failed; `origin` is where the window is
/// now, to put the fresh box in global coordinates.
pub fn verdict(target: &Target, fresh: Option<&Fresh>, origin: Option<Point>) -> Verdict {
    // `let ... else` names the happy path and leaves early otherwise.
    let Some(origin) = origin else {
        return Verdict::Refuse(Why::Unfocused);
    };
    let Some(fresh) = fresh else {
        return Verdict::Refuse(Why::Gone);
    };
    let why = if !fresh.states.showing {
        Some(Why::Hidden)
    } else if !fresh.states.enabled && !fresh.states.sensitive {
        Some(Why::Disabled)
    } else if fresh.role != target.role || fresh.name != target.name {
        Some(Why::Replaced)
    } else if !fresh.bounds.offset(origin).near(target.rect, TOLERANCE) {
        Some(Why::Moved)
    } else {
        None
    };
    match why {
        Some(why) => Verdict::Refuse(why),
        None => Verdict::Click {
            editable: fresh.states.editable,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::tree::ObjectId;

    const ORIGIN: Point = Point::new(100, 50);

    fn target() -> Target {
        Target {
            object: ObjectId {
                bus: ":1.7".into(),
                path: "/ok".into(),
            },
            rect: Rect::new(110, 60, 80, 30),
            hit: Point::new(150, 75),
            role: "button".into(),
            name: "OK".into(),
            editable: false,
        }
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
        }
    }

    #[test]
    fn same_thing_same_place_clicks() {
        assert_eq!(
            verdict(&target(), Some(&fresh()), Some(ORIGIN)),
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
            verdict(&target(), Some(&nudged), Some(ORIGIN)),
            Verdict::Click { .. }
        ));
        let scrolled = Fresh {
            bounds: Rect::new(10, 60, 80, 30),
            ..fresh()
        };
        assert_eq!(
            verdict(&target(), Some(&scrolled), Some(ORIGIN)),
            Verdict::Refuse(Why::Moved)
        );
        // The window itself moved by 40 px: the target moved with it.
        assert_eq!(
            verdict(&target(), Some(&fresh()), Some(Point::new(140, 50))),
            Verdict::Refuse(Why::Moved)
        );
    }

    #[test]
    fn vanished_hidden_disabled_replaced_unfocused() {
        let t = target();
        assert_eq!(verdict(&t, None, Some(ORIGIN)), Verdict::Refuse(Why::Gone));
        assert_eq!(
            verdict(&t, Some(&fresh()), None),
            Verdict::Refuse(Why::Unfocused)
        );
        let mut hidden = fresh();
        hidden.states.showing = false;
        assert_eq!(
            verdict(&t, Some(&hidden), Some(ORIGIN)),
            Verdict::Refuse(Why::Hidden)
        );
        let mut off = fresh();
        off.states.enabled = false;
        off.states.sensitive = false;
        assert_eq!(
            verdict(&t, Some(&off), Some(ORIGIN)),
            Verdict::Refuse(Why::Disabled)
        );
        let other = Fresh {
            name: "Cancel".into(),
            ..fresh()
        };
        assert_eq!(
            verdict(&t, Some(&other), Some(ORIGIN)),
            Verdict::Refuse(Why::Replaced)
        );
    }

    #[test]
    fn editable_comes_from_the_fresh_read() {
        let mut field = fresh();
        field.states.editable = true;
        assert_eq!(
            verdict(&target(), Some(&field), Some(ORIGIN)),
            Verdict::Click { editable: true }
        );
    }
}
