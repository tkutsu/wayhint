//! Holding modifiers around a click, through the compositor's virtual
//! keyboard (`zwp_virtual_keyboard_v1`).
//!
//! A virtual pointer carries no modifiers: an app reads Shift and Ctrl
//! from the keyboard. So a Ctrl-click is Ctrl pressed on a keyboard, the
//! click, then Ctrl let go, and the keyboard is a virtual one. Spike test 9
//! (2026-10-23) did exactly this: a background tab in Firefox, a range
//! selected in Dolphin, and both layouts typed as before afterwards.
//!
//! The compositor gives the app the keymap of whichever keyboard spoke
//! last, so the virtual one is handed a copy of the real one's, and keeps
//! its layout and locks: a US keymap here would leave the app typing US
//! until the next real key, and Num Lock would read as off.
//!
//! Every modifier pressed is written down and released by
//! [`Keyboard::release_all`]; `Drop` calls it too, as a backstop. A Ctrl
//! left held makes every later click on the desktop a Ctrl-click.
//!
//! The real keyboard's keymap is read here too: [`seat_keymap`] asks for it
//! before anything else, and the config is decoded with it.

use std::{
    fs::File,
    io::Write as _,
    os::{
        fd::{AsFd as _, OwnedFd},
        unix::fs::FileExt as _,
    },
};

use anyhow::{Context as _, Result};
use rustix::fs::{MemfdFlags, memfd_create};
use smithay_client_toolkit::reexports::{
    client::{
        Connection, Dispatch, EventQueue, Proxy as _, QueueHandle, WEnum,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{
            wl_keyboard::{self, WlKeyboard},
            wl_registry::{self, WlRegistry},
            wl_seat::WlSeat,
        },
    },
    protocols_misc::zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    },
};

use crate::{
    action::Mods,
    keys::KeyCode,
    pointer::{Quiet, now},
};

smithay_client_toolkit::reexports::client::delegate_noop!(Quiet: ZwpVirtualKeyboardManagerV1);
smithay_client_toolkit::reexports::client::delegate_noop!(Quiet: ZwpVirtualKeyboardV1);

/// `wl_keyboard`'s keymap format for an xkb text keymap.
const XKB_V1: u32 = 1;
const PRESSED: u32 = 1;
const RELEASED: u32 = 0;

/// The real keyboard's lock and layout state, copied into every modifier
/// event the virtual keyboard sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Locks {
    /// Caps Lock and Num Lock, as an xkb modifier mask.
    pub locked: u32,
    /// Which layout is active: 0 for the first, 1 for the second (Greek).
    pub group: u32,
}

// The modifiers' xkb masks. The eight real modifiers have fixed places in
// every xkb keymap (Shift is bit 0, Control bit 2, Mod1 bit 3), and Alt is
// Mod1 in all of them, so these don't need the keymap read.
const SHIFT: u32 = 1 << 0;
const CONTROL: u32 = 1 << 2;
const MOD1: u32 = 1 << 3;

pub struct Keyboard {
    conn: Connection,
    // Kept alive with the keyboard, as `Pointer` keeps its own.
    _queue: EventQueue<Quiet>,
    device: ZwpVirtualKeyboardV1,
    held: Vec<KeyCode>,
    locks: Locks,
}

impl Keyboard {
    /// A virtual keyboard on the first seat, with `keymap` (xkb text, as
    /// the real keyboard sent it).
    pub fn new(conn: &Connection, keymap: &str) -> Result<Self> {
        let (globals, queue) = registry_queue_init::<Quiet>(conn)?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=9, ())?;
        let manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("the compositor has no virtual keyboard (zwp_virtual_keyboard_v1)")?;
        let device = manager.create_virtual_keyboard(&seat, &qh, ());

        // A keymap travels as a file descriptor. A memfd is a file that
        // lives only in memory, so there's no path to make or clean up.
        // Keymaps are sent with the C string's closing zero.
        let mut file = File::from(memfd_create("wayhint-keymap", MemfdFlags::CLOEXEC)?);
        file.write_all(keymap.as_bytes())?;
        file.write_all(&[0])?;
        let size = u32::try_from(keymap.len() + 1)?;
        device.keymap(XKB_V1, file.as_fd(), size);
        conn.flush()?;
        Ok(Self {
            conn: conn.clone(),
            _queue: queue,
            device,
            held: Vec::new(),
            locks: Locks::default(),
        })
    }

    /// Presses the keys for `mods` and tells the compositor they're down,
    /// keeping `locks`. Call [`Keyboard::release_all`] after the click.
    pub fn hold(&mut self, mods: Mods, locks: Locks) -> Result<()> {
        let mut depressed = 0;
        for (wanted, key, mask) in [
            (mods.shift, KeyCode::LEFT_SHIFT, SHIFT),
            (mods.ctrl, KeyCode::LEFT_CTRL, CONTROL),
            (mods.alt, KeyCode::LEFT_ALT, MOD1),
        ] {
            if wanted {
                self.device.key(now(), key.evdev(), PRESSED);
                self.held.push(key);
                depressed |= mask;
            }
        }
        self.locks = locks;
        self.device
            .modifiers(depressed, 0, locks.locked, locks.group);
        self.conn.flush()?;
        Ok(())
    }

    /// Lets go of every modifier still held. Safe to call any number of
    /// times.
    pub fn release_all(&mut self) {
        if self.held.is_empty() {
            return;
        }
        for key in std::mem::take(&mut self.held) {
            self.device.key(now(), key.evdev(), RELEASED);
        }
        let Locks { locked, group } = self.locks;
        self.device.modifiers(0, 0, locked, group);
        let _ = self.conn.flush();
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.release_all();
        self.device.destroy();
        let _ = self.conn.flush();
    }
}

/// The keymap your keyboard types with, as xkb text: the one the compositor
/// gives every app. It knows what a layout name can't say, such as a keymap
/// file. A `wl_keyboard` is sent its keymap as soon as it's made, so this
/// needs no window and no focus, and runs before any grab.
pub fn seat_keymap() -> Result<String> {
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init::<SeatKeymap>(&conn)?;
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=9, ())?;
    let keyboard = seat.get_keyboard(&qh, ());
    let mut state = SeatKeymap(None);
    queue.roundtrip(&mut state)?;
    // `release` exists from version 3; older keyboards end with the
    // connection.
    if keyboard.version() >= 3 {
        keyboard.release();
    }
    state
        .0
        .context("the compositor sent no keymap for your keyboard")
}

/// [`seat_keymap`]'s queue state: the keymap, once it has come.
struct SeatKeymap(Option<String>);

impl Dispatch<WlRegistry, GlobalListContents> for SeatKeymap {
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

smithay_client_toolkit::reexports::client::delegate_noop!(SeatKeymap: ignore WlSeat);

impl Dispatch<WlKeyboard, ()> for SeatKeymap {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Keys, focus and repeat rate don't matter here; this keyboard is
        // let go right after its first event.
        if let wl_keyboard::Event::Keymap {
            format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
            fd,
            size,
        } = event
        {
            state.0 = read_keymap(fd, size);
        }
    }
}

/// Reads a keymap the compositor sent as a file descriptor. `read_exact_at`
/// reads from the start without moving the file's position, which the
/// compositor may share with every other client it sent the keymap to.
pub fn read_keymap(fd: OwnedFd, size: u32) -> Option<String> {
    let mut bytes = vec![0; usize::try_from(size).ok()?];
    File::from(fd).read_exact_at(&mut bytes, 0).ok()?;
    // A keymap ends with a C string's closing zero.
    let text = bytes.split(|&b| b == 0).next()?;
    String::from_utf8(text.to_vec()).ok()
}
