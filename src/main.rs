//! wayhint: keyboard hints for everything clickable on a Wayland desktop.
//!
//! `main` only turns the command line into a call and an error into an exit
//! code. Each module owns one job:
//!
//! | Module      | Job                                                      |
//! |-------------|----------------------------------------------------------|
//! | `config`    | the settings file, validated once at start               |
//! | `keys`      | physical keys, and decoding config characters            |
//! | `style`     | how a tag looks                                          |
//! | `desktop`   | Hyprland and the accessibility tree, behind a few calls  |
//! | `snapshot`  | the scan's output: the seam every other part reads       |
//! | `selection` | labels and what typed keys mean (pure)                   |
//! | `overlay`   | the Wayland surface: draws tags, reads keys              |
//! | `pick`      | overlay plus selection: choose one target                |
//! | `dump`      | `wayhint dump`, printing each seam as JSON               |

mod cli;
mod config;
mod desktop;
mod dump;
mod geometry;
mod keys;
mod overlay;
mod pick;
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
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:#}` prints the whole context chain on one line:
            // "config ~/.config/wayhint/config.toml: [keys] alphabet: ..."
            eprintln!("wayhint: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let config = || Config::load(cli.config.as_deref(), desktop::keyboard_layout);
    match &cli.command {
        None => {
            config()?;
            println!("wayhint: nothing to do yet");
            Ok(())
        }
        Some(Command::Config) => {
            print!("{}", config()?.render());
            Ok(())
        }
        Some(Command::Dump(args)) => dump::run(args),
        Some(Command::Overlay { from }) => overlay_from(&config()?, from),
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
    if let Pick::Target(index) = pick::pick(&mut overlay, &config.alphabet, &snapshot.targets)? {
        println!(
            "{}",
            serde_json::to_string_pretty(&snapshot.targets[index])?
        );
    }
    Ok(())
}
