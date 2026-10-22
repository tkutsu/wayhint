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
//! | `overlay`   | the Wayland surface: draws tags, reads keys              |
//! | `pick`      | overlay plus selection: choose one target                |
//! | `pointer`   | the virtual pointer: click, and never leave a button held |
//! | `run`       | the loop: scan, pick, re-check, click, again             |
//! | `dump`      | `wayhint dump`, printing each seam as JSON               |

mod action;
mod cli;
mod config;
mod desktop;
mod dump;
mod geometry;
mod keys;
mod overlay;
mod pick;
mod pointer;
mod run;
mod selection;
mod snapshot;
mod style;

use std::{path::Path, process::ExitCode};

use anyhow::{Context as _, Result};
use clap::Parser as _;
use smithay_client_toolkit::reexports::client::Connection;

use crate::{
    cli::{Cli, Command},
    config::Config,
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
            eprintln!("wayhint: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// One arm per command. The config is a closure, loaded only by the
/// commands that need it: `dump` and `click` work even with a broken file.
fn dispatch(cli: &Cli) -> Result<()> {
    let config = || Config::load(cli.config.as_deref(), desktop::keyboard_layout);
    match &cli.command {
        None => run::run(&config()?, cli.once, cli.verb),
        Some(Command::Config) => {
            print!("{}", config()?.render());
            Ok(())
        }
        Some(Command::Dump(args)) => dump::run(args),
        Some(Command::Overlay { from }) => overlay_from(&config()?, from),
        Some(Command::Click { at, verb }) => run::click_at(*at, *verb),
    }
}

/// `wayhint overlay --from snap.json`: the overlay alone, on saved targets.
/// Prints the picked target as JSON; clicks nothing.
fn overlay_from(config: &Config, path: &Path) -> Result<()> {
    let snapshot = Snapshot::load(path)?;
    let monitor = desktop::monitors()?
        .into_iter()
        .find(|m| m.id == snapshot.window.monitor)
        .with_context(|| {
            format!(
                "monitor {} from the snapshot isn't connected",
                snapshot.window.monitor
            )
        })?;
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let mut overlay = Overlay::new(&conn, &config.style, &monitor)?;
    eprintln!(
        "wayhint: {} tags, keys through the {}; Esc leaves",
        snapshot.targets.len(),
        overlay.keyboard_mode()
    );
    match pick::pick(&mut overlay, config, &snapshot.targets, None)? {
        Pick::Target { index, action } => {
            eprintln!("wayhint: {} on", action.name());
            println!(
                "{}",
                serde_json::to_string_pretty(&snapshot.targets[index])?
            );
        }
        Pick::Here(action) => eprintln!("wayhint: {} where the pointer is", action.name()),
        Pick::Leave => {}
    }
    Ok(())
}
