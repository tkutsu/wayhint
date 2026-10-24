//! Pictures of windows, for the pixel tags, through Hyprland's toplevel
//! export (`hyprland_toplevel_export_v1`). Hyprland renders the one window
//! alone into a buffer wayhint hands it: no other window, no wallpaper,
//! covered parts included (`ScreenshareFrame::renderWindow`, 0.56.2).
//! The spike measured 6 to 12 ms a picture at 2560x1440
//! (`mds/spike-pixels/capture.py`).
//!
//! The buffer is shared memory: a memfd the compositor writes and wayhint
//! reads back with `read_exact_at`, no `mmap`, so no `unsafe`. Every wait
//! on the compositor, connecting included, has the caller's deadline.

use std::{
    fs::File,
    io::ErrorKind,
    os::{fd::AsFd as _, unix::fs::FileExt as _},
    time::Instant,
};

use anyhow::{Context as _, Result, bail, ensure};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{MemfdFlags, ftruncate, memfd_create},
};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
    backend::WaylandError,
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
};

use self::protocol::{
    hyprland_toplevel_export_frame_v1::{self as export_frame, HyprlandToplevelExportFrameV1},
    hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1,
};
use crate::pixels::Frame;

/// The protocol's client code, generated from its XML at build time, as
/// `wayland-protocols` does it for the protocols it ships. Version 2 has a
/// request taking a wlroots toplevel handle, so that protocol's types
/// come into scope too; wayhint never sends it.
#[allow(
    clippy::all,
    clippy::pedantic,
    dead_code,
    missing_docs,
    non_upper_case_globals
)]
mod protocol {
    use smithay_client_toolkit::reexports::{
        client::{self as wayland_client, protocol::*},
        protocols_wlr::foreign_toplevel::v1::client::*,
    };

    pub mod __interfaces {
        use smithay_client_toolkit::reexports::{
            client::{backend as wayland_backend, protocol::__interfaces::*},
            protocols_wlr::foreign_toplevel::v1::client::__interfaces::*,
        };
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-export-v1.xml");
}

/// A connection that takes pictures of windows.
pub struct Camera {
    conn: Connection,
    queue: EventQueue<Heard>,
    heard: Heard,
    manager: HyprlandToplevelExportManagerV1,
    shm: WlShm,
}

/// What the compositor has said on the camera's connection.
#[derive(Default)]
struct Heard {
    /// Its globals, as name, interface and version: read once, to connect.
    globals: Vec<(u32, String, u32)>,
    /// The roundtrip that ends the list of globals came back.
    synced: bool,
    /// The picture on its way.
    shot: Shot,
}

/// One picture on its way.
#[derive(Default)]
struct Shot {
    /// The shared-memory buffers it can fill, in the order offered:
    /// format, width, height, stride.
    buffers: Vec<(wl_shm::Format, u32, u32, u32)>,
    /// Rows come bottom to top.
    inverted: bool,
    stage: Stage,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Asked for a picture; waiting to hear which buffers it can fill.
    #[default]
    Asked,
    /// Every buffer kind is announced: one can be made.
    Announced,
    /// The picture is in the buffer.
    Ready,
    Failed,
}

impl Camera {
    /// Connects and binds the window export, giving up at `until`. Not
    /// `registry_queue_init`, which waits on its roundtrip for as long as
    /// the compositor takes.
    pub fn connect(until: Instant) -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        conn.display().sync(&qh, ());
        let mut heard = Heard::default();
        wait(&conn, &mut queue, &mut heard, until, |h| h.synced)
            .context("listing the compositor's protocols")?;
        let global = |interface: &str| {
            let mut globals = heard.globals.iter();
            globals.find_map(|(name, i, version)| (i == interface).then_some((*name, *version)))
        };
        let (name, version) = global("hyprland_toplevel_export_manager_v1")
            .context("the compositor can't export windows (hyprland_toplevel_export_v1)")?;
        let manager = registry.bind(name, version.min(2), &qh, ());
        let (name, _) = global("wl_shm").context("the compositor has no shared memory (wl_shm)")?;
        let shm = registry.bind(name, 1, &qh, ());
        Ok(Self {
            conn,
            queue,
            heard,
            manager,
            shm,
        })
    }

    /// A picture of the window with Hyprland address `address` (`0x...`),
    /// in the window's own pixels (physical, at the monitor's scale), on
    /// black where it's translucent. Gives up at `until`.
    pub fn shoot(&mut self, address: &str, until: Instant) -> Result<Frame> {
        // The protocol takes the address's low 32 bits, as `hyprctl
        // clients` prints it, and finds the window by them.
        let handle = u64::from_str_radix(address.trim_start_matches("0x"), 16)
            .with_context(|| format!("window address {address:?}"))?;
        #[allow(clippy::cast_possible_truncation)] // the low half, on purpose
        let handle = handle as u32;
        let qh = self.queue.handle();
        self.heard.shot = Shot::default();
        let frame = self.manager.capture_toplevel(0, handle, &qh, ());
        let picture = self.develop(&frame, until, &qh);
        frame.destroy();
        let _ = self.conn.flush();
        picture.with_context(|| format!("picture of window {address}"))
    }

    /// Hands the compositor a buffer for `frame` once it says which, and
    /// reads the picture back.
    fn develop(
        &mut self,
        frame: &HyprlandToplevelExportFrameV1,
        until: Instant,
        qh: &QueueHandle<Heard>,
    ) -> Result<Frame> {
        let (conn, queue, heard) = (&self.conn, &mut self.queue, &mut self.heard);
        wait(conn, queue, heard, until, |h| h.shot.stage != Stage::Asked)?;
        ensure!(heard.shot.stage != Stage::Failed, "the compositor refused");
        let order = |format| match format {
            wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => Some(true),
            wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => Some(false),
            _ => None,
        };
        let offered = &heard.shot.buffers;
        let Some((bgr, (format, width, height, stride))) =
            offered.iter().find_map(|&b| order(b.0).map(|bgr| (bgr, b)))
        else {
            let formats: Vec<_> = offered.iter().map(|b| b.0).collect();
            bail!("no pixel format wayhint reads among {formats:?}");
        };
        let size = u64::from(stride) * u64::from(height);
        let fd = memfd_create("wayhint-window", MemfdFlags::CLOEXEC)?;
        ftruncate(&fd, size)?;
        let (w, h, s, total) = (
            i32::try_from(width)?,
            i32::try_from(height)?,
            i32::try_from(stride)?,
            i32::try_from(size)?,
        );
        let pool = self.shm.create_pool(fd.as_fd(), total, qh, ());
        let buffer = pool.create_buffer(0, w, h, s, format, qh, ());
        frame.copy(&buffer, 1);
        let waited = wait(conn, queue, heard, until, |h| {
            matches!(h.shot.stage, Stage::Ready | Stage::Failed)
        });
        buffer.destroy();
        pool.destroy();
        waited?;
        ensure!(
            heard.shot.stage == Stage::Ready,
            "the compositor failed to copy it"
        );

        let mut raw = vec![0; usize::try_from(size)?];
        File::from(fd).read_exact_at(&mut raw, 0)?;
        let (width, height, stride) = (width as usize, height as usize, stride as usize);
        ensure!(stride >= width * 4, "rows of {stride} bytes for {width} px");
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let row = if heard.shot.inverted {
                height - 1 - y
            } else {
                y
            };
            let (pixels, _) = raw[row * stride..row * stride + width * 4].as_chunks::<4>();
            for &[a, b, c, _] in pixels {
                // Premultiplied alpha: dropping it is drawing on black.
                rgb.extend(if bgr { [c, b, a] } else { [a, b, c] });
            }
        }
        Ok(Frame { width, height, rgb })
    }
}

/// Dispatches `queue`'s events into `heard` until `done` or `until`,
/// waiting on the socket in between: a compositor that never answers costs
/// the deadline, not the run.
fn wait(
    conn: &Connection,
    queue: &mut EventQueue<Heard>,
    heard: &mut Heard,
    until: Instant,
    done: impl Fn(&Heard) -> bool,
) -> Result<()> {
    loop {
        queue.dispatch_pending(heard)?;
        if done(heard) {
            return Ok(());
        }
        conn.flush()?;
        // `None`: events came in meanwhile; dispatch them first.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("no answer in time");
        }
        let timeout = Timespec {
            tv_sec: i64::try_from(left.as_secs())?,
            tv_nsec: i64::from(left.subsec_nanos()),
        };
        let fd = guard.connection_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        // Dropping the guard instead of reading cancels the read.
        if fds[0].revents().contains(PollFlags::IN) {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Dispatch<HyprlandToplevelExportFrameV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &HyprlandToplevelExportFrameV1,
        event: export_frame::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let shot = &mut heard.shot;
        match event {
            export_frame::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => shot.buffers.push((format, width, height, stride)),
            export_frame::Event::Flags {
                flags: WEnum::Value(flags),
            } => shot.inverted = flags.contains(export_frame::Flags::YInvert),
            export_frame::Event::BufferDone if shot.stage == Stage::Asked => {
                shot.stage = Stage::Announced;
            }
            export_frame::Event::Ready { .. } => shot.stage = Stage::Ready,
            export_frame::Event::Failed => shot.stage = Stage::Failed,
            // Damage, a dmabuf offer and formats wayhint can't name: not
            // needed for one picture in shared memory.
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            heard.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<WlCallback, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            heard.synced = true;
        }
    }
}

smithay_client_toolkit::reexports::client::delegate_noop!(Heard: ignore WlShm);
smithay_client_toolkit::reexports::client::delegate_noop!(Heard: WlShmPool);
smithay_client_toolkit::reexports::client::delegate_noop!(Heard: ignore WlBuffer);
smithay_client_toolkit::reexports::client::delegate_noop!(Heard: HyprlandToplevelExportManagerV1);
