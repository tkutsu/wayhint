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
//! That's the truth only for layers in the corner of the monitor at the
//! global origin, which is where a bar and full-screen overlays are. So:
//!
//! - A frame is a layer's only when its box is exactly the layer's. A layer
//!   elsewhere (a dock with a margin) gets no tags; it still covers its box.
//! - Leftover frames are placed as popups only when every one of the app's
//!   layers matched, on the monitor at the origin. Otherwise they're
//!   reported, not guessed at.
//!
//! The rule behind both: a missing tag is fine, a click that lands on
//! something other than the tagged control is not.

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
///
/// Above a client or layer is whatever comes after it. Above a popup is
/// anything from [`Stack::popup_floor`] up, other popups too: their order
/// among themselves isn't known either.
pub fn uncovered(stack: &Stack, address: &str, point: Point) -> Option<Rect> {
    let at = stack.windows.iter().position(|w| w.address == address)?;
    let from = if stack.windows[at].kind == Kind::Popup {
        stack.popup_floor
    } else {
        at + 1
    };
    let covered = stack
        .windows
        .iter()
        .enumerate()
        .skip(from)
        .any(|(i, w)| i != at && w.rect.contains(point));
    (!covered).then_some(stack.windows[at].rect)
}

/// Sorts one app's `frames` out against its mapped `layers` on every
/// monitor (bottom to top within each). `origin` is the monitor at the
/// global origin.
///
/// Frames whose box is a layer's go to that layer. Two layers with one box
/// (a full-screen backdrop and a full-screen launcher) get all of that
/// box's frames given to the upper one, and the scan reads whichever of
/// them has controls, if just one does.
pub fn place(layers: &[Window], frames: &[Frame], origin: Option<i64>) -> Placed {
    let mut placed = Placed::default();
    let mut left: Vec<&Frame> = frames.iter().collect();
    let mut all_matched = true;
    // Upper layers first, so the upper of two with one box takes the frames.
    for (i, layer) in layers.iter().enumerate().rev() {
        let mine: Vec<ObjectId> = left
            .iter()
            .filter(|f| f.rect == layer.rect)
            .map(|f| f.object.clone())
            .collect();
        left.retain(|f| f.rect != layer.rect);
        if !mine.is_empty() {
            placed.layers.push((layer.address.clone(), mine));
        } else if !layers[i + 1..].iter().any(|l| l.rect == layer.rect) {
            // No frame and no upper twin that took its frames: this one's
            // frame is somewhere Qt put it, not where the layer is.
            all_matched = false;
        }
    }
    let trusted = all_matched && layers.iter().all(|l| Some(l.monitor) == origin);
    if trusted {
        placed.popups = left.into_iter().cloned().collect();
    } else {
        placed.unplaced = left.len();
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
        let placed = place(&layers, &frames, Some(0));
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
        let placed = place(&layers, &frames, Some(0));
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
        // Qt reports a layer at 100,300 as being at 0,0. That frame and a
        // popup's look the same, so neither gets placed.
        let layers = [
            layer("bar", 1, BAR),
            layer("dock", 2, Rect::new(100, 300, 300, 200)),
        ];
        let frames = [
            frame("/bar", BAR),
            frame("/dock", Rect::new(0, 0, 300, 200)),
            frame("/audio", AUDIO),
        ];
        let placed = place(&layers, &frames, Some(0));
        assert_eq!(placed.layers.len(), 1);
        assert_eq!(placed.popups, []);
        assert_eq!(placed.unplaced, 2);
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
        let placed = place(&layers, &frames, Some(0));
        assert_eq!(placed.layers.len(), 1);
        assert_eq!((placed.popups.len(), placed.unplaced), (0, 1));
    }
}
