//! wayhint: keyboard hints for everything clickable on a Wayland desktop.
//!
//! `main` only turns the command line into a call and an error into an exit
//! code. Each module owns one job:
//!
//! | Module      | Job                                                      |
//! |-------------|----------------------------------------------------------|
//! | `action`    | the verbs: what to do at a target                        |
//! | `config`    | the settings file, validated once at start               |
//! | `keys`      | physical keys, and decoding config characters            |
//! | `style`     | how a tag looks                                          |
//! | `desktop`   | Hyprland and the accessibility tree, behind a few calls  |
//! | `snapshot`  | the scan's output: the seam every other part reads       |
//! | `selection` | labels and what typed keys mean (pure)                   |
//! | `text`      | text mode's matching: query in, ordered matches out (pure) |
//! | `overlay`   | the Wayland surface: draws tags, reads keys              |
//! | `pick`      | overlay plus selection: choose one target                |
//! | `search`    | text mode: overlay plus text, until Enter or back to tags |
//! | `pointer`   | the virtual pointer: click, and never leave a button held |
//! | `keyboard`  | your keymap, and modifiers held around a click           |
//! | `run`       | the loop: scan, pick, re-check, click, again             |
//! | `dump`      | `wayhint dump`, printing each seam as JSON               |
//! | `doctor`    | `wayhint doctor`, checking the setup a run needs         |

mod action;
mod cli;
mod config;
mod desktop;
mod doctor;
mod dump;
mod feed;
mod geometry;
mod keyboard;
mod keys;
mod overlay;
mod pick;
mod pixels;
mod placement;
mod pointer;
mod run;
mod search;
mod selection;
mod snapshot;
mod space;
mod status;
mod style;
mod text;
mod toggle;

use std::{path::Path, process::ExitCode};

use anyhow::{Context as _, Result};
use clap::Parser as _;
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    cli::{Cli, Command},
    config::Config,
    geometry::{Point, Rect},
    keys::Layout,
    overlay::Overlay,
    pick::Pick,
    snapshot::Snapshot,
};

fn main() -> ExitCode {
    // `parse` prints help, version and usage errors itself, exiting 0 or 2.
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:#}` prints the whole context chain on one line:
            // "config ~/.config/wayhint/config.toml: [keys] alphabet: ..."
            desktop::warn(&format!("wayhint: {error:#}"));
            ExitCode::FAILURE
        }
    }
}

/// One arm per command. The config is a closure, loaded only by the
/// commands that need it: `dump` and `click` work even with a broken file.
/// Its keys are decoded with the keymap your keyboard really has.
fn dispatch(cli: &Cli) -> Result<()> {
    let config = || {
        let layout = Layout::from_keymap(&keyboard::seat_keymap()?)?;
        Config::load(cli.config.as_deref(), &layout)
    };
    match &cli.command {
        None => match toggle::start_or_stop()? {
            Some(socket) => {
                let start = pick::Start {
                    verb: cli.verb,
                    text: cli.text,
                    holding: false,
                };
                run::run(config()?, cli.once, cli.static_mode, start, socket)
            }
            None => Ok(()),
        },
        Some(Command::Config { default: true }) => {
            print!("{}", config::DEFAULT);
            Ok(())
        }
        Some(Command::Config { default: false }) => {
            print!("{}", config()?.render());
            Ok(())
        }
        Some(Command::Dump(args)) => dump::run(args, cli.config.as_deref()),
        Some(Command::Doctor) => doctor::run(config(), cli.config.as_deref()),
        Some(Command::Overlay { from }) => overlay_from(&config()?, from),
        Some(Command::Click { at, verb }) => run::click_at(*at, *verb),
    }
}

/// Targets from a file: they never change, and opened blocks show all
/// their lines, with no windows to check them against.
struct Saved;

impl pick::Source for Saved {
    fn rescan(&mut self, _: &mut dyn FnMut()) -> Result<Option<Snapshot>> {
        Ok(None)
    }

    fn keep(&self, _: &mut Snapshot) {}

    fn read_text(&mut self, _: &Snapshot) -> Vec<text::Text> {
        Vec::new()
    }

    fn locate(&mut self, _: &Snapshot, _: &[text::Text], spans: &[text::Span]) -> Vec<Vec<Rect>> {
        vec![Vec::new(); spans.len()]
    }

    fn shows(&self, _: &Snapshot, _: usize, _: Point) -> bool {
        true
    }
}

/// `wayhint overlay --from snap.json`: the overlay alone, on saved targets.
/// Prints the picked target as JSON; clicks nothing.
fn overlay_from(config: &Config, path: &Path) -> Result<()> {
    let mut snapshot = Snapshot::load(path)?;
    snapshot.merge_nearby(config.merge_distance);
    let monitor = desktop::monitors()?
        .into_iter()
        .find(|m| m.id == snapshot.monitor)
        .with_context(|| {
            format!(
                "monitor {} from the snapshot isn't connected",
                snapshot.monitor
            )
        })?;
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let mut overlay = Overlay::new(
        &conn,
        &config.style,
        &config.marks,
        &config.input,
        &config.info,
        &monitor,
    )?;
    eprintln!(
        "wayhint: {} tags, keys through the {}; Esc leaves",
        snapshot.targets.len(),
        overlay.keyboard_mode()
    );
    let cursor = || anyhow::bail!("no pointer when drawing from a file");
    match pick::pick(
        &mut overlay,
        config,
        &mut snapshot,
        pick::Start::default(),
        &cursor,
        &mut Saved,
    )? {
        Pick::Target { index, act } => {
            let (x, y) = (act.nudge.x, act.nudge.y);
            eprintln!("wayhint: {} on, nudged {x},{y} px", act.action.name());
            println!(
                "{}",
                serde_json::to_string_pretty(&snapshot.targets[index])?
            );
        }
        Pick::Here { act, .. } => {
            eprintln!("wayhint: {} where the pointer is", act.action.name());
        }
        Pick::Again { act } => {
            eprintln!("wayhint: {} where the last command was", act.action.name());
        }
        Pick::Repeat => eprintln!("wayhint: repeat (nothing to repeat here)"),
        Pick::Release => eprintln!("wayhint: release (nothing is held here)"),
        Pick::Text(chosen) => {
            eprintln!("wayhint: {} on a text match", chosen.act.action.name());
            println!("{}", serde_json::to_string_pretty(&chosen.target)?);
        }
        // Only a run follows workspaces: tags from a file stay put.
        Pick::Leave | Pick::Rescan => {}
    }
    Ok(())
}
