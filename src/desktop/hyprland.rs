//! Hyprland over its IPC socket: which windows and layer surfaces a monitor
//! shows and how they stack, where the monitors are, where the cursor is,
//! and a notification when something goes wrong.
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
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{IsTerminal as _, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    desktop::{surfaces::Stack, tree::ObjectId},
    geometry::{Point, Rect},
};

/// External calls are bounded: a hung compositor must not hang wayhint.
const TIMEOUT: Duration = Duration::from_secs(1);

/// How long a notification stays: long enough to read one sentence.
const NOTIFY_MS: u32 = 6000;

/// Asks which layers are mapped. `j/layers` lists unmapped ones too (a
/// hidden drop-down terminal, at full size), and only the Lua API tells them
/// apart. The reply starts with a word because Hyprland answers an empty
/// one with "unknown request".
const MAPPED: &str = "repl local t = {} for _, l in ipairs(hl.get_layers()) do \
    if l.mapped then t[#t + 1] = l.address end end \
    return \"mapped \" .. table.concat(t, \" \")";

/// A surface that can get tags, in global logical pixels: a toplevel
/// window, a layer surface (a bar, a launcher) or a layer's popup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Hyprland's handle (`0x55...`), stable while the surface lives. A
    /// popup has none, so it gets one made from its frame (see
    /// `surfaces::popup`).
    pub address: String,
    pub pid: u32,
    /// The app's class; a layer's namespace (`quickshell:bar`).
    pub class: String,
    /// Empty for layers and popups: they have none.
    pub title: String,
    pub rect: Rect,
    /// Id of the monitor it's on, as in [`Monitor::id`].
    pub monitor: i64,
    /// Whether it has focus. The scan trusts the app's "active" frame only
    /// for the focused window (see `atspi::pick_frame`).
    pub focused: bool,
    /// `#[serde(default)]`: snapshots saved before layers came in are all
    /// of clients.
    #[serde(default)]
    pub kind: Kind,
    /// For a layer or popup: the frames in the accessibility tree that may
    /// be its own (see `surfaces::place`). Empty for a client, whose frame
    /// is found by title, and for a layer no frame could be matched to: it
    /// covers its box, but gets no tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<ObjectId>,
}

/// What a [`Window`] is. Hyprland knows the first two; popups are known only
/// from the accessibility tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A toplevel window.
    #[default]
    Client,
    /// A layer surface, at one of layer shell's levels: 0 background,
    /// 1 bottom, 2 top, 3 overlay.
    Layer { level: u8 },
    /// A popup of a layer surface: a bar's menu, a settings dropdown.
    Popup,
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
    /// Bits: 1 maximized, 2 fullscreen.
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
            kind: Kind::Client,
            frames: Vec::new(),
        }
    }
}

/// One output's layer surfaces, by level, each level bottom to top.
/// `j/layers` keys them by output name: `{"DP-1": {"levels": {"0": [...]}}}`.
#[derive(Deserialize)]
struct WireOutput {
    levels: BTreeMap<u8, Vec<WireLayer>>,
}

#[derive(Deserialize)]
struct WireLayer {
    address: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    namespace: String,
    /// -1 for a layer whose client is gone, while Hyprland still holds it.
    pid: i64,
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

/// `.socket.sock` takes requests, `.socket2.sock` sends events.
fn socket_path(socket: &str) -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is unset")?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE is unset: is this running under Hyprland?")?;
    Ok(PathBuf::from(runtime)
        .join("hypr")
        .join(instance)
        .join(socket))
}

/// Sends one command and returns the reply as Hyprland wrote it.
fn send(command: &str) -> Result<String> {
    let path = socket_path(".socket.sock")?;
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

/// Says `message` on stderr and, unless that's a terminal, in a
/// notification too: a run started from a keybind has nowhere else to say
/// what went wrong, since nobody reads its stderr. From a terminal, once is
/// enough.
pub fn warn(message: &str) {
    eprintln!("{message}");
    if !std::io::stderr().is_terminal() {
        let _ = notify(message);
    }
}

/// Shows `message` in Hyprland's own notification for a few seconds.
fn notify(message: &str) -> Result<()> {
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

/// What a monitor shows as far as Hyprland knows: everything but popups.
pub struct Shown {
    /// The monitor's clients and mapped layers, stacked (see [`shown_of`]),
    /// with no frames matched to the layers yet.
    pub stack: Stack,
    /// The monitor's box.
    pub screen: Rect,
    /// Every mapped layer on every monitor, bottom to top within each, for
    /// telling a layer's frame from a popup's.
    pub layers: Vec<Window>,
    /// Every monitor's top-left corner, by id: where Qt reports a layer.
    pub corners: HashMap<i64, Point>,
    /// Every process with a client, on any workspace.
    pub client_pids: BTreeSet<u32>,
}

/// What `monitor` shows, bottom to top: the order Hyprland's own hit test
/// reads it in, so the last surface holding a point is the one a click
/// there reaches. Wayhint's own layer (its tags) is left out.
pub fn shown(monitor: i64) -> Result<Shown> {
    let monitors: Vec<WireMonitor> = request("monitors")?;
    let clients: Vec<WireWindow> = request("clients")?;
    let outputs: HashMap<String, WireOutput> = request("layers")?;
    // Without an answer (a Hyprland without the Lua API), every layer it
    // lists counts as mapped: one that isn't covers some tags for nothing,
    // and has no frame to get tags of its own.
    let mapped = mapped_layers().ok();
    shown_of(
        monitors,
        monitor,
        clients,
        outputs,
        mapped.as_ref(),
        std::process::id(),
    )
}

fn mapped_layers() -> Result<BTreeSet<String>> {
    parse_mapped(&send(MAPPED)?)
}

/// The reply to [`MAPPED`]: `mapped 0x55... 0x55...`.
fn parse_mapped(reply: &str) -> Result<BTreeSet<String>> {
    let addresses = reply
        .strip_prefix("mapped")
        .with_context(|| format!("Hyprland mapped layers: unexpected reply {reply:?}"))?;
    Ok(addresses.split_whitespace().map(str::to_string).collect())
}

/// [`shown`] without the socket. The order, as Hyprland 0.56.2 hit-tests
/// (checked in a nested Hyprland, 2026-10-24, `mds/spike-quickshell/`):
///
/// 1. background and bottom layers
/// 2. clients (see [`stack_of`]), then top layers; top layers first when
///    a window is fullscreen, which covers them as it covers the rest
/// 3. overlay layers
///
/// Inside a level, the order `j/layers` lists them in: a layer mapped
/// later is above. Popups come later, from the accessibility tree, above
/// all of these. Their `popup_floor` is just past the top layers: a popup
/// of a bar is really there, under the overlays and a fullscreen window
/// (`surfaces::uncovered` says what that changes).
fn shown_of(
    monitors: Vec<WireMonitor>,
    monitor: i64,
    clients: Vec<WireWindow>,
    outputs: HashMap<String, WireOutput>,
    mapped: Option<&BTreeSet<String>>,
    own_pid: u32,
) -> Result<Shown> {
    let ids: HashMap<&str, i64> = monitors.iter().map(|m| (m.name.as_str(), m.id)).collect();
    let mut layers = Vec::new();
    for (output, wire) in outputs {
        let Some(&on) = ids.get(output.as_str()) else {
            continue;
        };
        for (level, list) in wire.levels {
            for layer in list {
                let mapped = mapped.is_none_or(|m| m.contains(&layer.address));
                let Some(pid) = u32::try_from(layer.pid).ok().filter(|&p| p != own_pid) else {
                    continue;
                };
                if mapped {
                    layers.push(Window {
                        address: layer.address,
                        pid,
                        class: layer.namespace,
                        title: String::new(),
                        rect: Rect::new(layer.x, layer.y, layer.w, layer.h),
                        monitor: on,
                        focused: false,
                        kind: Kind::Layer { level },
                        frames: Vec::new(),
                    });
                }
            }
        }
    }
    // `HashMap` order is random: put the monitors back in a fixed order,
    // so the same desktop gives the same list.
    layers.sort_by_key(|l| l.monitor);

    let corners = monitors
        .iter()
        .map(|m| (m.id, Point::new(m.x, m.y)))
        .collect();
    let shown = monitors
        .into_iter()
        .find(|m| m.id == monitor)
        .with_context(|| format!("monitor {monitor} is gone"))?;
    let client_pids = clients.iter().map(|c| c.pid).collect();
    let fullscreen = clients.iter().any(|c| {
        let ws = c.workspace.id;
        c.monitor == monitor
            && !c.hidden
            && c.fullscreen & 2 != 0
            && (ws == shown.active_workspace.id || ws == shown.special_workspace.id)
    });
    let at = |level: u8| {
        layers
            .iter()
            .filter(move |l| l.monitor == monitor && l.kind == Kind::Layer { level })
            .cloned()
    };
    let mut windows: Vec<Window> = at(0).chain(at(1)).collect();
    let clients = stack_of(&shown, clients);
    let popup_floor;
    if fullscreen {
        windows.extend(at(2));
        popup_floor = windows.len();
        windows.extend(clients);
    } else {
        windows.extend(clients);
        windows.extend(at(2));
        popup_floor = windows.len();
    }
    windows.extend(at(3));
    Ok(Shown {
        stack: Stack {
            windows,
            popup_floor,
            unplaced: Vec::new(),
        },
        screen: Monitor::from(shown).rect,
        layers,
        corners,
        client_pids,
    })
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

/// Hyprland's event socket: a line, `NAME>>DATA`, for each thing that
/// happens, for as long as the connection stays open. Non-blocking, for an
/// event loop to wait on.
pub fn events() -> Result<UnixStream> {
    let path = socket_path(".socket2.sock")?;
    let socket = UnixStream::connect(&path)
        .with_context(|| format!("Hyprland event socket {}", path.display()))?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// Whether the event `line` means the monitor named `monitor` may now show
/// another workspace.
///
/// `workspacev2>>ID,NAME` names no monitor: it's the focused one, which is
/// where a run puts its tags. A special workspace opening or closing does,
/// last: `activespecialv2>>ID,NAME,MONITOR`. Each change sends a v1 event
/// too, so only the v2 one counts: one change, one rescan.
pub fn changes_workspace(line: &str, monitor: &str) -> bool {
    match line.split_once(">>") {
        Some(("workspacev2", _)) => true,
        Some(("activespecialv2", data)) => data.rsplit(',').next() == Some(monitor),
        _ => false,
    }
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
    fn a_new_workspace_counts_once_and_a_special_one_only_on_its_monitor() {
        assert!(changes_workspace("workspacev2>>3,3", "DP-1"));
        assert!(!changes_workspace("workspace>>3", "DP-1"));
        assert!(changes_workspace(
            "activespecialv2>>-98,special:term,DP-1",
            "DP-1"
        ));
        assert!(changes_workspace("activespecialv2>>,,DP-1", "DP-1"));
        assert!(!changes_workspace(
            "activespecialv2>>-98,special:term,HDMI-A-1",
            "DP-1"
        ));
        assert!(!changes_workspace("openwindow>>55,3,kitty,t", "DP-1"));
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
    fn mapped_layers_parse_and_an_empty_list_is_still_an_answer() {
        let parse = |reply| {
            parse_mapped(reply)
                .ok()
                .map(|m| m.into_iter().collect::<Vec<_>>())
        };
        assert_eq!(
            parse("mapped 0x1 0x2"),
            Some(vec!["0x1".into(), "0x2".into()])
        );
        assert_eq!(parse("mapped "), Some(vec![]));
        assert_eq!(parse("unknown request"), None);
    }

    /// The user's desktop, as `j/layers` showed it on 2026-10-24, plus a
    /// hidden drop-down terminal, wayhint's own tag layer, a layer whose
    /// app is gone, and a top layer.
    const LAYERS: &str = r#"{"DP-1": {"levels": {
        "0": [{"address": "0xink", "x": 0, "y": 0, "w": 1, "h": 1, "namespace": "quickshell:ink", "pid": 50},
              {"address": "0xback", "x": 0, "y": 0, "w": 1920, "h": 1080, "namespace": "quickshell:backdrop", "pid": 50}],
        "1": [{"address": "0xbar", "x": 0, "y": 0, "w": 1920, "h": 38, "namespace": "quickshell:bar", "pid": 50}],
        "2": [{"address": "0xtop", "x": 100, "y": 300, "w": 300, "h": 200, "namespace": "osd", "pid": 60},
              {"address": "0xdead", "x": 100, "y": 100, "w": 300, "h": 200, "namespace": "osd", "pid": -1}],
        "3": [{"address": "0xkitty", "x": 427, "y": 360, "w": 1712, "h": 726, "namespace": "kitty-quick-access", "pid": 70},
              {"address": "0xtags", "x": 0, "y": 0, "w": 1920, "h": 1080, "namespace": "wayhint", "pid": 99},
              {"address": "0xsettings", "x": 0, "y": 0, "w": 1920, "h": 1080, "namespace": "quickshell:settings", "pid": 50}]
    }}}"#;

    fn shown_with(clients: Vec<WireWindow>) -> Shown {
        let outputs = serde_json::from_str(LAYERS).unwrap();
        let mapped = ["0xink", "0xback", "0xbar", "0xtop", "0xtags", "0xsettings"]
            .into_iter()
            .map(String::from)
            .collect();
        shown_of(vec![monitor(0)], 0, clients, outputs, Some(&mapped), 99).unwrap()
    }

    #[test]
    fn layers_stack_around_the_clients_by_level() {
        let shown = shown_with(vec![client("app")]);
        assert_eq!(
            addresses(&shown.stack.windows),
            ["0xink", "0xback", "0xbar", "app", "0xtop", "0xsettings"],
            "the hidden terminal, wayhint's tags and the dead layer are left out"
        );
        assert_eq!(
            shown.stack.popup_floor, 5,
            "popups are at least over the top layers"
        );
        assert_eq!(
            shown.layers.len(),
            6 - 1,
            "every mapped layer but no client"
        );
        assert_eq!(shown.stack.windows[2].kind, Kind::Layer { level: 1 });
        assert_eq!(shown.corners[&0], Point::new(0, 0));
        assert!(shown.client_pids.contains(&7));
    }

    #[test]
    fn a_fullscreen_window_is_over_the_top_layers_and_popups() {
        let shown = shown_with(vec![WireWindow {
            fullscreen: 2,
            ..client("full")
        }]);
        assert_eq!(
            addresses(&shown.stack.windows),
            ["0xink", "0xback", "0xbar", "0xtop", "full", "0xsettings"]
        );
        assert_eq!(
            shown.stack.popup_floor, 4,
            "a popup may be under the window"
        );
        let maximized = shown_with(vec![WireWindow {
            fullscreen: 1,
            ..client("max")
        }]);
        assert_eq!(
            addresses(&maximized.stack.windows)[3..5],
            ["max", "0xtop"],
            "a maximized one isn't"
        );
    }
}
