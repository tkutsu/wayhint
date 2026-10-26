//! `wayhint doctor`: checks the setup a run needs and says what's missing.
//!
//! Each check prints one line: `ok`, `warn` (a part of wayhint won't
//! work) or `fail` (no run will). Nothing here takes the keyboard or draws:
//! it asks Hyprland, the Wayland registry, the accessibility bus,
//! fontconfig and kitty, and stops there.

use std::{fs, path::Path};

use anyhow::{Result, bail};
use smithay_client_toolkit::reexports::client::{Connection, globals::registry_queue_init};

use crate::{
    config::{self, Config},
    desktop::{self, Desktop, Monitor},
    overlay,
    pointer::Quiet,
    style::Info,
};

/// How a check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Ok,
    Warn,
    Fail,
}

/// One line of the report.
struct Check {
    mark: Mark,
    what: String,
    detail: String,
}

impl Check {
    fn new(mark: Mark, what: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            mark,
            what: what.into(),
            detail: detail.into(),
        }
    }
}

/// What each Wayland protocol is for, and whether a run can do without it.
const PROTOCOLS: [(&str, Mark, &str); 7] = [
    (
        "zwlr_layer_shell_v1",
        Mark::Fail,
        "the overlay the tags are on",
    ),
    ("xdg_wm_base", Mark::Fail, "the popup the tags are drawn on"),
    (
        "zwlr_virtual_pointer_manager_v1",
        Mark::Fail,
        "clicking, scrolling and moving the pointer",
    ),
    (
        "zwp_input_method_manager_v2",
        Mark::Warn,
        "reading keys without taking focus; without it an open menu closes when the tags show",
    ),
    (
        "zwp_virtual_keyboard_manager_v1",
        Mark::Warn,
        "Shift, Ctrl and Alt clicks, and keys let go during a grab reaching the app",
    ),
    (
        "hyprland_toplevel_export_manager_v1",
        Mark::Warn,
        "pixel tags in windows with no accessibility tree",
    ),
    (
        "wp_viewporter",
        Mark::Warn,
        "sharp tags on a monitor with a fractional scale",
    ),
];

/// Runs every check on `config` (the file loaded, or why it didn't),
/// prints the report, and fails if a run can't work.
pub fn run(config: Result<Config>, explicit: Option<&Path>) -> Result<()> {
    let mut checks = Vec::new();
    let config = match config {
        Ok(config) => {
            let path = explicit
                .map(Path::to_path_buf)
                .or_else(config::default_path);
            let detail = match path {
                Some(path) if path.exists() => format!("{} reads fine", path.display()),
                _ => "no file: the defaults".into(),
            };
            checks.push(Check::new(Mark::Ok, "config", detail));
            Some(config)
        }
        Err(e) => {
            checks.push(Check::new(Mark::Fail, "config", format!("{e:#}")));
            None
        }
    };
    let monitors = hyprland(&mut checks);
    wayland(&mut checks, &monitors);
    checks.push(match Desktop::connect() {
        Ok(_) => Check::new(Mark::Ok, "accessibility bus", "connected"),
        Err(e) => Check::new(
            Mark::Fail,
            "accessibility bus",
            format!("{e:#}; is at-spi2-core installed and running?"),
        ),
    });
    if let Some(config) = &config {
        fonts(&mut checks, config);
    }
    checks.extend(kitty());

    for check in &checks {
        let mark = match check.mark {
            Mark::Ok => "ok  ",
            Mark::Warn => "warn",
            Mark::Fail => "fail",
        };
        println!("{mark}  {}: {}", check.what, check.detail);
    }
    let failed = checks.iter().filter(|c| c.mark == Mark::Fail).count();
    if failed > 0 {
        bail!("{failed} of {} checks failed", checks.len());
    }
    Ok(())
}

/// Hyprland's socket, and the monitors it reports.
fn hyprland(checks: &mut Vec<Check>) -> Vec<Monitor> {
    match desktop::monitors() {
        Ok(monitors) => {
            let list: Vec<String> = monitors
                .iter()
                .map(|m| {
                    let focused = if m.focused { ", focused" } else { "" };
                    format!("{} at scale {}{focused}", m.name, m.scale)
                })
                .collect();
            checks.push(Check::new(Mark::Ok, "Hyprland", list.join("; ")));
            monitors
        }
        Err(e) => {
            checks.push(Check::new(
                Mark::Fail,
                "Hyprland",
                format!("{e:#}; wayhint runs on Hyprland only"),
            ));
            Vec::new()
        }
    }
}

/// The Wayland protocols a run binds. A viewporter only matters on a
/// monitor with a fractional scale.
fn wayland(checks: &mut Vec<Check>, monitors: &[Monitor]) {
    let globals = Connection::connect_to_env()
        .map_err(anyhow::Error::from)
        .and_then(|conn| Ok(registry_queue_init::<Quiet>(&conn)?.0));
    let globals = match globals {
        Ok(globals) => globals.contents().clone_list(),
        Err(e) => {
            checks.push(Check::new(Mark::Fail, "Wayland", format!("{e:#}")));
            return;
        }
    };
    let fractional = monitors.iter().any(|m| m.scale.fract() != 0.0);
    for (interface, missing, what) in PROTOCOLS {
        let found = globals.iter().any(|g| g.interface == interface);
        let mark = match (found, interface) {
            (true, _) => Mark::Ok,
            (false, "wp_viewporter") if !fractional => Mark::Ok,
            (false, _) => missing,
        };
        let detail = if found {
            what.to_owned()
        } else {
            format!("missing: no {what}")
        };
        checks.push(Check::new(mark, interface, detail));
    }
}

/// The fonts the tags, pixel tags and input box are drawn in, and whether
/// the icon font has the messages' icons.
fn fonts(checks: &mut Vec<Check>, config: &Config) {
    let looks = [
        ("[style] font", &config.style.font),
        ("[style.pixels] font", &config.style.for_pixels().font),
        ("[input] font", &config.input.style.font),
    ];
    for (what, pattern) in looks {
        checks.push(match overlay::load_font(pattern, config.style.size) {
            Ok(_) => Check::new(Mark::Ok, what, format!("{pattern:?} found")),
            Err(e) => Check::new(Mark::Fail, what, format!("{e:#}")),
        });
    }
    checks.push(icons(&config.info));
}

/// Whether `[info] icon_font` has every message's icon: fontconfig falls
/// back to another font for a name it doesn't know, which has none.
fn icons(info: &Info) -> Check {
    let what = "[info] icon_font";
    let font = match overlay::load_font(&info.icon_font, 13.0) {
        Ok(font) => font,
        Err(e) => return Check::new(Mark::Warn, what, format!("{e:#}; icons are left out")),
    };
    let b = &info.badges;
    let missing = [&b.scanning, &b.no_targets, &b.no_match, &b.help]
        .iter()
        .flat_map(|badge| badge.icon.chars())
        .filter(|&c| font.lookup_glyph_index(c) == 0)
        .count();
    if missing == 0 {
        Check::new(
            Mark::Ok,
            what,
            format!("{:?} has every icon", info.icon_font),
        )
    } else {
        Check::new(
            Mark::Warn,
            what,
            format!(
                "{:?} lacks {missing} icon(s), which are left out; install a Nerd Font's symbols (ttf-nerd-fonts-symbols)",
                info.icon_font
            ),
        )
    }
}

/// Each running kitty's remote control socket, which text mode reads
/// kitty through.
fn kitty() -> Vec<Check> {
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = processes
        .filter_map(|entry| {
            let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let name = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            (name.trim() == "kitty").then_some(pid)
        })
        .collect();
    pids.sort_unstable();
    pids.into_iter()
        .map(|pid| {
            let what = format!("kitty {pid}");
            match desktop::listen_on(pid) {
                Ok(address) => Check::new(
                    Mark::Ok,
                    what,
                    format!("remote control on {address}: text mode reads it"),
                ),
                Err(e) => Check::new(Mark::Warn, what, format!("{e:#}; text mode can't read it")),
            }
        })
        .collect()
}
