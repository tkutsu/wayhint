//! Text mode's way into kitty, which has no accessibility tree: ask kitty
//! itself what its panes show. Its remote control runs a kitten (a Python
//! file, `kitty.py`) inside kitty, over the socket `listen_on` opens; the
//! kitten answers with each pane's lines and the cell they start at.
//!
//! The socket's address is whatever kitty.conf says, so it's read where
//! kitty puts it for its programs: `KITTY_LISTEN_ON` in the environment of
//! a shell it started.

use std::{
    fs,
    io::{Read as _, Write as _},
    os::{
        linux::net::SocketAddrExt as _,
        unix::net::{SocketAddr, UnixStream},
    },
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use super::{Window, runtime_dir};
use crate::{geometry::Point, text::Cells};

/// The kitten, written out where kitty can run it from.
const KITTEN: &str = include_str!("kitty.py");

/// How remote control frames a command and its answer: a DCS escape.
const START: &[u8] = b"\x1bP@kitty-cmd";
const END: &[u8] = b"\x1b\\";

/// One pane's text, as text mode searches it.
pub struct Pane {
    pub title: String,
    pub content: String,
    pub cells: Cells,
}

/// Whether `window` is kitty's: by its class, or by its program when the
/// class was changed (`kitty --class`).
pub fn is_kitty(window: &Window) -> bool {
    let exe = fs::read_link(format!("/proc/{}/exe", window.pid)).ok();
    window.class.eq_ignore_ascii_case("kitty")
        || exe.is_some_and(|p| p.file_name().is_some_and(|n| n == "kitty"))
}

/// The panes `window` shows, each with its text, within `timeout`.
pub fn read(window: &Window, timeout: Duration) -> Result<Vec<Pane>> {
    let until = Instant::now() + timeout;
    let address = listen_on(window.pid)?;
    let answer = ask(&address, &kitten()?, until)
        .with_context(|| format!("kitty's remote control at {address}"))?;
    let windows: Vec<OsWindow> = serde_json::from_str(&answer).context("the kitten's answer")?;
    let os_window = pick(&windows, window)?;
    // Logical px per physical px: kitty's own, so a size Hyprland rounded
    // doesn't skew the cells.
    let scale = (os_window.framebuffer_width.max(1), os_window.width.max(1));
    let cell = (os_window.cell[0], os_window.cell[1]);
    Ok(os_window
        .panes
        .iter()
        .map(|pane| {
            let (content, at) = lay_out(&pane.lines);
            Pane {
                title: os_window.title.clone(),
                content,
                cells: Cells {
                    pane: pane.id,
                    window: window.rect.origin(),
                    first: Point::new(pane.left, pane.top),
                    cell,
                    scale,
                    at,
                },
            }
        })
        .collect())
}

/// What the kitten says about one OS window.
#[derive(Deserialize)]
struct OsWindow {
    title: String,
    focused: bool,
    width: i32,
    framebuffer_width: i32,
    cell: [i32; 2],
    panes: Vec<PaneRead>,
}

#[derive(Deserialize)]
struct PaneRead {
    id: u64,
    left: i32,
    top: i32,
    lines: Vec<Line>,
}

/// One row as kitty shows it: its text, whether it goes on in the next
/// row, and the column each character starts at plus where the last one
/// ends, when that isn't the character's index (wide characters, accents).
#[derive(Deserialize)]
struct Line {
    text: String,
    wrapped: bool,
    #[serde(default)]
    cols: Option<Vec<u16>>,
}

/// Which of kitty's OS windows is `window`: the only one, or the only one
/// with its title, or the focused one when `window` has the focus.
fn pick<'a>(windows: &'a [OsWindow], window: &Window) -> Result<&'a OsWindow> {
    if let [only] = windows {
        return Ok(only);
    }
    let titled: Vec<&OsWindow> = windows.iter().filter(|w| w.title == window.title).collect();
    match titled.as_slice() {
        [one] => Ok(one),
        _ => windows
            .iter()
            .find(|w| w.focused && window.focused)
            .with_context(|| {
                format!(
                    "{} kitty windows and none is clearly {:?}",
                    windows.len(),
                    window.title
                )
            }),
    }
}

/// The rows joined into one text, with each character's place: a wrapped
/// row runs on into the next, any other ends with a line break, so a
/// search finds words across both (white space folds to one space).
fn lay_out(lines: &[Line]) -> (String, Vec<(u16, u16, u16)>) {
    let mut content = String::new();
    let mut at = Vec::new();
    for (row, line) in (0u16..).zip(lines) {
        let count = line.text.chars().count();
        let starts: Vec<u16> = match &line.cols {
            Some(cols) if cols.len() == count + 1 => cols.clone(),
            _ => (0u16..).take(count + 1).collect(),
        };
        // A character ends where the next one in a later column starts:
        // an accent shares its letter's cell, and ends where it does.
        let mut ends = vec![0; count];
        for i in (0..count).rev() {
            ends[i] = if starts[i + 1] > starts[i] || i + 1 == count {
                starts[i + 1].max(starts[i] + 1)
            } else {
                ends[i + 1]
            };
        }
        content.push_str(&line.text);
        at.extend((0..count).map(|i| (row, starts[i], ends[i])));
        if !line.wrapped && usize::from(row) + 1 < lines.len() {
            content.push('\n');
            at.push((row, starts[count], starts[count]));
        }
    }
    (content, at)
}

/// The address kitty listens on, from a program it started.
fn listen_on(pid: u32) -> Result<String> {
    let tasks = fs::read_dir(format!("/proc/{pid}/task"))?;
    let children = tasks
        .filter_map(|task| fs::read_to_string(task.ok()?.path().join("children")).ok())
        .flat_map(|list| {
            list.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
    for child in children {
        let Ok(environ) = fs::read(format!("/proc/{child}/environ")) else {
            continue;
        };
        let found = environ
            .split(|&b| b == 0)
            .find_map(|var| var.strip_prefix(b"KITTY_LISTEN_ON="));
        if let Some(address) = found {
            return Ok(String::from_utf8_lossy(address).into_owned());
        }
    }
    bail!(
        "pid {pid} has no remote control socket: add \
         `listen_on unix:${{XDG_RUNTIME_DIR}}/kitty-{{kitty_pid}}.sock` to kitty.conf \
         and restart kitty"
    )
}

/// The kitten's path, written out first if it isn't there as this build
/// has it. In the runtime directory: only this user can change it, and
/// kitty runs whatever it holds.
fn kitten() -> Result<PathBuf> {
    let path = runtime_dir()?.join("kitty.py");
    if fs::read_to_string(&path).ok().as_deref() != Some(KITTEN) {
        fs::write(&path, KITTEN)?;
    }
    Ok(path)
}

/// Runs the kitten at `kitten` through the remote control at `address`
/// (`unix:/path` or `unix:@abstract`) and returns what it said.
fn ask(address: &str, kitten: &std::path::Path, until: Instant) -> Result<String> {
    let path = address
        .strip_prefix("unix:")
        .with_context(|| format!("only unix sockets are supported, not {address}"))?;
    let socket = match path.strip_prefix('@') {
        Some(name) => SocketAddr::from_abstract_name(name)?,
        None => SocketAddr::from_pathname(path)?,
    };
    let mut stream = UnixStream::connect_addr(&socket)?;
    let left = || {
        until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .context("kitty took too long")
    };
    stream.set_write_timeout(Some(left()?))?;
    // `all` runs it once whichever window is active; it reads them all.
    let command = serde_json::json!({
        "cmd": "kitten",
        "version": [0, 26, 0],
        "no_response": false,
        "payload": { "kitten": kitten, "args": [], "all": true },
    });
    stream.write_all(&[START, command.to_string().as_bytes(), END].concat())?;
    let mut answer = Vec::new();
    let mut chunk = [0; 8192];
    while !answer.ends_with(END) {
        stream.set_read_timeout(Some(left()?))?;
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("kitty closed the connection");
        }
        answer.extend_from_slice(&chunk[..read]);
    }
    let body = answer
        .strip_prefix(START)
        .and_then(|a| a.strip_suffix(END))
        .context("kitty's answer isn't framed as remote control frames it")?;
    let reply: Reply = serde_json::from_slice(body)?;
    match reply {
        Reply { ok: true, data, .. } => data.context("the kitten said nothing"),
        Reply { error, .. } => bail!("{}", error.unwrap_or_default()),
    }
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    data: Option<String>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, wrapped: bool, cols: Option<Vec<u16>>) -> Line {
        Line {
            text: text.into(),
            wrapped,
            cols,
        }
    }

    #[test]
    fn rows_join_with_breaks_unless_wrapped() {
        let lines = [
            line("ab", false, None),
            line("cd", true, None),
            line("ef", false, None),
        ];
        let (content, at) = lay_out(&lines);
        assert_eq!(content, "ab\ncdef");
        assert_eq!(at[2], (0, 2, 2), "the break covers no cell");
        assert_eq!(at[5], (2, 0, 1), "a wrapped row runs on");
        assert_eq!(at.len(), content.chars().count());
    }

    #[test]
    fn wide_characters_and_accents_keep_their_cells() {
        // `a你` then `e` with a combining accent: 你 is two cells wide.
        let lines = [line("a你e\u{301}", false, Some(vec![0, 1, 3, 3, 4]))];
        let (_, at) = lay_out(&lines);
        assert_eq!(at, [(0, 0, 1), (0, 1, 3), (0, 3, 4), (0, 3, 4)]);
    }

    fn os_window(title: &str, focused: bool) -> OsWindow {
        OsWindow {
            title: title.into(),
            focused,
            width: 100,
            framebuffer_width: 100,
            cell: [7, 17],
            panes: Vec::new(),
        }
    }

    #[test]
    fn os_windows_match_by_title_then_focus() {
        let mut window = Window {
            address: "0x1".into(),
            pid: 1,
            class: "kitty".into(),
            title: "b".into(),
            rect: crate::geometry::Rect::new(0, 0, 100, 100),
            monitor: 0,
            focused: true,
            kind: super::super::Kind::Client,
            frames: Vec::new(),
        };
        let windows = [os_window("a", false), os_window("b", true)];
        assert_eq!(pick(&windows, &window).unwrap().title, "b");
        let twins = [os_window("b", false), os_window("b", true)];
        window.focused = false;
        assert!(
            pick(&twins, &window).is_err(),
            "two of its title, unfocused"
        );
        window.focused = true;
        assert!(pick(&twins, &window).unwrap().focused);
    }
}
