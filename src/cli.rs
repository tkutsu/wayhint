//! The command line, declared as types. `clap`'s derive reads these structs
//! and enums and writes the parser, `--help` and `--version` for them; doc
//! comments become the help text.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
    Dump {
        /// Only the focused window and its monitor, no scan
        #[arg(long)]
        window: bool,
    },
    /// Print the config in effect: your file merged with the defaults
    Config,
}
