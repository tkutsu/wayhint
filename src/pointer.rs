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
        protocol::{
            wl_pointer::{Axis, AxisSource, ButtonState},
            wl_registry,
            wl_seat::WlSeat,
        },
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
/// One wheel notch, as a mouse sends it: 15 in the axis's units, one step.
const NOTCH: f64 = 15.0;
/// A drag moves in this many steps, `STEP` apart: apps start a drag once
/// the pointer has moved a few px with the button down, and some want to
/// see it move more than once.
const STEPS: i32 = 12;
const STEP: Duration = Duration::from_millis(8);

/// The button a click presses, and how many times: a hover or a scroll,
/// none.
const fn presses(action: Action) -> (u32, u32) {
    match action {
        Action::Left | Action::Hold => (BTN_LEFT, 1),
        Action::Double => (BTN_LEFT, 2),
        Action::Right => (BTN_RIGHT, 1),
        Action::Middle => (BTN_MIDDLE, 1),
        Action::Back => (BTN_SIDE, 1),
        Action::Forward => (BTN_EXTRA, 1),
        Action::Hover | Action::ScrollUp | Action::ScrollDown => (BTN_LEFT, 0),
    }
}

/// The pointer's own event queue needs a state type. None of these objects
/// sends anything wayhint reads, so it's empty. The virtual keyboard uses
/// it too.
pub struct Quiet;

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
    /// Wheel notches a scroll turns: `[pointer] scroll`.
    notches: u16,
}

impl Pointer {
    /// A virtual pointer on the first seat. `outputs` is the bounding box
    /// of every monitor, in global logical pixels; a scroll turns the wheel
    /// `notches` notches.
    pub fn new(conn: &Connection, outputs: Rect, notches: u16) -> Result<Self> {
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
            notches,
        })
    }

    /// Moves to `at` and does `action` there. The cursor stays; put it back
    /// with [`Pointer::put_back`], after letting go of any modifiers. A
    /// hold leaves its button down, for [`Pointer::drag`] to let go.
    pub fn act(&mut self, action: Action, at: Point) -> Result<()> {
        self.move_to(at)?;
        sleep(ARRIVE);
        match action {
            Action::ScrollUp => self.scroll(-1)?,
            Action::ScrollDown => self.scroll(1)?,
            Action::Hold => self.button(BTN_LEFT, true)?,
            _ => {
                let (button, count) = presses(action);
                for i in 0..count {
                    if i > 0 {
                        sleep(PRESS);
                    }
                    self.button(button, true)?;
                    sleep(PRESS);
                    self.button(button, false)?;
                }
            }
        }
        self.handled()
    }

    /// Moves from `from` to `to` with the held button still down, in
    /// steps, and lets go there: the end of a hold. From a point to
    /// itself, it's a long press.
    pub fn drag(&mut self, from: Point, to: Point) -> Result<()> {
        for i in 1..=STEPS {
            let along = |a: i32, b: i32| a + (b - a) * i / STEPS;
            self.move_to(Point::new(along(from.x, to.x), along(from.y, to.y)))?;
            sleep(STEP);
        }
        sleep(PRESS);
        for button in self.held.clone() {
            self.button(button, false)?;
        }
        self.handled()
    }

    /// Turns the wheel down, or up for a `way` of -1, one frame a notch,
    /// as a mouse wheel sends them.
    fn scroll(&mut self, way: i32) -> Result<()> {
        for _ in 0..self.notches {
            self.device.axis_source(AxisSource::Wheel);
            self.device
                .axis_discrete(now(), Axis::VerticalScroll, NOTCH * f64::from(way), way);
            self.device.frame();
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Moves the cursor to `to` at once, with a held button still down: an
    /// arrow moving it.
    pub fn go(&mut self, to: Point) -> Result<()> {
        self.move_to(to)?;
        self.handled()
    }

    /// Moves the cursor to `to` after a pause, so the app sees the click
    /// end before the pointer leaves.
    pub fn put_back(&mut self, to: Point) -> Result<()> {
        sleep(ARRIVE);
        self.move_to(to)?;
        self.handled()
    }

    /// Waits until the compositor has handled everything sent. Without
    /// this, a process that exits right after (`click --at`, `--once`)
    /// closes the connection with the last motion unprocessed, and Hyprland
    /// drops it: the cursor stayed on the button (found 2026-10-22).
    fn handled(&self) -> Result<()> {
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
