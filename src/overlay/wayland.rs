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

use std::{collections::VecDeque, time::Instant};

use smithay_client_toolkit::{
    compositor::CompositorHandler,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            Connection, Proxy, QueueHandle, WEnum,
            protocol::{wl_callback, wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
        },
        protocols::wp::viewporter::client::wp_viewport::WpViewport,
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
        keyboard::{KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            KeyboardInteractivity, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
        xdg::{
            popup::{Popup, PopupConfigure, PopupHandler},
            window::{Window, WindowConfigure, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::Pixmap;

use crate::{
    action::Mods,
    desktop,
    keyboard::{Keyboard, Locks, held, read_keymap},
    keys::{Key, KeyCode},
    overlay::{
        Event, draw,
        gate::{KeyGate, Seen},
        hold::Holds,
        typing::Typist,
    },
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
    /// The size the compositor gave the overlay, once it has, in logical px.
    pub size: Option<(u32, u32)>,
    /// Buffer px per logical px: what the popup's buffer is drawn at.
    pub scale: f64,
    /// Shows the popup's buffer at its logical size, whatever its scale.
    /// `None` without `wp_viewporter`.
    pub viewport: Option<WpViewport>,
    /// The layer's popup, which the tags are drawn on, and whether the
    /// compositor has placed it yet.
    pub popup: Option<Popup>,
    pub placed: bool,
    pub pool: Option<SlotPool>,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
    pub grab: Grab,
    pub gate: KeyGate,
    /// The keys this grab saw go down, to tell the app's releases apart.
    pub seen: Seen,
    /// The arrows held down, for their repeats.
    pub holds: Holds,
    /// Passes the app what the input-method grab takes from it and wayhint
    /// doesn't use (see `keyboard.rs`). Made before the first grab, so a
    /// key let go the moment the grab starts still reaches the app.
    pub passthrough: Option<Keyboard>,
    /// The modifiers held right now, as the compositor last reported them,
    /// and the same as its xkb mask (depressed and latched), for the
    /// characters keys type.
    pub held: Mods,
    pub pressed: u32,
    /// Reads keys as text while text mode is on; `None` otherwise.
    pub typist: Option<Typist>,
    /// The real keyboard's keymap as xkb text, and its locks and layout,
    /// for the virtual keyboard of a modifier click to copy.
    pub keymap: Option<String>,
    pub locks: Locks,
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
        // A new grab is told what's held as it starts, a Ctrl kept down
        // across the click included (Hyprland sends `modifiers` with the
        // keymap). Until then none, so one let go meanwhile can't stick.
        self.held = Mods::NONE;
        self.seen = Seen::default();
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
        self.holds.clear();
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

    /// Maps the layer with one clear pixel. It draws nothing itself, but
    /// its popup shows only while it's mapped.
    pub fn map_layer(&mut self) -> anyhow::Result<()> {
        let (Some(layer), Some(pool)) = (&self.layer, &mut self.pool) else {
            anyhow::bail!("the overlay isn't configured yet");
        };
        let (buffer, canvas) = pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888)?;
        canvas.fill(0);
        buffer.attach_to(layer.wl_surface())?;
        layer.commit();
        Ok(())
    }

    /// The popup's buffer size, in buffer px, once the compositor has given
    /// the overlay its size.
    pub fn buffer_size(&self) -> Option<(u32, u32)> {
        self.size.map(|size| buffer_size(size, self.scale))
    }

    /// Draws a finished pixmap on the overlay's popup.
    pub fn present(&mut self, pixmap: &Pixmap) -> anyhow::Result<()> {
        let (Some(popup), Some((w, h)), Some(pool)) =
            (&self.popup, self.buffer_size(), &mut self.pool)
        else {
            anyhow::bail!("the overlay isn't configured yet");
        };
        // The pixmap is copied row after row with the buffer's width: a
        // pixmap of another size would come out sheared.
        if (pixmap.width(), pixmap.height()) != (w, h) {
            anyhow::bail!("the monitor changed size; run wayhint again");
        }
        let (w, h) = (i32::try_from(w)?, i32::try_from(h)?);
        // A buffer from the pool: a free slot is reused, so memory stays at
        // one or two frames.
        let (buffer, canvas) = pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)?;
        draw::to_argb8888(pixmap, canvas);
        let surface = popup.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        buffer.attach_to(surface)?;
        surface.commit();
        Ok(())
    }

    /// Modifier keys come through here too, as keys nothing in the config
    /// is, so `pick` skips them; what they hold arrives as `modifiers`. The
    /// release of a key pressed before the grab goes on to the app.
    ///
    /// An arrow acts as it goes down, and again while held (`hold.rs`):
    /// it never ends the pick, so acting early leaves no release behind
    /// for the app. Every other key acts as it's let go, in the order the
    /// keys went down (`gate.rs`), except while typing (see
    /// [`Event::Typed`]).
    fn key(&mut self, code: u32, pressed: bool) {
        let code = KeyCode::from_evdev(code);
        if pressed {
            self.seen.press(code);
            let key = Key {
                code,
                shift: self.held.shift,
            };
            if !self.gate.press(key, self.held) {
                return;
            }
            if let Some(typist) = &mut self.typist
                && code != KeyCode::ENTER
            {
                let typed = typist.press(code, self.pressed, self.locks);
                self.gate.forget(code);
                self.events.push_back(Event::Typed(key, self.held, typed));
            } else if code.is_arrow() {
                self.gate.forget(code);
                self.events.push_back(Event::Key(key, self.held));
                self.holds.press(key, self.held, Instant::now());
            }
            return;
        }
        if self.seen.release(code)
            && let Some(passthrough) = &self.passthrough
        {
            passthrough.release(code);
        }
        self.holds.release(code);
        // Acting on release: press and release were both swallowed, so
        // nothing reaches the app once the grab ends. While typing, a key
        // found here went down before typing started (a typed key leaves
        // the gate at its press), and is read as text now.
        for (key, mods) in self.gate.release(code) {
            let event = match &mut self.typist {
                Some(typist) if key.code != KeyCode::ENTER => {
                    Event::Typed(key, mods, typist.press_late(key.code, mods, self.locks))
                }
                _ => Event::Key(key, mods),
            };
            self.events.push_back(event);
        }
    }

    /// The compositor's word on the modifiers held (depressed and latched)
    /// and the locks. It comes right after the key that changed them, so a
    /// modifier newly down was the last key pressed.
    fn modifiers(&mut self, pressed: u32, locks: Locks) {
        if pressed & !self.pressed != 0 {
            self.gate.modifier();
        }
        self.pressed = pressed;
        self.held = held(pressed);
        self.locks = locks;
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
            desktop::warn(
                "wayhint: another input method holds the keyboard; taking focus instead (open menus will close)",
            );
            state.release_keys();
            state.input_methods = None;
            // Focus moves now, and the app is caught up when it comes back.
            state.passthrough = None;
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
        // Keys are matched by position, so tags only need the codes. The
        // modifiers held are the compositor's word, read with the keymap it
        // knows; the keymap and the lock state are kept for modifier clicks.
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Key {
                key,
                state: WEnum::Value(key_state),
                ..
            } => state.key(key, key_state == wl_keyboard::KeyState::Pressed),
            zwp_input_method_keyboard_grab_v2::Event::Keymap { fd, size, .. } => {
                state.keymap = read_keymap(fd, size);
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                state.modifiers(
                    mods_depressed | mods_latched,
                    Locks {
                        locked: mods_locked,
                        group,
                    },
                );
                // The app hears them as it would without the grab, so a
                // Super let go here is let go there too.
                if let Some(passthrough) = &state.passthrough {
                    passthrough.modifiers(mods_depressed, mods_latched, state.locks);
                }
            }
            _ => {}
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
        if self.size.is_some_and(|old| old != size) {
            self.events.push_back(Event::Resized);
        }
        if self.size != Some(size) {
            self.size = Some(size);
            let (w, h) = buffer_size(size, self.scale);
            let bytes = w as usize * h as usize * 4;
            self.pool = SlotPool::new(bytes, &self.shm).ok();
        }
    }
}

/// A buffer for a surface `size` logical px big, at `scale` buffer px each.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // monitor sizes fit a u32
pub fn buffer_size((w, h): (u32, u32), scale: f64) -> (u32, u32) {
    let scaled = |n: u32| (f64::from(n) * scale).round() as u32;
    (scaled(w), scaled(h))
}

/// The scale the overlay draws at, for a monitor at `scale`: all of it
/// with a viewport to show the buffer at the logical size, else only a
/// whole scale, which `set_buffer_scale` takes, else 1. A fractional scale
/// drawn at 1 is blurred up by the compositor, as before.
pub fn drawn_scale(scale: f64, viewport: bool) -> f64 {
    let whole = scale >= 1.0 && scale.fract() == 0.0;
    if (viewport && scale > 0.0) || whole {
        scale
    } else {
        1.0
    }
}

impl PopupHandler for State {
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Popup, _: PopupConfigure) {
        self.placed = true;
    }

    /// Hyprland dismisses only popups that took a grab, and this one
    /// doesn't; if it comes anyway, the tags are gone.
    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Popup) {
        self.events.push_back(Event::Closed);
    }
}

/// There are no windows, but binding xdg-shell binds its decoration
/// manager too, whose events SCTK hands to a window handler.
impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        _: WindowConfigure,
        _: u32,
    ) {
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
    /// The fallback's keymap and lock state, kept as the grab's are.
    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        self.keymap = Some(keymap.as_string());
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        raw: RawModifiers,
        layout: u32,
    ) {
        self.modifiers(
            raw.depressed | raw.latched,
            Locks {
                locked: raw.locked,
                group: layout,
            },
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scaled_monitor_gets_its_physical_pixels() {
        assert_eq!(drawn_scale(1.5, true), 1.5, "a viewport takes any scale");
        assert_eq!(drawn_scale(1.5, false), 1.0, "else a fraction is blurred");
        assert_eq!(drawn_scale(2.0, false), 2.0, "a whole scale needs none");
        assert_eq!(drawn_scale(1.0, false), 1.0);
        // 2560x1440 at 1.5 is 1707x960 logical.
        assert_eq!(buffer_size((1707, 960), 1.5), (2561, 1440));
        assert_eq!(buffer_size((1920, 1080), 2.0), (3840, 2160));
    }
}
