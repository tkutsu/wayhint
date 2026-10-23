//! Hyprland over its IPC socket: which windows a monitor shows and how they
//! stack, where the monitors are, where the cursor is, and a notification
//! when a run fails.
//!
//! The protocol is one request per connection: connect to
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`, write
//! `j/<command>` (`j/` asks for JSON), read until Hyprland closes it. Plain
//! functions, no trait: there is one compositor until a second one exists to
//! show what's actually shared.
//!
//! Hyprland's JSON goes into private `Wire*` structs shaped exactly like it,
//! then into wayhint's own [`Window`] and [`Monitor`]. If Hyprland renames a
//! field, only a `Wire` struct changes.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::geometry::{Point, Rect};

/// External calls are bounded: a hung compositor must not hang wayhint.
const TIMEOUT: Duration = Duration::from_secs(1);

/// How long a notification stays: long enough to read one sentence.
const NOTIFY_MS: u32 = 6000;

/// A toplevel window, in global logical pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Hyprland's handle (`0x55...`), stable while the window lives.
    pub address: String,
    pub pid: u32,
    pub class: String,
    pub title: String,
    pub rect: Rect,
    /// Id of the monitor it's on, as in [`Monitor::id`].
    pub monitor: i64,
    /// Whether it has focus. The scan trusts the app's "active" frame only
    /// for the focused window (see `atspi::pick_frame`).
    pub focused: bool,
}

/// A monitor's place in the global layout, in logical pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub id: i64,
    /// The output name (`HDMI-A-1`), which is also the `wl_output` name.
    pub name: String,
    pub rect: Rect,
    pub scale: f64,
    /// Whether it has the keyboard focus: a run starts on this one.
    pub focused: bool,
}

#[derive(Deserialize)]
#[allow(clippy::struct_excessive_bools)] // shaped like Hyprland's JSON, flags and all
struct WireWindow {
    address: String,
    pid: u32,
    class: String,
    title: String,
    at: [i32; 2],
    size: [i32; 2],
    monitor: i64,
    workspace: WireWorkspace,
    floating: bool,
    /// Shown on every workspace, above everything.
    pinned: bool,
    /// 0 for none, 1 maximized, 2 fullscreen.
    fullscreen: u8,
    /// A window group's tabs other than the shown one.
    hidden: bool,
    /// False when clicks go through it, as Hyprland's own hit test skips
    /// it: a window under a fullscreen one, or one whose input is blocked.
    #[serde(rename = "acceptsInput")]
    accepts_input: bool,
    /// On a workspace with a fullscreen (or maximized) window: whether
    /// this one may show above it. Hyprland clears it on the workspace's
    /// other windows when one goes fullscreen and sets it again on a
    /// floating window that's raised, focused or newly opened. It means
    /// nothing on other workspaces.
    #[serde(rename = "allowedOverFullscreen")]
    allowed_over_fullscreen: bool,
    /// 0 for the focused window, then 1, 2... back through focus history.
    #[serde(rename = "focusHistoryID")]
    focus_history_id: i64,
}

#[derive(Deserialize)]
struct WireWorkspace {
    /// 0 for none: a monitor that shows no special workspace.
    id: i64,
}

impl From<WireWindow> for Window {
    fn from(w: WireWindow) -> Self {
        Self {
            address: w.address,
            pid: w.pid,
            class: w.class,
            title: w.title,
            rect: Rect::new(w.at[0], w.at[1], w.size[0], w.size[1]),
            monitor: w.monitor,
            focused: w.focus_history_id == 0,
        }
    }
}

#[derive(Deserialize)]
struct WireMonitor {
    id: i64,
    name: String,
    x: i32,
    y: i32,
    /// Physical pixels, before rotation: divide by the scale for logical
    /// ones.
    width: i32,
    height: i32,
    scale: f64,
    /// `wl_output`'s transform: 0 to 3 turn by 90° steps, 4 to 7 flip and
    /// turn. The odd ones stand the output on its side.
    transform: u8,
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    active_workspace: WireWorkspace,
    #[serde(rename = "specialWorkspace")]
    special_workspace: WireWorkspace,
}

impl From<WireMonitor> for Monitor {
    #[allow(clippy::cast_possible_truncation)] // monitor sizes are far inside i32
    fn from(m: WireMonitor) -> Self {
        let logical = |px: i32| (f64::from(px) / m.scale).round() as i32;
        // Hyprland swaps the sides of a turned output before scaling
        // (`CMonitor::m_transformedSize`); IPC sends them unswapped.
        let (w, h) = if m.transform % 2 == 1 {
            (m.height, m.width)
        } else {
            (m.width, m.height)
        };
        Self {
            id: m.id,
            rect: Rect::new(m.x, m.y, logical(w), logical(h)),
            name: m.name,
            scale: m.scale,
            focused: m.focused,
        }
    }
}

#[derive(Deserialize)]
struct WireCursor {
    x: f64,
    y: f64,
}

fn socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is unset")?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE is unset: is this running under Hyprland?")?;
    Ok(PathBuf::from(runtime)
        .join("hypr")
        .join(instance)
        .join(".socket.sock"))
}

/// Sends one command and returns the reply as Hyprland wrote it.
fn send(command: &str) -> Result<String> {
    let path = socket_path()?;
    let mut socket = UnixStream::connect(&path)
        .with_context(|| format!("Hyprland socket {}", path.display()))?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))?;
    socket.write_all(command.as_bytes())?;
    let mut reply = String::new();
    socket
        .read_to_string(&mut reply)
        .with_context(|| format!("Hyprland {command}"))?;
    Ok(reply)
}

/// Sends one command and parses the JSON reply into any type that can be
/// deserialized. The caller picks the type: `request::<WireCursor>("cursorpos")`.
/// The `j/` prefix asks Hyprland for JSON.
fn request<T: DeserializeOwned>(command: &str) -> Result<T> {
    let reply = send(&format!("j/{command}"))?;
    serde_json::from_str(&reply)
        .with_context(|| format!("Hyprland {command}: unexpected reply {reply:?}"))
}

/// Shows `message` in Hyprland's own notification for a few seconds. A run
/// started from a keybind has nowhere else to say why it stopped: nobody
/// reads its stderr.
pub fn notify(message: &str) -> Result<()> {
    // `notify ICON MS COLOR TEXT`: icon 0 is Hyprland's warning sign, color
    // 0 its default. The text runs to the end of the line, so it's one line.
    let text = message.replace('\n', " ");
    let reply = send(&format!("notify 0 {NOTIFY_MS} 0 {text}"))?;
    if reply.trim() != "ok" {
        bail!("Hyprland notify: {reply}");
    }
    Ok(())
}

/// The focused window, or `None` when nothing has focus (an empty
/// workspace). Hyprland answers `{}` then, which no `WireWindow` matches, so
/// the reply is read as loose JSON first.
pub fn focused_window() -> Result<Option<Window>> {
    let value: serde_json::Value = request("activewindow")?;
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(None);
    }
    let wire: WireWindow = serde_json::from_value(value).context("Hyprland activewindow")?;
    Ok(Some(wire.into()))
}

/// The windows `monitor` shows, bottom to top: the order Hyprland's own hit
/// test reads them in, so the last window holding a point is the one a
/// click there reaches.
pub fn stack(monitor: i64) -> Result<Vec<Window>> {
    let monitors: Vec<WireMonitor> = request("monitors")?;
    let monitor = monitors
        .iter()
        .find(|m| m.id == monitor)
        .with_context(|| format!("monitor {monitor} is gone"))?;
    let clients: Vec<WireWindow> = request("clients")?;
    Ok(stack_of(monitor, clients))
}

/// Where the window at `address` is now, if nothing stacked above it in
/// `stack` covers `point`; `None` if it's gone, or covered there.
pub fn uncovered(stack: &[Window], address: &str, point: Point) -> Option<Rect> {
    let at = stack.iter().position(|w| w.address == address)?;
    let covered = stack[at + 1..].iter().any(|w| w.rect.contains(point));
    (!covered).then_some(stack[at].rect)
}

/// [`stack`] without the socket. The layers, as Hyprland 0.56.2 stacks
/// them (`ViewHitTester::windowAt`): the active workspace's tiled windows,
/// which never overlap, then its floating ones, the special workspace's in
/// the same two layers above both, and pinned windows on top. `clients`
/// lists windows in stacking order (a raised window moves to the end), so
/// a stable sort by layer keeps the order inside one.
///
/// A fullscreen or maximized window hides the rest of its workspace, except
/// the floating windows Hyprland lets over it (`allowedOverFullscreen`).
/// A window clicks go through (`acceptsInput` false) is left out
/// everywhere: it takes no click, so it neither gets tags nor covers any.
/// Both rules are the ones `ViewHitTester::windowAt` reads.
fn stack_of(monitor: &WireMonitor, clients: Vec<WireWindow>) -> Vec<Window> {
    let (active, special) = (monitor.active_workspace.id, monitor.special_workspace.id);
    let fullscreen: Vec<i64> = clients
        .iter()
        .filter(|c| c.fullscreen != 0 && !c.hidden)
        .map(|c| c.workspace.id)
        .collect();
    let layer = |c: &WireWindow| -> Option<u8> {
        if c.hidden || !c.accepts_input {
            return None;
        }
        if c.pinned {
            return (c.monitor == monitor.id).then_some(4);
        }
        let on_special = special != 0 && c.workspace.id == special;
        if !on_special && c.workspace.id != active {
            return None;
        }
        let floats = c.floating && c.fullscreen == 0;
        if fullscreen.contains(&c.workspace.id)
            && c.fullscreen == 0
            && !(floats && c.allowed_over_fullscreen)
        {
            return None;
        }
        // 0 tiled, 1 floating; 2 and 3 the same on the special workspace.
        Some(u8::from(on_special) * 2 + u8::from(floats))
    };
    let mut layered: Vec<(u8, WireWindow)> = clients
        .into_iter()
        .filter_map(|c| layer(&c).map(|l| (l, c)))
        .collect();
    layered.sort_by_key(|(l, _)| *l);
    layered.into_iter().map(|(_, c)| c.into()).collect()
}

/// Any mapped window by its address (`dump --address`, for scanning a
/// window without focusing it).
pub fn window_by_address(address: &str) -> Result<Option<Window>> {
    let clients: Vec<WireWindow> = request("clients")?;
    Ok(clients
        .into_iter()
        .find(|c| c.address == address)
        .map(Window::from))
}

pub fn monitors() -> Result<Vec<Monitor>> {
    let wire: Vec<WireMonitor> = request("monitors")?;
    Ok(wire.into_iter().map(Monitor::from).collect())
}

#[allow(clippy::cast_possible_truncation)] // cursor coordinates are far inside i32
pub fn cursor() -> Result<Point> {
    let c: WireCursor = request("cursorpos")?;
    Ok(Point::new(c.x.round() as i32, c.y.round() as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_window_becomes_a_rect() {
        let json = r#"{"address":"0x1","pid":7,"class":"kitty","title":"t","at":[8,38],"size":[1262,1394],
                       "monitor":0,"workspace":{"id":1,"name":"1"},"floating":false,"pinned":false,
                       "fullscreen":0,"hidden":false,"acceptsInput":true,"allowedOverFullscreen":true,
                       "focusHistoryID":0}"#;
        let window: Window = serde_json::from_str::<WireWindow>(json).unwrap().into();
        assert_eq!(window.rect, Rect::new(8, 38, 1262, 1394));
        assert!(window.focused);
    }

    #[test]
    fn monitor_size_is_logical() {
        let json = r#"{"id":1,"name":"DP-1","x":2560,"y":0,"width":3840,"height":2160,"scale":2.0,
                "transform":0,"focused":true,"activeWorkspace":{"id":1,"name":"1"},
                "specialWorkspace":{"id":0,"name":""}}"#;
        let monitor: Monitor = serde_json::from_str::<WireMonitor>(json).unwrap().into();
        assert_eq!(monitor.rect, Rect::new(2560, 0, 1920, 1080));
    }

    #[test]
    fn a_turned_monitor_stands_on_its_side() {
        for (transform, size) in [(1, (1080, 1920)), (2, (1920, 1080)), (7, (1080, 1920))] {
            let turned: Monitor = WireMonitor {
                transform,
                ..monitor(0)
            }
            .into();
            assert_eq!(
                (turned.rect.w, turned.rect.h),
                size,
                "transform {transform}"
            );
        }
    }

    /// Monitor 0, showing workspace 1 and, if `special` isn't 0, that one.
    fn monitor(special: i64) -> WireMonitor {
        WireMonitor {
            id: 0,
            name: "DP-1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            transform: 0,
            focused: true,
            active_workspace: WireWorkspace { id: 1 },
            special_workspace: WireWorkspace { id: special },
        }
    }

    /// A tiled window on workspace 1, named by its address.
    fn client(address: &str) -> WireWindow {
        WireWindow {
            address: address.into(),
            pid: 7,
            class: "app".into(),
            title: address.into(),
            at: [0, 0],
            size: [100, 100],
            monitor: 0,
            workspace: WireWorkspace { id: 1 },
            floating: false,
            pinned: false,
            fullscreen: 0,
            hidden: false,
            accepts_input: true,
            allowed_over_fullscreen: true,
            focus_history_id: 1,
        }
    }

    fn addresses(stack: &[Window]) -> Vec<&str> {
        stack.iter().map(|w| w.address.as_str()).collect()
    }

    #[test]
    fn floating_over_tiled_special_over_both_pinned_on_top() {
        let clients = vec![
            WireWindow {
                pinned: true,
                floating: true,
                workspace: WireWorkspace { id: 4 },
                ..client("pinned")
            },
            WireWindow {
                floating: true,
                ..client("float-low")
            },
            WireWindow {
                workspace: WireWorkspace { id: -98 },
                ..client("scratch")
            },
            client("tiled"),
            WireWindow {
                workspace: WireWorkspace { id: 2 },
                ..client("elsewhere")
            },
            WireWindow {
                hidden: true,
                ..client("tab")
            },
            WireWindow {
                floating: true,
                ..client("float-high")
            },
        ];
        assert_eq!(
            addresses(&stack_of(&monitor(-98), clients)),
            ["tiled", "float-low", "float-high", "scratch", "pinned"]
        );
    }

    #[test]
    fn a_hidden_special_workspace_is_left_out() {
        let clients = vec![
            client("tiled"),
            WireWindow {
                workspace: WireWorkspace { id: -98 },
                ..client("scratch")
            },
        ];
        assert_eq!(addresses(&stack_of(&monitor(0), clients)), ["tiled"]);
    }

    #[test]
    fn fullscreen_shows_itself_and_the_floating_windows_let_over_it() {
        // Hyprland clears the flag on the tiled window as `full` goes
        // fullscreen; focus doesn't decide anything.
        let full = || WireWindow {
            fullscreen: 2,
            ..client("full")
        };
        let float = |address, over| WireWindow {
            floating: true,
            allowed_over_fullscreen: over,
            ..client(address)
        };
        let other = WireWindow {
            allowed_over_fullscreen: false,
            ..client("other")
        };
        let focused = WireWindow {
            focus_history_id: 0,
            ..float("focused", true)
        };
        let clients = vec![
            float("dialog", true),
            full(),
            other,
            float("under", false),
            focused,
        ];
        assert_eq!(
            addresses(&stack_of(&monitor(0), clients)),
            ["full", "dialog", "focused"],
            "an unfocused dialog over it still covers it"
        );
    }

    #[test]
    fn a_window_clicks_go_through_is_left_out() {
        let clients = vec![
            client("tiled"),
            WireWindow {
                floating: true,
                accepts_input: false,
                ..client("blocked")
            },
        ];
        assert_eq!(addresses(&stack_of(&monitor(0), clients)), ["tiled"]);
    }

    #[test]
    fn a_window_above_covers_only_its_own_box() {
        let window = |address: &str, rect| Window {
            address: address.into(),
            pid: 7,
            class: "app".into(),
            title: String::new(),
            rect,
            monitor: 0,
            focused: false,
        };
        let stack = [
            window("tiled", Rect::new(0, 0, 1000, 1000)),
            window("float", Rect::new(100, 100, 200, 200)),
        ];
        assert_eq!(
            uncovered(&stack, "tiled", Point::new(50, 50)),
            Some(stack[0].rect)
        );
        assert_eq!(uncovered(&stack, "tiled", Point::new(150, 150)), None);
        assert_eq!(
            uncovered(&stack, "float", Point::new(150, 150)),
            Some(stack[1].rect)
        );
        assert_eq!(uncovered(&stack, "gone", Point::new(50, 50)), None);
    }
}
