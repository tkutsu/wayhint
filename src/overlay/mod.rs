//! The overlay: a transparent layer-shell surface over the whole monitor
//! that shows tags and reads keys, and never takes a click.
//!
//! - Drawing: software, into a pixmap the size of the monitor (`draw.rs`).
//! - Keys: an input-method keyboard grab, or keyboard focus as the
//!   fallback (`wayland.rs`), filtered by the stale-key rule (`gate.rs`).
//! - Clicks: the surface's input region is empty, so every pointer event
//!   goes straight through to the app underneath.
//!
//! The event loop is `calloop`. Wayland events are one source in it and
//! SIGINT/SIGTERM another, so Ctrl+C is an ordinary [`Event`] and every exit
//! goes through the same cleanup.

mod draw;
mod font;
mod gate;
mod wayland;

use std::collections::VecDeque;

use anyhow::{Context as _, Result, bail};
use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    output::OutputState,
    reexports::{
        calloop::{
            EventLoop,
            signals::{Signal, Signals},
        },
        calloop_wayland_source::WaylandSource,
        client::{Connection, QueueHandle, globals::registry_queue_init},
    },
    registry::RegistryState,
    seat::SeatState,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell},
    },
    shm::Shm,
};
use tiny_skia::Pixmap;

pub use self::draw::Tag;
use self::{
    draw::Painter,
    wayland::{Answered, Grab, NoEvents, State, Synced},
};
use crate::{
    action::Mods, desktop::Monitor, geometry::Point, keyboard::Locks, keys::Key, style::Style,
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
    /// The compositor closed the overlay (its monitor went away).
    Closed,
    /// The monitor changed size (a new mode or scale). The tags were placed
    /// for the old one and the drawing buffer has the old size, so the run
    /// can't go on.
    Resized,
    /// Keyboard focus left the overlay in the Exclusive fallback.
    KeyboardLost,
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
        let mut state = State {
            registry: RegistryState::new(&globals),
            seats: SeatState::new(&globals, &qh),
            outputs: OutputState::new(&globals, &qh),
            shm: Shm::bind(&globals, &qh).context("wl_shm")?,
            input_methods: globals.bind(&qh, 1..=1, NoEvents).ok(),
            layer: None,
            size: None,
            pool: None,
            keyboard: None,
            grab: Grab::Released,
            gate: gate::KeyGate::default(),
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

    /// Draws a new set of `tags` (their points in global coordinates) and
    /// starts accepting keys for them once the compositor confirms the
    /// frame: every key pressed before that was aimed at something else.
    pub fn show(&mut self, tags: &[Tag]) -> Result<()> {
        self.update(tags)?;
        let generation = self.new_tags();
        self.conn.display().sync(&self.qh, Synced(generation));
        self.conn.flush()?;
        Ok(())
    }

    /// Redraws the same tags (a letter was typed). Keys stay live: a fast
    /// second letter must not count as stale.
    pub fn update(&mut self, tags: &[Tag]) -> Result<()> {
        let origin = self.monitor.rect.origin();
        let local: Vec<Tag> = tags
            .iter()
            .map(|t| Tag {
                at: Point::new(t.at.x - origin.x, t.at.y - origin.y),
                ..t.clone()
            })
            .collect();
        self.painter.paint(&mut self.pixmap, &local);
        self.state.present(&self.pixmap)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Shows nothing. Keys are refused until the next `show`.
    pub fn clear(&mut self) -> Result<()> {
        self.painter.paint(&mut self.pixmap, &[]);
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

    /// Waits for the next thing to react to.
    pub fn next_event(&mut self) -> Result<Event> {
        loop {
            if let Some(event) = self.state.events.pop_front() {
                return Ok(event);
            }
            self.event_loop.dispatch(None, &mut self.state)?;
        }
    }

    /// Stops taking keys and waits until the compositor has processed that,
    /// so a click right after lands with the app's keyboard back.
    pub fn release_keyboard(&mut self) -> Result<()> {
        self.state.release_keys();
        self.roundtrip()
    }

    pub fn grab_keyboard(&mut self) -> Result<()> {
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
        self.state.layer = None; // dropping the LayerSurface destroys it
        let _ = self.conn.flush();
    }
}
