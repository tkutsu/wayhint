//! Which surfaces a monitor shows, how they stack, and which accessibility
//! frame belongs to which layer. Pure: Hyprland's layers and an app's frames
//! in, a [`Stack`] and a [`Placed`] out.
//!
//! Hyprland knows where every layer surface is (a bar, a launcher), and the
//! accessibility tree knows what's on it, but neither names the other: the
//! frames of a Quickshell or other layer-shell app are unnamed. And a popup
//! (a bar's menu) isn't in Hyprland's lists at all; only its frame tells
//! where it is. So frames are joined to layers by their boxes, and the
//! frames left over are popups.
//!
//! The boxes come from Qt, which never learns where a layer is: it reports
//! every layer at its screen's corner, and a popup relative to its layer
//! (checked in a nested Hyprland, 2026-10-24, `mds/spike-quickshell/`).
//! That's the truth only for layers in their monitor's corner, which is
//! where a bar and full-screen overlays are, and for popups only on the
//! monitor at the global origin. So:
//!
//! - A frame is a layer's only when its box is the one Qt reports for the
//!   layer, the layer really is there, and no layer elsewhere reports that
//!   box too. A layer elsewhere (a dock with a margin) gets no tags; it
//!   still covers its box.
//! - Leftover frames are placed as popups only when every one of the app's
//!   layers is in the corner of the monitor at the origin, with its frame.
//!   Otherwise they may be anywhere, so they cover the whole monitor
//!   ([`unknown`]).
//!
//! The rule behind both: a missing tag is fine, a click that lands on
//! something other than the tagged control is not.

use std::collections::HashMap;

use crate::{
    desktop::{Kind, Window, tree::ObjectId},
    geometry::{Point, Rect},
};

/// What a monitor shows, bottom to top: the clients and layers Hyprland
/// stacks (see `hyprland::shown_of`), then the popups.
#[derive(Debug, Clone, Default)]
pub struct Stack {
    pub windows: Vec<Window>,
    /// The lowest index a popup may really be at. Popups sit at the end,
    /// above everything, but a popup's level is its layer's: a bar's popup
    /// is under the overlays (and under a fullscreen window), a settings
    /// dropdown above them. The tree doesn't say which layer is a popup's,
    /// so it's taken to be as high as it can be for what it covers, and
    /// as low as it can be for its own tags.
    pub popup_floor: usize,
    /// Why some frames got no place: one line per app.
    pub unplaced: Vec<String>,
}

/// A top-level frame of an app, with its box as the app reports it in
/// screen coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub object: ObjectId,
    pub rect: Rect,
}

/// One app's frames, sorted out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Placed {
    /// For a layer's address, the frames that may be its own.
    pub layers: Vec<(String, Vec<ObjectId>)>,
    pub popups: Vec<Frame>,
    /// Frames that are neither, as far as can be told.
    pub unplaced: usize,
}

/// Where the surface at `address` is now, if nothing that may be above it
/// covers `point`; `None` if it's gone, or covered there.
pub fn uncovered(stack: &Stack, address: &str, point: Point) -> Option<Rect> {
    let rect = stack.windows.iter().find(|w| w.address == address)?.rect;
    (!above(stack, address).any(|w| w.rect.contains(point))).then_some(rect)
}

/// The surfaces that may be above the one at `address`; none if it's gone.
///
/// Above a client or layer is whatever comes after it. Above a popup is
/// anything from [`Stack::popup_floor`] up, other popups too: their order
/// among themselves isn't known either.
pub fn above<'a>(stack: &'a Stack, address: &str) -> impl Iterator<Item = &'a Window> {
    let at = stack.windows.iter().position(|w| w.address == address);
    let from = match at {
        Some(at) if stack.windows[at].kind == Kind::Popup => stack.popup_floor,
        Some(at) => at + 1,
        None => stack.windows.len(),
    };
    stack
        .windows
        .iter()
        .enumerate()
        .skip(from)
        .filter(move |&(i, _)| Some(i) != at)
        .map(|(_, w)| w)
}

/// Sorts one app's `frames` out against its mapped `layers` on every
/// monitor (bottom to top within each). `corners` are the monitors'
/// top-left corners, by id.
///
/// A layer's frame has the box Qt reports for it: its monitor's corner and
/// its size. Frames with that box go to the layer if it really is there.
/// Two layers with one box (a full-screen backdrop and a full-screen
/// launcher) get all of that box's frames given to the upper one, and the
/// scan reads whichever of them has controls, if just one does. If one of
/// the layers that report a box is elsewhere, which frame is whose can't be
/// told, and none of them get its frames.
pub fn place(layers: &[Window], frames: &[Frame], corners: &HashMap<i64, Point>) -> Placed {
    let reported = |l: &Window| {
        corners
            .get(&l.monitor)
            .map(|c| Rect::new(c.x, c.y, l.rect.w, l.rect.h))
    };
    let mut placed = Placed::default();
    let mut left: Vec<&Frame> = frames.iter().collect();
    // A popup's box is right only if its layer is in the corner of the
    // monitor at the origin, and it can't be told which layer is its.
    let mut trusted = layers.iter().all(|l| l.rect.origin() == Point::new(0, 0));
    // Upper layers first, so the upper of two with one box takes the frames.
    for (i, layer) in layers.iter().enumerate().rev() {
        let Some(at) = reported(layer) else {
            // Its monitor is gone, so where Qt has its frame isn't known.
            trusted = false;
            continue;
        };
        if layers[i + 1..].iter().any(|l| reported(l) == Some(at)) {
            continue; // the upper one with this box took the frames
        }
        let mine: Vec<ObjectId> = left
            .iter()
            .filter(|f| f.rect == at)
            .map(|f| f.object.clone())
            .collect();
        left.retain(|f| f.rect != at);
        let owners: Vec<&Window> = layers.iter().filter(|l| reported(l) == Some(at)).collect();
        if mine.is_empty() {
            // Its frame is somewhere else than where Qt should report it.
            trusted = false;
        } else if owners.iter().all(|l| l.rect == at) {
            placed.layers.push((layer.address.clone(), mine));
        } else {
            // Each layer has one frame: any more are something else, a
            // popup maybe, somewhere unknown.
            placed.unplaced += mine.len().saturating_sub(owners.len());
        }
    }
    if trusted {
        placed.popups = left.into_iter().cloned().collect();
    } else {
        placed.unplaced += left.len();
    }
    placed
}

/// A popup as a surface: its frame is all there is to it, so its address
/// is made from the frame's, which stays the same while it's open.
pub fn popup(frame: &Frame, pid: u32, monitor: i64) -> Window {
    Window {
        address: format!("popup {}{}", frame.object.bus, frame.object.path),
        pid,
        class: "popup".into(),
        title: String::new(),
        rect: frame.rect,
        monitor,
        focused: false,
        kind: Kind::Popup,
        frames: vec![frame.object.clone()],
    }
}

/// Stands for `pid`'s popups that couldn't be placed, or read at all: they
/// may be anywhere, on any monitor, so on the one hinted they cover all of
/// `screen`. It has no frame, so it gets no tags.
pub fn unknown(pid: u32, monitor: i64, screen: Rect) -> Window {
    Window {
        address: format!("unplaced {pid}"),
        pid,
        class: "unplaced".into(),
        title: String::new(),
        rect: screen,
        monitor,
        focused: false,
        kind: Kind::Popup,
        frames: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface of `kind` named `address`.
    fn surface(address: &str, kind: Kind, rect: Rect) -> Window {
        Window {
            address: address.into(),
            pid: 7,
            class: "shell".into(),
            title: String::new(),
            rect,
            monitor: 0,
            focused: false,
            kind,
            frames: Vec::new(),
        }
    }

    fn layer(address: &str, level: u8, rect: Rect) -> Window {
        surface(address, Kind::Layer { level }, rect)
    }

    fn frame(path: &str, rect: Rect) -> Frame {
        Frame {
            object: ObjectId {
                bus: ":1.9".into(),
                path: path.into(),
            },
            rect,
        }
    }

    const SCREEN: Rect = Rect::new(0, 0, 2560, 1440);
    const BAR: Rect = Rect::new(0, 0, 2560, 38);
    const AUDIO: Rect = Rect::new(1073, 34, 324, 186);

    fn paths(ids: &[ObjectId]) -> Vec<&str> {
        ids.iter().map(|o| o.path.as_str()).collect()
    }

    /// Monitor 0 at the origin, monitor 1 to its right.
    fn corners() -> HashMap<i64, Point> {
        HashMap::from([(0, Point::new(0, 0)), (1, Point::new(2560, 0))])
    }

    #[test]
    fn a_window_above_covers_only_its_own_box() {
        let stack = Stack {
            windows: vec![
                surface("tiled", Kind::Client, Rect::new(0, 0, 1000, 1000)),
                surface("float", Kind::Client, Rect::new(100, 100, 200, 200)),
            ],
            popup_floor: 2,
            ..Stack::default()
        };
        let tiled = stack.windows[0].rect;
        assert_eq!(uncovered(&stack, "tiled", Point::new(50, 50)), Some(tiled));
        assert_eq!(uncovered(&stack, "tiled", Point::new(150, 150)), None);
        assert!(uncovered(&stack, "float", Point::new(150, 150)).is_some());
        assert_eq!(uncovered(&stack, "gone", Point::new(50, 50)), None);
    }

    /// The user's desktop, with Audio open over a window that overlaps the
    /// bar: bar at the bottom level, the window, the popup.
    fn with_popup(overlay: Option<Rect>) -> Stack {
        let mut windows = vec![
            layer("bar", 1, BAR),
            surface("app", Kind::Client, Rect::new(0, 20, 2560, 1420)),
        ];
        let popup_floor = windows.len();
        windows.extend(overlay.map(|r| layer("settings", 3, r)));
        windows.push(surface("audio", Kind::Popup, AUDIO));
        Stack {
            windows,
            popup_floor,
            ..Stack::default()
        }
    }

    #[test]
    fn a_client_over_a_bottom_bar_hides_its_tags() {
        let stack = with_popup(None);
        assert_eq!(uncovered(&stack, "bar", Point::new(10, 30)), None);
        assert!(uncovered(&stack, "bar", Point::new(10, 10)).is_some());
    }

    #[test]
    fn a_popup_gets_tags_and_covers_what_is_under_it() {
        let stack = with_popup(None);
        let inside = Point::new(1200, 100);
        assert_eq!(uncovered(&stack, "audio", inside), Some(AUDIO));
        assert_eq!(uncovered(&stack, "app", inside), None);
        assert_eq!(uncovered(&stack, "bar", Point::new(1200, 36)), None);
    }

    #[test]
    fn an_overlay_hides_app_tags_and_may_be_over_a_popup() {
        // Settings open with Audio: the popup may be the bar's (under
        // Settings) or Settings' own (over it). It covers Settings' tags
        // and gets none of its own.
        let stack = with_popup(Some(SCREEN));
        let inside = Point::new(1200, 100);
        assert_eq!(uncovered(&stack, "app", Point::new(10, 500)), None);
        assert!(uncovered(&stack, "settings", Point::new(10, 500)).is_some());
        assert_eq!(uncovered(&stack, "settings", inside), None);
        assert_eq!(uncovered(&stack, "audio", inside), None);
    }

    #[test]
    fn two_popups_cover_each_other() {
        let mut stack = with_popup(None);
        stack
            .windows
            .push(surface("menu", Kind::Popup, Rect::new(1100, 50, 100, 100)));
        let both = Point::new(1150, 100);
        assert_eq!(uncovered(&stack, "audio", both), None);
        assert_eq!(uncovered(&stack, "menu", both), None);
        assert!(uncovered(&stack, "audio", Point::new(1300, 100)).is_some());
    }

    #[test]
    fn under_a_fullscreen_window_a_popup_gets_no_tags() {
        // `popup_floor` is below the window, as `shown_of` sets it then.
        let stack = Stack {
            windows: vec![
                layer("bar", 2, BAR),
                surface("full", Kind::Client, SCREEN),
                surface("audio", Kind::Popup, AUDIO),
            ],
            popup_floor: 1,
            ..Stack::default()
        };
        assert_eq!(uncovered(&stack, "audio", Point::new(1200, 100)), None);
        assert_eq!(uncovered(&stack, "full", Point::new(1200, 100)), None);
    }

    #[test]
    fn frames_go_to_the_layer_with_their_box_and_the_rest_are_popups() {
        let layers = [
            layer("ink", 0, Rect::new(0, 0, 1, 1)),
            layer("backdrop", 0, SCREEN),
            layer("bar", 1, BAR),
        ];
        let frames = [
            frame("/bar", BAR),
            frame("/ink", Rect::new(0, 0, 1, 1)),
            frame("/backdrop", SCREEN),
            frame("/audio", AUDIO),
        ];
        let placed = place(&layers, &frames, &corners());
        let mut matched: Vec<(&str, Vec<&str>)> = placed
            .layers
            .iter()
            .map(|(a, f)| (a.as_str(), paths(f)))
            .collect();
        matched.sort_unstable();
        assert_eq!(
            matched,
            [
                ("backdrop", vec!["/backdrop"]),
                ("bar", vec!["/bar"]),
                ("ink", vec!["/ink"])
            ]
        );
        assert_eq!(placed.popups, [frame("/audio", AUDIO)]);
        assert_eq!(placed.unplaced, 0);
    }

    #[test]
    fn two_layers_with_one_box_give_its_frames_to_the_upper() {
        let layers = [layer("backdrop", 0, SCREEN), layer("settings", 3, SCREEN)];
        let frames = [frame("/a", SCREEN), frame("/b", SCREEN)];
        let placed = place(&layers, &frames, &corners());
        assert_eq!(placed.layers.len(), 1);
        let (address, ids) = &placed.layers[0];
        assert_eq!(
            (address.as_str(), paths(ids)),
            ("settings", vec!["/a", "/b"])
        );
        assert_eq!(placed.popups, []);
    }

    #[test]
    fn a_layer_off_the_corner_matches_nothing_and_popups_stay_unplaced() {
        // Qt reports a layer at 100,300 as being at 0,0: that frame is the
        // dock's, but it's not where the dock is. The popup's place can't be
        // trusted then.
        let layers = [
            layer("bar", 1, BAR),
            layer("dock", 2, Rect::new(100, 300, 300, 200)),
        ];
        let frames = [
            frame("/bar", BAR),
            frame("/dock", Rect::new(0, 0, 300, 200)),
            frame("/audio", AUDIO),
        ];
        let placed = place(&layers, &frames, &corners());
        assert_eq!(placed.layers.len(), 1);
        assert_eq!(placed.popups, []);
        assert_eq!(placed.unplaced, 1);
    }

    #[test]
    fn a_layer_elsewhere_with_a_corner_layers_size_leaves_both_frames_alone() {
        // The audit's case: both frames say 0,0,300,200, and the one with
        // controls may be the layer at 100,300. Neither layer gets a frame.
        let corner = Rect::new(0, 0, 300, 200);
        let layers = [
            layer("decor", 2, corner),
            layer("panel", 2, Rect::new(100, 300, 300, 200)),
        ];
        let frames = [frame("/a", corner), frame("/b", corner)];
        let placed = place(&layers, &frames, &corners());
        assert_eq!(placed, Placed::default(), "two frames, both the layers'");
        let more = [
            frame("/a", corner),
            frame("/b", corner),
            frame("/c", corner),
        ];
        assert_eq!(place(&layers, &more, &corners()).unplaced, 1);
    }

    #[test]
    fn an_unknown_popup_covers_the_whole_monitor() {
        // The audit's case: Audio open on a bar that isn't at 0,0, over a
        // window under the bar.
        let screen = Rect::new(2560, 0, 1920, 1080);
        let bar = Rect::new(2560, 0, 1920, 38);
        let layers = [Window {
            monitor: 1,
            ..layer("bar", 2, bar)
        }];
        let frames = [
            frame("/bar", bar),
            frame("/audio", Rect::new(2670, 38, 300, 200)),
        ];
        let placed = place(&layers, &frames, &corners());
        assert_eq!(placed.unplaced, 1);
        let stack = Stack {
            windows: vec![
                surface("app", Kind::Client, Rect::new(2560, 38, 1920, 1042)),
                layers[0].clone(),
                unknown(7, 1, screen),
            ],
            popup_floor: 2,
            ..Stack::default()
        };
        assert_eq!(uncovered(&stack, "app", Point::new(2700, 100)), None);
        assert_eq!(uncovered(&stack, "bar", Point::new(2600, 10)), None);
    }

    #[test]
    fn popups_stay_unplaced_off_the_origin_monitor() {
        // Qt adds the screen's corner to a popup's place, sometimes twice.
        let layers = [Window {
            monitor: 1,
            ..layer("bar", 1, Rect::new(2560, 0, 1920, 38))
        }];
        let frames = [
            frame("/bar", Rect::new(2560, 0, 1920, 38)),
            frame("/audio", Rect::new(5120, 34, 324, 186)),
        ];
        let placed = place(&layers, &frames, &corners());
        assert_eq!(placed.layers.len(), 1);
        assert_eq!((placed.popups.len(), placed.unplaced), (0, 1));
    }
}
