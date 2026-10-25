//! One run per Wayland session. Launching again asks the current run to
//! leave through its normal cleanup, before opening another overlay.

use std::{
    env,
    io::ErrorKind,
    os::{
        linux::net::SocketAddrExt as _,
        unix::net::{SocketAddr, UnixDatagram},
    },
};

use anyhow::{Context as _, Result};

/// Owns the toggle endpoint for a new run, or closes an existing one.
/// Linux abstract sockets disappear when their owner exits, so a crash
/// cannot leave a stale socket file that blocks the next launch.
pub fn start_or_stop() -> Result<Option<UnixDatagram>> {
    let runtime = env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let display = env::var_os("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is not set")?;
    let mut name = b"wayhint:".to_vec();
    name.extend_from_slice(runtime.as_encoded_bytes());
    name.push(b':');
    name.extend_from_slice(display.as_encoded_bytes());
    let address = SocketAddr::from_abstract_name(name).context("naming the toggle socket")?;
    at(&address)
}

/// Binding chooses the owner atomically; other launches send it a stop.
fn at(address: &SocketAddr) -> Result<Option<UnixDatagram>> {
    match UnixDatagram::bind_addr(address) {
        Ok(socket) => {
            socket.set_nonblocking(true)?;
            Ok(Some(socket))
        }
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            let sender = UnixDatagram::unbound()?;
            sender.set_nonblocking(true)?;
            sender.connect_addr(address)?;
            sender.send(b"stop")?;
            Ok(None)
        }
        Err(error) => Err(error).context("binding the toggle socket"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: &str) -> SocketAddr {
        SocketAddr::from_abstract_name(format!("wayhint-test:{}:{name}", std::process::id()))
            .unwrap()
    }

    #[test]
    fn a_second_launch_stops_the_first_and_a_later_launch_starts() {
        let address = address("toggle");
        let first = at(&address).unwrap().unwrap();
        assert!(at(&address).unwrap().is_none());
        let mut message = [0; 16];
        let n = first.recv(&mut message).unwrap();
        assert_eq!(&message[..n], b"stop");
        drop(first);
        assert!(at(&address).unwrap().is_some());
    }

    #[test]
    fn separate_sessions_do_not_stop_each_other() {
        let first = at(&address("first-session")).unwrap().unwrap();
        let second = at(&address("second-session")).unwrap().unwrap();
        assert_eq!(
            first.recv(&mut [0; 16]).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
        assert_eq!(
            second.recv(&mut [0; 16]).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }
}
