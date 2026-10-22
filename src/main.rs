//! wayhint: keyboard hints for everything clickable on a Wayland desktop.
//!
//! `main` only turns the command line into a call and an error into an exit
//! code. Each module owns one job:
//!
//! | Module      | Job                                                      |
//! |-------------|----------------------------------------------------------|
//! | `config`    | the settings file, validated once at start               |
//! | `keys`      | physical keys, and decoding config characters            |
//! | `desktop`   | Hyprland and the accessibility tree, behind a few calls  |
//! | `snapshot`  | the scan's output: the seam every other part reads       |
//! | `dump`      | `wayhint dump`, printing each seam as JSON               |

mod cli;
mod config;
mod desktop;
mod dump;
mod geometry;
mod keys;
mod selection;
mod snapshot;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser as _;

use crate::{
    cli::{Cli, Command},
    config::Config,
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
    }
}
