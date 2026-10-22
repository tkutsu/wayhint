//! The Wayland side of the overlay: the state SCTK dispatches events into.
//!
//! SCTK's pattern: one `State` struct, and for each kind of Wayland object a
//! handler trait it implements (`LayerShellHandler`, `KeyboardHandler`...).
//! When the compositor sends an event, SCTK calls the matching trait method
//! with `&mut State`. Most methods are required but unused here, so the file
//! is mostly empty bodies; the ones that matter are commented.
//!
//! Objects SCTK doesn't wrap (the input method, the sync callback) get a
//! small "user data" type implementing `Dispatch2`, which is where their
//! events land: [`NoEvents`], [`ImeEvents`], [`Synced`], [`Answered`].

use std::collections::VecDeque;

use smithay_client_toolkit::{
    compositor::CompositorHandler,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            Connection, Proxy, QueueHandle, WEnum,
            protocol::{wl_callback, wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
        },
        protocols_misc::zwp_input_method_v2::client::{
            zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
            zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
            zwp_input_method_v2::{self, ZwpInputMethodV2},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            KeyboardInteractivity, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::Pixmap;

use crate::{
    keys::{Key, KeyCode},
    overlay::{Event, draw, gate::KeyGate},
};

/// How keys reach wayhint right now.
pub enum Grab {
    /// Not taking keys (while clicking).
    Released,
    /// An input-method keyboard grab: keys come to wayhint without moving
    /// keyboard focus, so an open menu keeps its popup grab (spike test 4c).
    InputMethod {
        method: ZwpInputMethodV2,
        grab: ZwpInputMethodKeyboardGrabV2,
    },
    /// The fallback: the overlay takes keyboard focus. Open menus close.
    Exclusive,
}

pub struct State {
    pub registry: RegistryState,
    pub seats: SeatState,
    pub outputs: OutputState,
    pub shm: Shm,
    /// `None` when the compositor has no input-method protocol, or once
    /// another input method (fcitx, IBus) turned out to hold the seat's slot.
    pub input_methods: Option<ZwpInputMethodManagerV2>,
    pub layer: Option<LayerSurface>,
    /// The size the compositor gave the overlay, once it has.
    pub size: Option<(u32, u32)>,
    pub pool: Option<SlotPool>,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
    pub grab: Grab,
    pub gate: KeyGate,
    /// The Shift keys held down right now, by position.
    pub shifts: Vec<KeyCode>,
    /// Events for `Overlay::next_event`, oldest first.
    pub events: VecDeque<Event>,
    /// Set when the compositor answers `Overlay::roundtrip`'s sync.
    pub answered: bool,
}

impl State {
    fn seat(&self) -> Option<wl_seat::WlSeat> {
        self.seats.seats().next()
    }

    /// Starts taking keys: through an input method if there is one,
    /// otherwise by taking keyboard focus.
    pub fn take_keys(&mut self, qh: &QueueHandle<Self>) {
        if !matches!(self.grab, Grab::Released) {
            return;
        }
        // A Shift held across the click was pressed before this grab, and
        // its release may never come here: start with none held.
        self.shifts.clear();
        if let (Some(manager), Some(seat)) = (&self.input_methods, self.seat()) {
            let method = manager.get_input_method(&seat, qh, ImeEvents);
            let grab = method.grab_keyboard(qh, ImeEvents);
            self.grab = Grab::InputMethod { method, grab };
        } else {
            self.take_focus(qh);
        }
    }

    /// The Exclusive fallback.
    fn take_focus(&mut self, qh: &QueueHandle<Self>) {
        if self.keyboard.is_none()
            && let Some(seat) = self.seat()
        {
            self.keyboard = self.seats.get_keyboard(qh, &seat, None).ok();
        }
        if let Some(layer) = &self.layer {
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
        }
        self.grab = Grab::Exclusive;
    }

    /// Hands the keyboard back. The input method is destroyed, not only its
    /// grab, so while wayhint isn't reading keys no app sees an input method
    /// that would never answer it.
    pub fn release_keys(&mut self) {
        match std::mem::replace(&mut self.grab, Grab::Released) {
            Grab::InputMethod { method, grab } => {
                grab.release();
                method.destroy();
            }
            Grab::Exclusive => {
                if let Some(layer) = &self.layer {
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.commit();
                }
            }
            Grab::Released => {}
        }
    }

    /// Draws a finished pixmap on the overlay surface.
    pub fn present(&mut self, pixmap: &Pixmap) -> anyhow::Result<()> {
        let (Some(layer), Some((w, h)), Some(pool)) = (&self.layer, self.size, &mut self.pool)
        else {
            anyhow::bail!("the overlay isn't configured yet");
        };
        let (w, h) = (i32::try_from(w)?, i32::try_from(h)?);
        // A buffer from the pool: a free slot is reused, so memory stays at
        // one or two frames.
        let (buffer, canvas) = pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)?;
        draw::to_argb8888(pixmap, canvas);
        let surface = layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        buffer.attach_to(surface)?;
        layer.commit();
        Ok(())
    }

    fn key(&mut self, code: u32, pressed: bool) {
        let code = KeyCode::from_evdev(code);
        // Shift is never a key of its own here, only state for the others.
        if code.is_shift() {
            self.shifts.retain(|&k| k != code);
            if pressed {
                self.shifts.push(code);
            }
            return;
        }
        if pressed {
            let shift = !self.shifts.is_empty();
            self.gate.press(Key { code, shift });
        } else if let Some(key) = self.gate.release(code) {
            // Acting on release: press and release were both swallowed, so
            // nothing reaches the app once the grab ends.
            self.events.push_back(Event::Key(key));
        }
    }
}

/// User data for objects whose events don't matter (the input-method
/// manager has none).
pub struct NoEvents;

impl<I: Proxy> Dispatch2<I, State> for NoEvents {
    fn event(&self, _: &mut State, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<State>) {}
}

/// User data for a `wl_display.sync` sent after drawing generation `.0`.
pub struct Synced(pub u32);

impl Dispatch2<wl_callback::WlCallback, State> for Synced {
    fn event(
        &self,
        state: &mut State,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.gate.synced(self.0);
        }
    }
}

/// User data for the `wl_display.sync` of `Overlay::roundtrip`.
pub struct Answered;

impl Dispatch2<wl_callback::WlCallback, State> for Answered {
    fn event(
        &self,
        state: &mut State,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.answered = true;
        }
    }
}

/// User data for the input method and its keyboard grab.
pub struct ImeEvents;

impl Dispatch2<ZwpInputMethodV2, State> for ImeEvents {
    fn event(
        &self,
        state: &mut State,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        // `unavailable`: another input method (fcitx, IBus) holds this seat.
        // Fall back to keyboard focus for the rest of the run.
        if let zwp_input_method_v2::Event::Unavailable = event {
            eprintln!(
                "wayhint: another input method holds the keyboard; taking focus instead (open menus will close)"
            );
            state.release_keys();
            state.input_methods = None;
            state.take_focus(qh);
        }
    }
}

impl Dispatch2<ZwpInputMethodKeyboardGrabV2, State> for ImeEvents {
    fn event(
        &self,
        state: &mut State,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // The grab also sends a keymap and modifiers; keys are matched by
        // position, so only the codes matter (Shift is followed by its
        // codes too, in `State::key`).
        if let zwp_input_method_keyboard_grab_v2::Event::Key {
            key,
            state: WEnum::Value(key_state),
            ..
        } = event
        {
            state.key(key, key_state == wl_keyboard::KeyState::Pressed);
        }
    }
}

// SCTK's handler traits from here on.

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.events.push_back(Event::Closed);
    }

    /// The compositor says how big the overlay is: the whole monitor.
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let size = configure.new_size;
        if self.size != Some(size) {
            self.size = Some(size);
            let bytes = size.0 as usize * size.1 as usize * 4;
            self.pool = SlotPool::new(bytes, &self.shm).ok();
        }
    }
}

impl KeyboardHandler for State {
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event.raw_code, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event.raw_code, false);
    }

    /// Losing focus while it's how keys arrive cancels the run.
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        if matches!(self.grab, Grab::Exclusive) {
            self.events.push_back(Event::KeyboardLost);
        }
    }

    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);
