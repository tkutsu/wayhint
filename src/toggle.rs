//! One run per Wayland session, in one mode: tags or text. Launching
//! again asks the current run to leave through its normal cleanup. The
//! same mode's launch is its off switch; the other mode's takes over once
//! the old run is gone, so each mode is a compositor shortcut of its own.

use std::{
    env,
    io::ErrorKind,
    os::{
        linux::net::SocketAddrExt as _,
        unix::net::{SocketAddr, UnixDatagram},
    },
    process,
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};

/// How long a launch waits for the run it stops to say its mode: a scan
/// holds up the run's socket as it holds up its keys.
const ANSWER: Duration = Duration::from_secs(2);

/// How long a launch in the other mode waits for the stopped run to leave.
const HANDOVER: Duration = Duration::from_secs(3);

/// Owns the toggle endpoint for a new run in text mode (`text`) or tag
/// mode, or stops the run in the same mode and returns `None`.
/// Linux abstract sockets disappear when their owner exits, so a crash
/// cannot leave a stale socket file that blocks the next launch.
pub fn start_or_stop(text: bool) -> Result<Option<UnixDatagram>> {
    let runtime = env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let display = env::var_os("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is not set")?;
    let mut name = b"wayhint:".to_vec();
    name.extend_from_slice(runtime.as_encoded_bytes());
    name.push(b':');
    name.extend_from_slice(display.as_encoded_bytes());
    let address = SocketAddr::from_abstract_name(name).context("naming the toggle socket")?;
    take_over(&address, text)
}

/// Requests on the session socket. Escape is acknowledged only after the
/// current mode decides whether it uses the key. Helpers forward `pass`
/// to the app; `used` means the overlay will process it.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Stop,
    Escape,
}

/// Reads one request and replies to its sender: an Escape helper hears
/// whether the key is used, a launch whether this run, in text mode if
/// `text`, is in its mode.
pub fn receive(socket: &UnixDatagram, uses_escape: bool, text: bool) -> Result<Option<Request>> {
    let mut message = [0; 32];
    let (len, sender) = match socket.recv_from(&mut message) {
        Ok(received) => received,
        Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match &message[..len] {
        mode @ (b"stop text" | b"stop tags") => {
            let same = (mode == b"stop text") == text;
            // Best effort: a sender with no name can't hear it, and still
            // stops the run.
            let _ = socket.send_to_addr(if same { b"same" } else { b"other" }, &sender);
            Ok(Some(Request::Stop))
        }
        b"escape" => {
            socket.send_to_addr(if uses_escape { b"used" } else { b"pass" }, &sender)?;
            Ok(uses_escape.then_some(Request::Escape))
        }
        _ => Ok(None),
    }
}

/// Binding chooses the owner atomically. A launch that finds a run asks
/// it to stop; in the other mode it then waits for the address to come
/// free, and owns it.
fn take_over(address: &SocketAddr, text: bool) -> Result<Option<UnixDatagram>> {
    if let Some(socket) = bind(address)? {
        return Ok(Some(socket));
    }
    if !stop(address, text)? {
        return Ok(None);
    }
    let until = Instant::now() + HANDOVER;
    loop {
        if let Some(socket) = bind(address)? {
            return Ok(Some(socket));
        }
        ensure!(
            Instant::now() < until,
            "the running wayhint didn't stop in time; launch again"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// The socket at `address`, or `None` while a run owns it.
fn bind(address: &SocketAddr) -> Result<Option<UnixDatagram>> {
    match UnixDatagram::bind_addr(address) {
        Ok(socket) => {
            socket.set_nonblocking(true)?;
            Ok(Some(socket))
        }
        Err(error) if error.kind() == ErrorKind::AddrInUse => Ok(None),
        Err(error) => Err(error).context("binding the toggle socket"),
    }
}

/// Asks the run at `address` to stop, saying this launch's mode, and
/// whether this launch takes over: the run was in the other mode, or left
/// before it could be asked. One that doesn't answer in time counts as the
/// same mode: it still stops, and nothing replaces it.
fn stop(address: &SocketAddr, text: bool) -> Result<bool> {
    // A name of its own to hear the answer on: the process, and a count
    // for launches in the same process (the tests).
    static LAUNCHES: AtomicU32 = AtomicU32::new(0);
    let launch = LAUNCHES.fetch_add(1, Ordering::Relaxed);
    let name = format!("wayhint-launch:{}:{launch}", process::id());
    let sender = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name)?)?;
    if sender.connect_addr(address).is_err() {
        return Ok(true);
    }
    sender.set_read_timeout(Some(ANSWER))?;
    sender.send(if text { b"stop text" } else { b"stop tags" })?;
    let mut answer = [0; 8];
    Ok(matches!(sender.recv(&mut answer), Ok(n) if &answer[..n] == b"other"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: &str) -> SocketAddr {
        SocketAddr::from_abstract_name(format!("wayhint-test:{}:{name}", std::process::id()))
            .unwrap()
    }

    /// Answers requests on `run`, a run in text mode if `text`, until a
    /// stop comes.
    fn until_stopped(run: &UnixDatagram, text: bool) {
        let until = Instant::now() + ANSWER;
        while receive(run, false, text).unwrap() != Some(Request::Stop) {
            assert!(Instant::now() < until, "no stop came");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_launch_in_the_same_mode_stops_the_run_and_starts_nothing() {
        let first = take_over(&address("same"), false).unwrap().unwrap();
        let second = thread::spawn(|| take_over(&address("same"), false).unwrap());
        until_stopped(&first, false);
        assert!(second.join().unwrap().is_none());
        drop(first);
        assert!(take_over(&address("same"), false).unwrap().is_some());
    }

    #[test]
    fn a_launch_in_the_other_mode_takes_over_once_the_run_leaves() {
        let first = take_over(&address("other"), false).unwrap().unwrap();
        let second = thread::spawn(|| take_over(&address("other"), true).unwrap());
        until_stopped(&first, false);
        drop(first);
        assert!(second.join().unwrap().is_some(), "text mode runs now");
    }

    #[test]
    fn separate_sessions_do_not_stop_each_other() {
        let first = take_over(&address("first-session"), false)
            .unwrap()
            .unwrap();
        let second = take_over(&address("second-session"), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            first.recv(&mut [0; 16]).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
        assert_eq!(
            second.recv(&mut [0; 16]).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }

    #[test]
    fn escape_is_acknowledged_only_when_the_current_mode_uses_it() {
        let server = take_over(&address("escape-server"), false)
            .unwrap()
            .unwrap();
        let client = UnixDatagram::bind_addr(&address("escape-client")).unwrap();
        client.connect_addr(&address("escape-server")).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        for used in [false, true] {
            client.send(b"escape").unwrap();
            assert_eq!(
                receive(&server, used, false).unwrap(),
                used.then_some(Request::Escape)
            );
            let mut reply = [0; 4];
            assert_eq!(client.recv(&mut reply).unwrap(), 4);
            assert_eq!(&reply, if used { b"used" } else { b"pass" });
        }
        client.send(b"unknown").unwrap();
        assert_eq!(receive(&server, true, false).unwrap(), None);
        client.send(b"stop text").unwrap();
        assert_eq!(receive(&server, false, false).unwrap(), Some(Request::Stop));
        let mut reply = [0; 5];
        assert_eq!(client.recv(&mut reply).unwrap(), 5);
        assert_eq!(&reply, b"other", "a tag run, asked by text mode");
    }
}
