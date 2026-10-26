//! `wayhint dump`: print what wayhint sees, as JSON on stdout.
//!
//! Each flag stops the pipeline at a different seam, so every part can be
//! checked alone: `--window` after Hyprland, `--raw` after the walk, the
//! default after the filter (a snapshot, which `overlay --from` reads).
//! `--surfaces` is the whole monitor as a run sees it: what stacks where,
//! which frame each layer got, and what couldn't be placed; with
//! `--pixels`, windows without a tree are tagged from their pixels too.
//! `--pixels` alone skips the tree and prints the tags the window's picture
//! gives, whether or not it has a tree: the pixel detector, on windows
//! whose tree says what the right answers are. `--text QUERY` stops after
//! text mode's reading: the window's text, and where the query is in it.

use std::time::Instant;

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli::Dump,
    desktop::{self, Desktop, Kind, Monitor, Window, filter},
    geometry::Rect,
    pixels::{Detector, Found},
    snapshot::{Read, Snapshot},
    text::{Place, Search, Text},
};

pub fn run(args: &Dump, config: Option<&std::path::Path>) -> Result<()> {
    if args.surfaces {
        return surfaces(args.pixels, config);
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
    if let Some(detector) = args.pixels {
        return print(&desktop::read_pixels(
            &window,
            detector,
            &crate::config::terminal_processes(config)?,
        )?);
    }

    let mut desktop = Desktop::connect()?;
    // kitty has no tree: its text comes from kitty itself.
    let tree = desktop.read_tree(&window);
    if let Some(query) = &args.text
        && desktop::is_kitty(&window)
    {
        let read = Read::Pixels {
            window: &window,
            found: &Found::default(),
            scale: 1.0,
            stats: &desktop::Stats::default(),
        };
        let snapshot = Snapshot::build(1, window.monitor, [read]);
        return text(&desktop, &snapshot, query);
    }
    let (tree, stats) = tree?;
    if args.raw {
        return print(&Raw {
            tree: &tree,
            stats: &stats,
        });
    }
    let filtered = filter::filter(&tree);
    let mut snapshot = Snapshot::build(1, window.monitor, [Read::Tree(&tree, &filtered, &stats)]);
    if let Some(monitor) = desktop::monitors()?
        .into_iter()
        .find(|m| m.id == window.monitor)
    {
        desktop.layout(&mut snapshot, &monitor, &desktop::Stack::default(), &[]);
    }
    if let Some(query) = &args.text {
        return text(&desktop, &snapshot, query);
    }
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

/// `dump --surfaces`: the focused monitor's stack, scanned as a run scans
/// it, with `pixels` for the windows without a tree.
fn surfaces(pixels: Option<Detector>, config: Option<&std::path::Path>) -> Result<()> {
    let monitor = desktop::monitors()?
        .into_iter()
        .find(|m| m.focused)
        .context("no monitor has focus")?;
    let mut desktop = Desktop::connect()?;
    let stack = desktop.stack(monitor.id)?;
    let terminals = crate::config::terminal_processes(config)?;
    let terminal_hint = crate::config::terminal_hint(config)?;
    let (snapshot, unread) = desktop.scan(&stack, &monitor, pixels, terminal_hint, &terminals);
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
        unread: unread.iter().map(|e| format!("{:#}", e.error)).collect(),
    })
}

#[derive(Serialize)]
struct TextDump<'a> {
    read_ms: f64,
    locate_ms: f64,
    /// Why the reading stopped early, if it did.
    incomplete: Option<String>,
    /// The windows whose text couldn't be read, and why.
    unread: Vec<String>,
    texts: Vec<TextRow<'a>>,
    matches: Vec<MatchRow<'a>>,
}

#[derive(Serialize)]
struct TextRow<'a> {
    role: &'a str,
    name: &'a str,
    rect: Rect,
    /// The character `text` starts at: more than 0 for a long text read
    /// from where it shows.
    start: u32,
    text: &'a str,
}

#[derive(Serialize)]
struct MatchRow<'a> {
    text: String,
    role: &'a str,
    /// A box per line, cut to where the text can show.
    parts: &'a [Rect],
}

/// `dump --text`: the text text mode would search in the scanned window,
/// and where `query` is in it, in the order text mode goes through them.
/// Nothing over the window is checked: that's a run's job.
fn text(desktop: &Desktop, snapshot: &Snapshot, query: &str) -> Result<()> {
    let ms = |since: Instant| since.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let reading = desktop.read_text(snapshot);
    let read_ms = ms(started);
    let mut search = Search::new(reading.texts);
    query.chars().for_each(|c| search.push(c));
    let wanted = search.wanted();
    let started = Instant::now();
    let boxes = desktop.locate(snapshot, search.texts(), &wanted);
    let locate_ms = ms(started);
    search.located(&wanted, boxes);
    search.update(&|_, _| true);
    let role = |text: &Text| match text.place {
        Place::Object(index) => snapshot.texts[index].role.as_str(),
        Place::Cells(_) => "terminal",
    };
    print(&TextDump {
        read_ms,
        locate_ms,
        incomplete: reading.incomplete,
        unread: reading
            .unread
            .iter()
            .map(|e| format!("{:#}", e.error))
            .collect(),
        texts: search
            .texts()
            .iter()
            .map(|t| TextRow {
                role: role(t),
                name: &t.name,
                rect: t.rect,
                start: t.start,
                text: &t.content,
            })
            .collect(),
        matches: search
            .matches()
            .iter()
            .map(|m| {
                let text = &search.texts()[m.span.text];
                MatchRow {
                    text: text.chars(m.span.start, m.span.end),
                    role: role(text),
                    parts: &m.parts,
                }
            })
            .collect(),
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
