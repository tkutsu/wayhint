//! The command line, declared as types. `clap`'s derive reads these structs
//! and enums and writes the parser, `--help` and `--version` for them; doc
//! comments become the help text.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Config file to use instead of ~/.config/wayhint/config.toml
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print what wayhint sees, as JSON (may contain private text from your windows)
    Dump(Dump),
    /// Print the config in effect: your file merged with the defaults
    Config,
    /// Show a saved snapshot's tags and print the one you pick; clicks nothing
    #[command(hide = true)]
    Overlay {
        /// A snapshot written by `wayhint dump`
        #[arg(long, value_name = "SNAPSHOT")]
        from: PathBuf,
    },
}

/// `#[derive(Args)]` makes a reusable group of flags: `Dump`'s live in their
/// own struct, so `main` can hand them around as one value.
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)] // independent flags, as clap wants them
pub struct Dump {
    /// Only the focused window and its monitor, no scan
    #[arg(long, conflicts_with_all = ["raw", "explain"])]
    pub window: bool,
    /// Every node the scan read, before filtering (the filter tests' fixtures)
    #[arg(long, conflicts_with = "explain")]
    pub raw: bool,
    /// Also list what was dropped, and why
    #[arg(long)]
    pub explain: bool,
    /// Scan this window (a Hyprland address, `0x...`) instead of the focused one
    #[arg(long, hide = true)]
    pub address: Option<String>,
}
