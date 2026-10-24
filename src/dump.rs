//! `wayhint dump`: print what wayhint sees, as JSON on stdout.
//!
//! Each flag stops the pipeline at a different seam, so every part can be
//! checked alone: `--window` after Hyprland, `--raw` after the walk, the
//! default after the filter (a snapshot, which `overlay --from` reads).
//! `--surfaces` is the whole monitor as a run sees it: what stacks where,
//! which frame each layer got, and what couldn't be placed.

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli::Dump,
    desktop::{self, Desktop, Kind, Monitor, Window, filter},
    geometry::Rect,
    snapshot::Snapshot,
};

pub fn run(args: &Dump) -> Result<()> {
    if args.surfaces {
        return surfaces();
    }
    let window = match &args.address {
        Some(address) => {
            desktop::window_by_address(address)?.with_context(|| format!("no window {address}"))?
        }
        None => desktop::focused_window()?.context("no window has focus")?,
    };
    if args.window {
        return print(&window_and_monitor(&window)?);
    }

    let desktop = Desktop::connect()?;
    let (tree, stats) = desktop.read_tree(&window)?;
    if args.raw {
        return print(&Raw {
            tree: &tree,
            stats: &stats,
        });
    }
    let filtered = filter::filter(&tree);
    let snapshot = Snapshot::build(1, window.monitor, [(&tree, &filtered, &stats)]);
    if !args.explain {
        return print(&snapshot);
    }

    // `--explain`: the snapshot plus one line per dropped candidate.
    let dropped: Vec<Explained> = filtered
        .dropped
        .iter()
        .map(|d| {
            let node = &tree.nodes[d.node];
            Explained {
                role: &node.role,
                name: node
                    .details
                    .as_ref()
                    .map_or("", |details| details.name.as_str()),
                rect: node.bounds.map(|b| b.offset(window.rect.origin())),
                reason: d.reason,
            }
        })
        .collect();
    print(&WithDrops {
        snapshot: &snapshot,
        dropped,
    })
}

// Small structs only for printing. They borrow what they show (`&'a`), so
// nothing is copied just to be written out.

#[derive(Serialize)]
struct WindowDump<'a> {
    window: &'a Window,
    monitor: Option<Monitor>,
}

#[derive(Serialize)]
struct Raw<'a> {
    tree: &'a desktop::tree::Tree,
    stats: &'a desktop::Stats,
}

#[derive(Serialize)]
struct Explained<'a> {
    role: &'a str,
    name: &'a str,
    rect: Option<Rect>,
    reason: filter::Reason,
}

#[derive(Serialize)]
struct WithDrops<'a> {
    #[serde(flatten)]
    snapshot: &'a Snapshot,
    dropped: Vec<Explained<'a>>,
}

#[derive(Serialize)]
struct Surface<'a> {
    kind: Kind,
    class: &'a str,
    title: &'a str,
    rect: Rect,
    /// Frames that may be its own: more than one for two layers with one
    /// box (the scan read the one with controls).
    frames: usize,
    tags: usize,
}

#[derive(Serialize)]
struct Surfaces<'a> {
    surfaces: Vec<Surface<'a>>,
    /// Surfaces that couldn't be read, and frames that couldn't be placed.
    unread: Vec<String>,
}

/// `dump --surfaces`: the focused monitor's stack, scanned as a run scans it.
fn surfaces() -> Result<()> {
    let monitor = desktop::monitors()?
        .into_iter()
        .find(|m| m.focused)
        .context("no monitor has focus")?;
    let mut desktop = Desktop::connect()?;
    let stack = desktop.stack(monitor.id)?;
    let (snapshot, unread) = desktop.scan(&stack, &monitor);
    let tags = |w: &Window| {
        let targets = snapshot.targets.iter();
        targets
            .filter(|t| snapshot.windows[t.window].window.address == w.address)
            .count()
    };
    print(&Surfaces {
        surfaces: stack
            .windows
            .iter()
            .map(|w| Surface {
                kind: w.kind,
                class: &w.class,
                title: &w.title,
                rect: w.rect,
                frames: w.frames.len(),
                tags: tags(w),
            })
            .collect(),
        unread: unread.iter().map(|e| format!("{e:#}")).collect(),
    })
}

fn window_and_monitor(window: &Window) -> Result<WindowDump<'_>> {
    let monitor = desktop::monitors()?
        .into_iter()
        .find(|m| m.id == window.monitor);
    Ok(WindowDump { window, monitor })
}

fn print(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
