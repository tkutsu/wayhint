//! Hyprland over its IPC socket: which window is focused, where the monitors
//! are, where the cursor is, which keyboard layout comes first.
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

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::geometry::{Point, Rect};

/// External calls are bounded: a hung compositor must not hang wayhint.
const TIMEOUT: Duration = Duration::from_secs(1);

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
}

/// A monitor's place in the global layout, in logical pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub id: i64,
    /// The output name (`HDMI-A-1`), which is also the `wl_output` name.
    pub name: String,
    pub rect: Rect,
    pub scale: f64,
}

#[derive(Deserialize)]
struct WireWindow {
    address: String,
    pid: u32,
    class: String,
    title: String,
    at: [i32; 2],
    size: [i32; 2],
    monitor: i64,
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
        }
    }
}

#[derive(Deserialize)]
struct WireMonitor {
    id: i64,
    name: String,
    x: i32,
    y: i32,
    /// Physical pixels: divide by the scale for logical ones.
    width: i32,
    height: i32,
    scale: f64,
}

impl From<WireMonitor> for Monitor {
    #[allow(clippy::cast_possible_truncation)] // monitor sizes are far inside i32
    fn from(m: WireMonitor) -> Self {
        let logical = |px: i32| (f64::from(px) / m.scale).round() as i32;
        Self {
            id: m.id,
            rect: Rect::new(m.x, m.y, logical(m.width), logical(m.height)),
            name: m.name,
            scale: m.scale,
        }
    }
}

#[derive(Deserialize)]
struct WireOption {
    #[serde(rename = "str")]
    value: String,
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

/// Sends one command and parses the JSON reply into any type that can be
/// deserialized. The caller picks the type: `request::<WireCursor>("cursorpos")`.
fn request<T: DeserializeOwned>(command: &str) -> Result<T> {
    let path = socket_path()?;
    let mut socket = UnixStream::connect(&path)
        .with_context(|| format!("Hyprland socket {}", path.display()))?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))?;
    socket.write_all(format!("j/{command}").as_bytes())?;
    let mut reply = String::new();
    socket
        .read_to_string(&mut reply)
        .with_context(|| format!("Hyprland {command}"))?;
    serde_json::from_str(&reply)
        .with_context(|| format!("Hyprland {command}: unexpected reply {reply:?}"))
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

/// The first entry of `input:kb_layout`, with the first `input:kb_variant`
/// in xkb's `layout(variant)` form: `us,gr` gives `us`.
pub fn keyboard_layout() -> Result<String> {
    let layouts: WireOption = request("getoption input:kb_layout")?;
    let variants: WireOption = request("getoption input:kb_variant")?;
    Ok(first_layout(&layouts.value, &variants.value))
}

/// The pure part of [`keyboard_layout`], so it can be tested.
fn first_layout(layouts: &str, variants: &str) -> String {
    let first = |list: &str| list.split(',').next().unwrap_or("").trim().to_string();
    let (layout, variant) = (first(layouts), first(variants));
    let layout = if layout.is_empty() {
        "us".to_string()
    } else {
        layout
    };
    if variant.is_empty() {
        layout
    } else {
        format!("{layout}({variant})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_layout_takes_the_first_of_each() {
        assert_eq!(first_layout("us,gr", ""), "us");
        assert_eq!(first_layout("us,gr", "colemak,"), "us(colemak)");
        assert_eq!(first_layout("", ""), "us", "Hyprland's own default");
    }

    #[test]
    fn wire_window_becomes_a_rect() {
        let json = r#"{"address":"0x1","pid":7,"class":"kitty","title":"t","at":[8,38],"size":[1262,1394],
                       "monitor":0,"floating":false}"#;
        let window: Window = serde_json::from_str::<WireWindow>(json).unwrap().into();
        assert_eq!(window.rect, Rect::new(8, 38, 1262, 1394));
    }

    #[test]
    fn monitor_size_is_logical() {
        let json =
            r#"{"id":1,"name":"DP-1","x":2560,"y":0,"width":3840,"height":2160,"scale":2.0}"#;
        let monitor: Monitor = serde_json::from_str::<WireMonitor>(json).unwrap().into();
        assert_eq!(monitor.rect, Rect::new(2560, 0, 1920, 1080));
    }
}
