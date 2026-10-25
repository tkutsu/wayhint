//! The overlay: a transparent layer-shell surface over the whole monitor
//! that shows tags and reads keys, and never takes a click.
//!
//! - Drawing: software, into a pixmap the size of the monitor (`draw.rs`),
//!   shown on a popup of the layer, not on the layer itself. Hyprland draws
//!   every layer's popups after all the layers, so a bar's open menu would
//!   cover tags on the layer.
//! - Keys: an input-method keyboard grab, or keyboard focus as the
//!   fallback (`wayland.rs`), filtered by the stale-key rule (`gate.rs`).
//!   What the grab takes and wayhint doesn't use goes on to the app.
//! - Clicks: the surface's input region is empty, so every pointer event
//!   goes straight through to the app underneath.
//!
//! The event loop is `calloop`. Wayland events are one source in it,
//! SIGINT/SIGTERM another, so Ctrl+C is an ordinary [`Event`] and every exit
//! goes through the same cleanup, and Hyprland's events a third, for a run
//! to follow workspace changes.

mod draw;
mod font;
mod gate;
mod wayland;

use std::{
    collections::VecDeque,
    io::{ErrorKind, Read as _},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    output::OutputState,
    reexports::{
        calloop::{
            EventLoop, Interest, Mode, PostAction,
            generic::Generic,
            signals::{Signal, Signals},
        },
        calloop_wayland_source::WaylandSource,
        client::{Connection, QueueHandle, globals::registry_queue_init},
        protocols::xdg::shell::client::xdg_positioner,
    },
    registry::RegistryState,
    seat::SeatState,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell},
        xdg::{XdgPositioner, XdgShell, popup::Popup},
    },
    shm::Shm,
};
use tiny_skia::Pixmap;

pub use self::draw::{Dot, Frame, Tag};
use self::{
    draw::Painter,
    wayland::{Answered, Grab, NoEvents, State, Synced},
};
use crate::{
    action::Mods,
    desktop::{self, Monitor},
    geometry::Point,
    keyboard::{self, Keyboard, Locks},
    keys::Key,
    style::Style,
};

/// Something the run loop has to react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A key was released, and its press counted (see `gate.rs`), with
    /// the modifiers held at its press. Modifier keys come too, and mean
    /// nothing to `pick`.
    /// Shift is in both: in the [`Key`], because `_` and `-` are different
    /// keys to the config, and in the [`Mods`], for a Shift-click.
    Key(Key, Mods),
    /// SIGINT or SIGTERM: leave through the normal cleanup.
    Interrupted,
    /// The compositor closed the overlay (its monitor went away) or
    /// dismissed the popup the tags are on.
    Closed,
    /// The monitor changed size (a new mode or scale). The tags were placed
    /// for the old one and the drawing buffer has the old size, so the run
    /// can't go on.
    Resized,
    /// Keyboard focus left the overlay in the Exclusive fallback.
    KeyboardLost,
    /// The monitor shows another workspace (once
    /// [`Overlay::follow_workspaces`] is on): the tags are for windows that
    /// left.
    Workspace,
}

pub struct Overlay {
    conn: Connection,
    qh: QueueHandle<State>,
    event_loop: EventLoop<'static, State>,
    state: State,
    painter: Painter,
    pixmap: Pixmap,
    monitor: Monitor,
}

impl Overlay {
    /// Puts an empty overlay on `monitor` and starts taking keys. Call it
    /// before anything starts other threads: it blocks SIGINT and SIGTERM
    /// so they arrive as [`Event::Interrupted`], and threads started later
    /// inherit that.
    pub fn new(conn: &Connection, style: &Style, monitor: &Monitor) -> Result<Self> {
        // The font first: a bad `[style] font` fails before any grab.
        let painter = Painter::new(style)?;

        let (globals, mut queue) =
            registry_queue_init::<State>(conn).context("connecting to Wayland")?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor")?;
        let layer_shell =
            LayerShell::bind(&globals, &qh).context("the compositor has no layer-shell")?;
        let xdg_shell = XdgShell::bind(&globals, &qh).context("the compositor has no xdg-shell")?;
        let mut state = State {
            registry: RegistryState::new(&globals),
            seats: SeatState::new(&globals, &qh),
            outputs: OutputState::new(&globals, &qh),
            shm: Shm::bind(&globals, &qh).context("wl_shm")?,
            input_methods: globals.bind(&qh, 1..=1, NoEvents).ok(),
            layer: None,
            size: None,
            popup: None,
            placed: false,
            pool: None,
            keyboard: None,
            grab: Grab::Released,
            gate: gate::KeyGate::default(),
            seen: gate::Seen::default(),
            passthrough: None,
            held: Mods::NONE,
            keymap: None,
            locks: Locks::default(),
            events: VecDeque::new(),
            answered: false,
        };
        // Two round trips: the first lists outputs and seats, the second
        // brings their names and capabilities.
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;

        let output = state.outputs.outputs().find(|o| {
            state.outputs.info(o).and_then(|i| i.name).as_deref() == Some(monitor.name.as_str())
        });
        let layer = layer_shell.create_layer_surface(
            &qh,
            compositor.create_surface(&qh),
            Layer::Overlay,
            Some("wayhint"),
            output.as_ref(),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        // -1: ignore other surfaces' reserved space (the bar), cover all of it.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // An empty input region: the pointer never lands on the overlay.
        let empty = Region::new(&compositor)?;
        layer.wl_surface().set_input_region(Some(empty.wl_region()));
        layer.commit();
        state.layer = Some(layer);
        while state.size.is_none() {
            queue.blocking_dispatch(&mut state)?;
        }
        let (w, h) = state.size.unwrap_or_default();
        let pixmap = Pixmap::new(w, h).with_context(|| format!("overlay size {w}x{h}"))?;

        // The popup the tags are drawn on: the layer's size, at its corner.
        // It takes no grab, so the compositor never dismisses it, and an
        // open menu of another app keeps its own.
        state.map_layer()?;
        let positioner = XdgPositioner::new(&xdg_shell)?;
        positioner.set_size(i32::try_from(w)?, i32::try_from(h)?);
        positioner.set_anchor_rect(0, 0, 1, 1);
        positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
        positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
        let popup = Popup::from_surface(
            None,
            &positioner,
            &qh,
            compositor.create_surface(&qh),
            &xdg_shell,
        )?;
        if let Some(layer) = &state.layer {
            layer.get_popup(popup.xdg_popup());
        }
        popup.wl_surface().set_input_region(Some(empty.wl_region()));
        popup.wl_surface().commit();
        state.popup = Some(popup);
        while !state.placed {
            queue.blocking_dispatch(&mut state)?;
        }

        let event_loop = EventLoop::try_new()?;
        WaylandSource::new(conn.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|e| anyhow::anyhow!("adding Wayland to the event loop: {}", e.error))?;
        let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;
        event_loop
            .handle()
            .insert_source(signals, |_, (), state: &mut State| {
                state.events.push_back(Event::Interrupted);
            })
            .map_err(|e| anyhow::anyhow!("adding signals to the event loop: {}", e.error))?;

        let mut overlay = Self {
            conn: conn.clone(),
            qh,
            event_loop,
            state,
            painter,
            pixmap,
            monitor: monitor.clone(),
        };
        overlay.clear()?;
        overlay.grab_keyboard()?;
        Ok(overlay)
    }

    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    /// Listens to Hyprland for this monitor showing another workspace,
    /// which comes as [`Event::Workspace`]. Changes heard while one is
    /// still waiting to be taken make no second one.
    pub fn follow_workspaces(&mut self) -> Result<()> {
        let socket = desktop::events()?;
        let monitor = self.monitor.name.clone();
        // A read can end mid-line: the rest comes with the next one.
        let mut pending = Vec::new();
        let source = Generic::new(socket, Interest::READ, Mode::Level);
        self.event_loop
            .handle()
            .insert_source(source, move |_, socket, state: &mut State| {
                let mut buffer = [0; 4096];
                loop {
                    match (&**socket).read(&mut buffer) {
                        Ok(n) if n > 0 => pending.extend_from_slice(&buffer[..n]),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        // Hyprland hung up: the tags stop following, and
                        // the run goes on.
                        _ => return Ok(PostAction::Remove),
                    }
                }
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    let line = String::from_utf8_lossy(&line);
                    if desktop::changes_workspace(line.trim_end(), &monitor)
                        && !state.events.contains(&Event::Workspace)
                    {
                        state.events.push_back(Event::Workspace);
                    }
                }
                Ok(PostAction::Continue)
            })
            .map_err(|e| {
                anyhow::anyhow!("adding Hyprland's events to the event loop: {}", e.error)
            })?;
        Ok(())
    }

    /// Which way keys come in, for messages.
    pub fn keyboard_mode(&self) -> &'static str {
        match self.state.grab {
            Grab::InputMethod { .. } => "input-method grab",
            Grab::Exclusive => "keyboard focus (fallback)",
            Grab::Released => "released",
        }
    }

    /// Whether keys come through the input-method grab, which takes no
    /// keyboard focus: an app can be clicked while wayhint holds it.
    pub fn grabs_without_focus(&self) -> bool {
        matches!(self.state.grab, Grab::InputMethod { .. })
    }

    /// The real keyboard's keymap, once the grab (or focus) has sent it.
    pub fn keymap(&self) -> Option<&str> {
        self.state.keymap.as_deref()
    }

    /// The real keyboard's locks and layout, as last reported.
    pub fn locks(&self) -> Locks {
        self.state.locks
    }

    /// Draws a new set of tags (`frame` in global coordinates) and starts
    /// accepting keys for them once the compositor confirms the frame:
    /// every key pressed before that was aimed at something else.
    pub fn show(&mut self, frame: &Frame) -> Result<()> {
        self.update(frame)?;
        let generation = self.new_tags();
        self.conn.display().sync(&self.qh, Synced(generation));
        self.conn.flush()?;
        Ok(())
    }

    /// Redraws the same tags (a letter or an arrow was typed). Keys stay
    /// live: a fast second letter must not count as stale.
    pub fn update(&mut self, frame: &Frame) -> Result<()> {
        let origin = self.monitor.rect.origin();
        let to_local = |p: Point| Point::new(p.x - origin.x, p.y - origin.y);
        let local = Frame {
            tags: frame
                .tags
                .iter()
                .map(|t| Tag {
                    at: to_local(t.at),
                    ..t.clone()
                })
                .collect(),
            dots: frame
                .dots
                .iter()
                .map(|d| Dot {
                    from: to_local(d.from),
                    at: to_local(d.at),
                })
                .collect(),
        };
        self.painter.paint(&mut self.pixmap, &local);
        self.state.present(&self.pixmap)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Shows nothing. Keys are refused until the next `show`, and a
    /// workspace change heard by now is old news: the next scan sees it.
    /// Without that, a click on a bar's workspace button would scan twice.
    pub fn clear(&mut self) -> Result<()> {
        // Reads what came in, without waiting, so the change is heard here
        // and not after the scan.
        self.event_loop
            .dispatch(Some(Duration::ZERO), &mut self.state)?;
        self.state.events.retain(|e| *e != Event::Workspace);
        self.painter.paint(&mut self.pixmap, &Frame::default());
        self.state.present(&self.pixmap)?;
        self.new_tags();
        self.conn.flush()?;
        Ok(())
    }

    /// Starts a new tag generation: the gate refuses keys until the frame
    /// is confirmed, and keys read but not yet taken are dropped. Both were
    /// aimed at the old tags. Two taps can arrive in one read; after the
    /// first picks a target, the second would pick from the next scan.
    fn new_tags(&mut self) -> u32 {
        self.state.events.retain(|e| !matches!(e, Event::Key(..)));
        self.state.gate.redrawn()
    }

    /// Waits for the next thing to react to, until `until` if given: `None`
    /// when that time comes first.
    pub fn next_event(&mut self, until: Option<Instant>) -> Result<Option<Event>> {
        loop {
            if let Some(event) = self.state.events.pop_front() {
                return Ok(Some(event));
            }
            let wait = until.map(|until| until.saturating_duration_since(Instant::now()));
            if wait == Some(Duration::ZERO) {
                return Ok(None);
            }
            self.event_loop.dispatch(wait, &mut self.state)?;
        }
    }

    /// Stops taking keys and waits until the compositor has processed that,
    /// so a click right after lands with the app's keyboard back.
    pub fn release_keyboard(&mut self) -> Result<()> {
        self.state.release_keys();
        self.roundtrip()
    }

    pub fn grab_keyboard(&mut self) -> Result<()> {
        // The grab sends its keymap once it has started, which is too late:
        // the app must be reachable from the grab's first event.
        if self.state.input_methods.is_some() && self.state.passthrough.is_none() {
            match keyboard::seat_keymap().and_then(|keymap| Keyboard::new(&self.conn, &keymap)) {
                Ok(passthrough) => self.state.passthrough = Some(passthrough),
                Err(e) => desktop::warn(&format!(
                    "wayhint: a key let go now may stay held in the app: {e:#}"
                )),
            }
        }
        self.state.take_keys(&self.qh);
        // Let the grab's `unavailable`, if it comes, arrive before any key.
        self.roundtrip()?;
        if matches!(self.state.grab, Grab::Released) {
            bail!("couldn't take the keyboard");
        }
        Ok(())
    }

    /// Waits until the compositor has handled every request sent so far,
    /// and handles every event it sent on the way.
    ///
    /// `Connection::roundtrip` waits the same but only queues those events,
    /// so their handlers run later. After a release in the fallback mode,
    /// the keyboard's `leave` would then run after the next grab and read as
    /// focus being taken away: the run would end after its first click.
    fn roundtrip(&mut self) -> Result<()> {
        self.state.answered = false;
        self.conn.display().sync(&self.qh, Answered);
        self.conn.flush()?;
        while !self.state.answered {
            self.event_loop.dispatch(None, &mut self.state)?;
        }
        Ok(())
    }
}

impl Drop for Overlay {
    /// The backstop: the run loop releases everything itself, but an early
    /// return or a panic still hands the keyboard back and removes the
    /// overlay. Errors can't be returned from `drop`, so they're ignored.
    fn drop(&mut self) {
        self.state.release_keys();
        // The popup before its parent; dropping each destroys it.
        self.state.popup = None;
        self.state.layer = None;
        let _ = self.conn.flush();
    }
}
