//! Pictures of windows, for the pixel tags, through Hyprland's toplevel
//! export (`hyprland_toplevel_export_v1`). Hyprland renders the one window
//! alone into a buffer wayhint hands it: no other window, no wallpaper,
//! covered parts included (`ScreenshareFrame::renderWindow`, 0.56.2).
//! The spike measured 6 to 12 ms a picture at 2560x1440
//! (`mds/spike-pixels/capture.py`).
//!
//! The buffer is shared memory: a memfd the compositor writes and wayhint
//! reads back with `read_exact_at`, no `mmap`, so no `unsafe`.

use std::{
    fs::File,
    io::ErrorKind,
    os::{fd::AsFd as _, unix::fs::FileExt as _},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{MemfdFlags, ftruncate, memfd_create},
};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
    backend::WaylandError,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
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
    queue: EventQueue<Shot>,
    manager: HyprlandToplevelExportManagerV1,
    shm: WlShm,
}

/// One picture on its way: what the compositor has said so far.
#[derive(Default)]
struct Shot {
    /// The shared-memory buffer it can fill: format, width, height,
    /// stride.
    buffer: Option<(wl_shm::Format, u32, u32, u32)>,
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
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
        let (globals, queue) = registry_queue_init::<Shot>(&conn)?;
        let qh = queue.handle();
        let manager = globals
            .bind(&qh, 1..=2, ())
            .context("the compositor can't export windows (hyprland_toplevel_export_v1)")?;
        let shm = globals.bind(&qh, 1..=1, ())?;
        Ok(Self {
            conn,
            queue,
            manager,
            shm,
        })
    }

    /// A picture of the window with Hyprland address `address` (`0x...`),
    /// in the window's own pixels (physical, at the monitor's scale), on
    /// black where it's translucent. Gives up after `timeout`.
    pub fn shoot(&mut self, address: &str, timeout: Duration) -> Result<Frame> {
        // The protocol takes the address's low 32 bits, as `hyprctl
        // clients` prints it, and finds the window by them.
        let handle = u64::from_str_radix(address.trim_start_matches("0x"), 16)
            .with_context(|| format!("window address {address:?}"))?;
        #[allow(clippy::cast_possible_truncation)] // the low half, on purpose
        let handle = handle as u32;
        let qh = self.queue.handle();
        let mut shot = Shot::default();
        let frame = self.manager.capture_toplevel(0, handle, &qh, ());
        let until = Instant::now() + timeout;
        let picture = self.develop(&frame, &mut shot, until, &qh);
        frame.destroy();
        let _ = self.conn.flush();
        picture.with_context(|| format!("picture of window {address}"))
    }

    /// Hands the compositor a buffer for `frame` once it says which, and
    /// reads the picture back.
    fn develop(
        &mut self,
        frame: &HyprlandToplevelExportFrameV1,
        shot: &mut Shot,
        until: Instant,
        qh: &QueueHandle<Shot>,
    ) -> Result<Frame> {
        self.wait(shot, until, |s| s.stage != Stage::Asked)?;
        ensure!(shot.stage != Stage::Failed, "the compositor refused");
        let (format, width, height, stride) = shot
            .buffer
            .context("the compositor offered no shared-memory buffer")?;
        let bgr = match format {
            wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => true,
            wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => false,
            other => bail!("pixel format {other:?} isn't one wayhint reads"),
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
        let waited = self.wait(shot, until, |s| {
            matches!(s.stage, Stage::Ready | Stage::Failed)
        });
        buffer.destroy();
        pool.destroy();
        waited?;
        ensure!(
            shot.stage == Stage::Ready,
            "the compositor failed to copy it"
        );

        let mut raw = vec![0; usize::try_from(size)?];
        File::from(fd).read_exact_at(&mut raw, 0)?;
        let (width, height, stride) = (width as usize, height as usize, stride as usize);
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let row = if shot.inverted { height - 1 - y } else { y };
            let (pixels, _) = raw[row * stride..row * stride + width * 4].as_chunks::<4>();
            for &[a, b, c, _] in pixels {
                // Premultiplied alpha: dropping it is drawing on black.
                rgb.extend(if bgr { [c, b, a] } else { [a, b, c] });
            }
        }
        Ok(Frame { width, height, rgb })
    }

    /// Dispatches events until `done` or `until`, waiting on the socket in
    /// between: a compositor that never answers costs the deadline, not
    /// the run.
    fn wait(
        &mut self,
        shot: &mut Shot,
        until: Instant,
        done: impl Fn(&Shot) -> bool,
    ) -> Result<()> {
        loop {
            self.queue.dispatch_pending(shot)?;
            if done(shot) {
                return Ok(());
            }
            self.conn.flush()?;
            // `None`: events came in meanwhile; dispatch them first.
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("no picture in time");
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
}

impl Dispatch<HyprlandToplevelExportFrameV1, ()> for Shot {
    fn event(
        shot: &mut Self,
        _: &HyprlandToplevelExportFrameV1,
        event: export_frame::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            export_frame::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => shot.buffer = Some((format, width, height, stride)),
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

impl Dispatch<WlRegistry, GlobalListContents> for Shot {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

smithay_client_toolkit::reexports::client::delegate_noop!(Shot: ignore WlShm);
smithay_client_toolkit::reexports::client::delegate_noop!(Shot: WlShmPool);
smithay_client_toolkit::reexports::client::delegate_noop!(Shot: ignore WlBuffer);
smithay_client_toolkit::reexports::client::delegate_noop!(Shot: HyprlandToplevelExportManagerV1);
