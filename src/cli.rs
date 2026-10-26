//! The command line, declared as types. `clap`'s derive reads these structs
//! and enums and writes the parser, `--help` and `--version` for them; doc
//! comments become the help text.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::{action::Action, geometry::Point, pixels::Detector};

/// Keyboard hints for everything clickable on a Wayland desktop.
///
/// With no command: tag the controls of every window on the focused
/// monitor, click the one whose tag you type, and keep going until the
/// configured close key (Esc by default).
#[derive(Debug, Parser)]
#[command(version, about, long_about)]
pub struct Cli {
    /// Exit after one click instead of tagging the windows again
    #[arg(long)]
    pub once: bool,

    /// Scan once, keep the hints fixed, and exit after one action
    #[arg(long = "static")]
    pub static_mode: bool,

    /// Arm a verb for the first tag, as if its key were typed first
    #[arg(long, value_name = "ACTION")]
    pub verb: Option<Action>,

    /// Start in text mode: type text on screen, Enter acts on it
    #[arg(long)]
    pub text: bool,

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
    Config {
        /// Print the default config instead, every setting explained and
        /// commented out: a file to start yours from
        #[arg(long)]
        default: bool,
    },
    /// Check the setup: config, Hyprland, Wayland protocols, accessibility,
    /// fonts, OCR and kitty's remote control
    Doctor,
    /// Show a saved snapshot's tags and print the one you pick; clicks nothing
    #[command(hide = true)]
    Overlay {
        /// A snapshot written by `wayhint dump`
        #[arg(long, value_name = "SNAPSHOT")]
        from: PathBuf,
    },
    /// Click once at a global position, then put the cursor back
    #[command(hide = true)]
    Click {
        /// Global logical coordinates, `X,Y`
        #[arg(long, value_name = "X,Y", value_parser = parse_point)]
        at: Point,
        /// What to do there
        #[arg(long, value_name = "ACTION", default_value = "left")]
        verb: Action,
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
    /// Every surface on the focused monitor, bottom to top, with its tags
    #[arg(long, conflicts_with_all = ["window", "raw", "explain", "address"])]
    pub surfaces: bool,
    /// Every node the scan read, before filtering (the filter tests' fixtures)
    #[arg(long, conflicts_with = "explain")]
    pub raw: bool,
    /// Also list what was dropped, and why
    #[arg(long)]
    pub explain: bool,
    /// Tag the window from its pixels, as if it had no accessibility tree
    #[arg(
        long,
        value_name = "DETECTOR",
        num_args = 0..=1,
        default_missing_value = "full",
        conflicts_with_all = ["window", "raw", "explain"]
    )]
    pub pixels: Option<Detector>,
    /// Also read the window's text as text mode does, and where QUERY is in it
    #[arg(
        long,
        value_name = "QUERY",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["window", "surfaces", "raw", "explain", "pixels"]
    )]
    pub text: Option<String>,
    /// Scan this window (a Hyprland address, `0x...`) instead of the focused one
    #[arg(long, hide = true)]
    pub address: Option<String>,
}

/// `"120,-40"` to a point. clap calls this for `--at`; its `Err` becomes the
/// usage error clap prints.
fn parse_point(text: &str) -> Result<Point, String> {
    let (x, y) = text
        .split_once(',')
        .ok_or("write X,Y, for example 640,400")?;
    let number = |part: &str| {
        part.trim()
            .parse::<i32>()
            .map_err(|e| format!("{part:?}: {e}"))
    };
    Ok(Point::new(number(x)?, number(y)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_cli_definition_is_consistent() {
        // clap checks for clashing flags and bad attributes here, not at
        // compile time, so one test catches them all.
        Cli::command().debug_assert();
    }

    #[test]
    fn points_parse() {
        assert_eq!(parse_point("120,-40"), Ok(Point::new(120, -40)));
        assert!(parse_point("120").is_err());
        assert!(parse_point("a,b").is_err());
    }
}
