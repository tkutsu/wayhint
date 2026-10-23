//! Clicking, through the compositor's virtual-pointer protocol
//! (`zwlr_virtual_pointer_v1`). No `uinput`, no `input` group, no root: the
//! compositor advertises the protocol, or wayhint can't click.
//!
//! Every button wayhint presses is written down in `held` and released by
//! [`Pointer::release_all`] on every exit; `Drop` calls it too, as a
//! backstop. A button left pressed by a crashed tool is the worst thing it
//! can do to a desktop.

use std::{thread::sleep, time::Duration};

use anyhow::{Context as _, Result};
use rustix::time::{ClockId, clock_gettime};
use smithay_client_toolkit::reexports::{
    client::{
        Connection, Dispatch, EventQueue, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_pointer::ButtonState, wl_registry, wl_seat::WlSeat},
    },
    protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    },
};

use crate::{
    action::Action,
    geometry::{Point, Rect},
};

// Linux's button codes (`BTN_*` in input-event-codes.h). Side and extra are
// the thumb buttons, which browsers and file managers read as back and forward.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
/// Pauses that let apps see the pointer arrive and the press as a click,
/// measured in the phase 1 spike (81 ms from pick to restored cursor). The
/// gap between a double click's two clicks is also `PRESS`: far inside the
/// 400 ms GTK and Qt allow by default.
const ARRIVE: Duration = Duration::from_millis(30);
const PRESS: Duration = Duration::from_millis(20);

/// The button an action presses, and how many times: a hover, none.
const fn presses(action: Action) -> (u32, u32) {
    match action {
        Action::Left => (BTN_LEFT, 1),
        Action::Double => (BTN_LEFT, 2),
        Action::Right => (BTN_RIGHT, 1),
        Action::Middle => (BTN_MIDDLE, 1),
        Action::Back => (BTN_SIDE, 1),
        Action::Forward => (BTN_EXTRA, 1),
        Action::Hover => (BTN_LEFT, 0),
    }
}

/// The pointer's own event queue needs a state type. None of these objects
/// sends anything wayhint reads, so it's empty.
struct Quiet;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Quiet {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// `delegate_noop!` writes empty `Dispatch` impls: `ignore` for objects whose
// events are dropped, plain for objects that have no events at all.
smithay_client_toolkit::reexports::client::delegate_noop!(Quiet: ignore WlSeat);
smithay_client_toolkit::reexports::client::delegate_noop!(Quiet: ZwlrVirtualPointerManagerV1);
smithay_client_toolkit::reexports::client::delegate_noop!(Quiet: ZwlrVirtualPointerV1);

pub struct Pointer {
    conn: Connection,
    // Kept alive with the pointer; never dispatched, since nothing on it
    // sends events.
    _queue: EventQueue<Quiet>,
    device: ZwlrVirtualPointerV1,
    /// All monitors together: absolute motion is a fraction of this box.
    extent: Rect,
    held: Vec<u32>,
}

impl Pointer {
    /// A virtual pointer on the first seat. `outputs` is the bounding box
    /// of every monitor, in global logical pixels.
    pub fn new(conn: &Connection, outputs: Rect) -> Result<Self> {
        let (globals, queue) = registry_queue_init::<Quiet>(conn)?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=9, ())?;
        let manager: ZwlrVirtualPointerManagerV1 = globals
            .bind(&qh, 1..=2, ())
            .context("the compositor has no virtual pointer (zwlr_virtual_pointer_v1)")?;
        let device = manager.create_virtual_pointer(Some(&seat), &qh, ());
        Ok(Self {
            conn: conn.clone(),
            _queue: queue,
            device,
            extent: outputs,
            held: Vec::new(),
        })
    }

    /// Does `action` at `at`, then moves the cursor to `back_to` if given.
    pub fn act(&mut self, action: Action, at: Point, back_to: Option<Point>) -> Result<()> {
        self.move_to(at)?;
        sleep(ARRIVE);
        let (button, count) = presses(action);
        for i in 0..count {
            if i > 0 {
                sleep(PRESS);
            }
            self.button(button, true)?;
            sleep(PRESS);
            self.button(button, false)?;
        }
        if let Some(back) = back_to {
            sleep(ARRIVE);
            self.move_to(back)?;
        }
        // Wait until the compositor has handled all of it. Without this, a
        // process that exits right after (`click --at`, `--once`) closes the
        // connection with the last motion unprocessed, and Hyprland drops it:
        // the cursor stayed on the button (found 2026-10-22).
        self.conn.roundtrip()?;
        Ok(())
    }

    /// Releases every button still pressed. Safe to call any number of times.
    pub fn release_all(&mut self) {
        for button in std::mem::take(&mut self.held) {
            self.device.button(now(), button, ButtonState::Released);
            self.device.frame();
        }
        let _ = self.conn.flush();
    }

    fn move_to(&self, at: Point) -> Result<()> {
        // Absolute motion takes unsigned coordinates inside the extent:
        // shift by the extent's origin, and never cast a negative to `u32`,
        // which would wrap to four billion.
        let x = u32::try_from(at.x - self.extent.x).context("point left of every monitor")?;
        let y = u32::try_from(at.y - self.extent.y).context("point above every monitor")?;
        let (w, h) = (u32::try_from(self.extent.w)?, u32::try_from(self.extent.h)?);
        self.device
            .motion_absolute(now(), x.min(w - 1), y.min(h - 1), w, h);
        self.device.frame();
        self.conn.flush()?;
        Ok(())
    }

    fn button(&mut self, button: u32, pressed: bool) -> Result<()> {
        let state = if pressed {
            self.held.push(button);
            ButtonState::Pressed
        } else {
            self.held.retain(|&b| b != button);
            ButtonState::Released
        };
        self.device.button(now(), button, state);
        self.device.frame();
        self.conn.flush()?;
        Ok(())
    }
}

/// The time to stamp an input event with: milliseconds of the monotonic
/// clock, the clock the compositor stamps the real mouse and keyboard with.
///
/// Apps count double clicks by comparing these stamps. Counting from when
/// the pointer was made restarted near zero in every run, so a Firefox
/// click in one run looked like the next click of the run before (spike
/// test 8, 2026-10-23: a triple click counted `3, 1, 2`). The protocol keeps
/// 32 bits of milliseconds, so the clock wraps every 49 days, as the
/// compositor's own stamps do.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // the wrap is the protocol's
pub fn now() -> u32 {
    let t = clock_gettime(ClockId::Monotonic);
    (t.tv_sec as u64 * 1000 + t.tv_nsec as u64 / 1_000_000) as u32
}

impl Drop for Pointer {
    fn drop(&mut self) {
        self.release_all();
        self.device.destroy();
        let _ = self.conn.flush();
    }
}
