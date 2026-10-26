//! The input box for another program to show, Quickshell's bar for one:
//! each new [`Status`] as one line of JSON, written to a socket that
//! program listens on, `$XDG_RUNTIME_DIR/wayhint-$WAYLAND_DISPLAY.sock`.
//!
//! wayhint dials; it never listens. A bar is up all the time and a run
//! isn't, so the bar holds the socket, and a run connects with its first
//! state. A run that ends closes the connection, a crash too, and the bar
//! clears what it shows: nothing stale is left either way.
//!
//! Nothing here waits for the listener, so typing never does. The dial
//! and the writes don't block: a listener that isn't there is tried
//! again with the next state, and one that falls behind, or a write that
//! goes out half, loses the connection. The next state dials again and
//! starts the listener over with the whole of it, never half a line.

use std::{
    env,
    io::{self, Read as _, Write as _},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
use serde::Serialize;

use crate::status::Status;

/// The line's shape; a listener checks it.
const VERSION: u32 = 1;

/// One line: the version, the monitor the run is on, then the status.
#[derive(Serialize)]
struct Line<'a> {
    version: u32,
    monitor: &'a str,
    #[serde(flatten)]
    status: &'a Status,
}

pub struct Feed {
    path: PathBuf,
    /// The output name (`HDMI-A-1`), for the bar on it.
    monitor: String,
    stream: Option<UnixStream>,
    /// The last line written on `stream`: the same state isn't sent twice.
    sent: String,
}

impl Feed {
    /// A feed for a run on `monitor`. `None` without `XDG_RUNTIME_DIR`,
    /// or with a `WAYLAND_DISPLAY` that's a path: no name to listen on.
    pub fn new(monitor: &str) -> Option<Self> {
        let runtime = env::var_os("XDG_RUNTIME_DIR")?;
        let display = env::var("WAYLAND_DISPLAY").ok()?;
        if display.contains('/') {
            return None;
        }
        Some(Self::at(
            Path::new(&runtime).join(format!("wayhint-{display}.sock")),
            monitor,
        ))
    }

    fn at(path: PathBuf, monitor: &str) -> Self {
        Self {
            path,
            monitor: monitor.into(),
            stream: None,
            sent: String::new(),
        }
    }

    /// Writes `status` to the listener, dialling it first if need be.
    pub fn send(&mut self, status: &Status) {
        let line = Line {
            version: VERSION,
            monitor: &self.monitor,
            status,
        };
        let Ok(mut line) = serde_json::to_string(&line) else {
            return;
        };
        line.push('\n');
        // The same state again needs no write, unless the listener went:
        // a bar that restarts wants it from a new connection.
        if let Some(stream) = &self.stream
            && line == self.sent
        {
            if !closed(stream) {
                return;
            }
            self.stream = None;
        }
        if self.stream.is_none() {
            self.stream = dial(&self.path).ok();
        }
        let Some(stream) = &mut self.stream else {
            return;
        };
        match stream.write(line.as_bytes()) {
            Ok(written) if written == line.len() => self.sent = line,
            _ => self.stream = None,
        }
    }
}

/// Connects to `path` without waiting: a listener whose queue is full
/// fails here instead of holding the run up.
fn dial(path: &Path) -> io::Result<UnixStream> {
    let socket = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&socket, &SocketAddrUnix::new(path)?)?;
    Ok(UnixStream::from(socket))
}

/// Whether the listener closed `stream`. It never writes, so a read that
/// would wait means it's still there; the stream doesn't block.
fn closed(mut stream: &UnixStream) -> bool {
    !matches!(stream.read(&mut [0]), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead as _, BufReader},
        os::unix::net::UnixListener,
    };

    use super::*;
    use crate::{status::Message, style::Notice};

    /// A socket path of the test's own.
    fn path(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("wayhint-feed-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.sock")
    }

    fn lines(stream: UnixStream) -> Vec<serde_json::Value> {
        BufReader::new(stream)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect()
    }

    #[test]
    fn a_listener_gets_the_current_state_then_each_change() {
        let path = path("changes");
        let mut feed = Feed::at(path.clone(), "HDMI-A-1");
        let typed = Status::tags("A".into(), Some("right"), false, true);
        feed.send(&typed);
        assert!(feed.stream.is_none(), "nobody listens yet");

        let listener = UnixListener::bind(&path).unwrap();
        feed.send(&typed);
        feed.send(&typed);
        let missed = Status {
            info: Some(Message::new(Notice::NoMatch, "no tag starts with \"<&")),
            ..typed.clone()
        };
        feed.send(&missed);
        drop(feed);
        let (stream, _) = listener.accept().unwrap();
        let got = lines(stream);
        assert_eq!(got.len(), 2, "the same state once: {got:?}");
        assert_eq!(got[0]["version"], 1);
        assert_eq!(got[0]["monitor"], "HDMI-A-1");
        assert_eq!(got[0]["input"], "A");
        assert_eq!(got[0]["verb"], "right");
        assert_eq!(got[1]["info"]["text"], "no tag starts with \"<&");
        let joined: String = got[0]["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["text"].as_str().unwrap())
            .collect();
        assert_eq!(joined, "right A");
    }

    #[test]
    fn a_listener_that_went_away_is_dialled_again() {
        let path = path("again");
        let listener = UnixListener::bind(&path).unwrap();
        let mut feed = Feed::at(path.clone(), "DP-1");
        feed.send(&Status::tags("A".into(), None, false, true));
        let (first, _) = listener.accept().unwrap();
        drop(first);
        // The write after the close fails and drops the connection; the
        // one after that dials again, and the new connection starts with
        // the whole state.
        let state = Status::tags("AB".into(), None, false, true);
        feed.send(&state);
        feed.send(&state);
        drop(feed);
        let (second, _) = listener.accept().unwrap();
        let got = lines(second);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["input"], "AB");
    }

    #[test]
    fn a_listener_that_went_away_gets_the_same_state_again() {
        let path = path("same");
        let listener = UnixListener::bind(&path).unwrap();
        let mut feed = Feed::at(path.clone(), "DP-1");
        let state = Status::tags("A".into(), None, false, true);
        feed.send(&state);
        let (first, _) = listener.accept().unwrap();
        drop(first);
        // A bar that restarts while nothing changes: the next send sees
        // the close and dials again.
        feed.send(&state);
        drop(feed);
        listener.set_nonblocking(true).unwrap();
        let (second, _) = listener.accept().expect("dialled again");
        let got = lines(second);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["input"], "A");
    }

    #[test]
    fn a_listener_that_never_reads_costs_its_connection_not_a_wait() {
        let path = path("stalled");
        let listener = UnixListener::bind(&path).unwrap();
        let mut feed = Feed::at(path.clone(), "DP-1");
        let long = "x".repeat(64 * 1024);
        let started = std::time::Instant::now();
        for i in 0..64 {
            feed.send(&Status::tags(format!("{i}{long}"), None, false, true));
        }
        assert!(started.elapsed().as_secs() < 2, "never blocked");
        drop(feed);
        let (mut stream, _) = listener.accept().unwrap();
        let mut got = Vec::new();
        stream.read_to_end(&mut got).unwrap();
        let text = String::from_utf8(got).unwrap();
        let whole: Vec<_> = text
            .split_inclusive('\n')
            .filter(|l| l.ends_with('\n'))
            .collect();
        assert!(
            !whole.is_empty() && whole.len() < 64,
            "{} lines",
            whole.len()
        );
        for line in whole {
            let _: serde_json::Value = serde_json::from_str(line).unwrap();
        }
    }
}
