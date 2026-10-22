//! wayhint: keyboard hints for everything clickable on a Wayland desktop.
//!
//! `main` only turns the command line into a call and an error into an exit
//! code. Each module owns one job; see the plan's module table.

mod cli;
mod config;
mod desktop;
mod geometry;
mod keys;

use std::process::ExitCode;

use anyhow::{Context as _, Result};
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
    match cli.command {
        None => {
            config()?;
            println!("wayhint: nothing to do yet");
            Ok(())
        }
        Some(Command::Config) => {
            print!("{}", config()?.render());
            Ok(())
        }
        Some(Command::Dump { window: true }) => dump_window(),
        Some(Command::Dump { window: false }) => {
            anyhow::bail!("only `dump --window` exists so far")
        }
    }
}

/// `wayhint dump --window`: the focused window and the monitor it's on.
fn dump_window() -> Result<()> {
    /// A struct only for printing. It borrows instead of owning, hence the
    /// lifetime `'a`: it can't outlive the values it points at. Unlike
    /// `serde_json::json!`, a struct keeps its fields in this order.
    #[derive(serde::Serialize)]
    struct Dump<'a> {
        window: &'a desktop::Window,
        monitor: Option<&'a desktop::Monitor>,
    }

    let window = desktop::focused_window()?.context("no window has focus")?;
    let monitors = desktop::monitors()?;
    let monitor = monitors.iter().find(|m| m.id == window.monitor);
    let dump = Dump {
        window: &window,
        monitor,
    };
    println!("{}", serde_json::to_string_pretty(&dump)?);
    Ok(())
}
