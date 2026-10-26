//! Which menus are open, as the apps announce them.
//!
//! An app's context menu is a popup of its window, and Hyprland lists no
//! client's popups. The accessibility tree doesn't help either: Qt leaves
//! popups out of the application's children, so Dolphin's open menu has
//! the application as its parent and no way down to it, and Firefox keeps
//! its menus in the window's tree but never marks them "showing" on
//! Wayland (both checked in a nested Hyprland, 2026-10-26). What both do
//! is say so as a menu shows and as it goes: `object:state-changed:showing`.
//!
//! So a thread listens for that one event for the whole run, on a
//! connection of its own, and keeps the list. A menu that opened before
//! the run started was never announced, and stays unseen.

use std::{
    pin::pin,
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use atspi::{
    Event, Role, State,
    events::ObjectEvents,
    proxy::{accessible::AccessibleProxy, registry::RegistryProxy},
};
use atspi_connection::AccessibilityConnection;
use futures::StreamExt as _;
use zbus::{MatchRule, proxy::CacheProperties};

use crate::desktop::tree::ObjectId;

/// How long the registration may take before the run goes on without it.
const READY_TIMEOUT: Duration = Duration::from_millis(500);

/// How long an app may take to say what a newly shown object is.
const ROLE_TIMEOUT: Duration = Duration::from_millis(300);

/// Only "showing": apps send the bus the events someone registered for,
/// and every other state change would be noise here.
const EVENT: &str = "object:state-changed:showing";
const RULE: &str = "type='signal',interface='org.a11y.atspi.Event.Object',\
    member='StateChanged',arg0='showing'";

/// The open menus, oldest first, kept up to date by the listening thread.
pub struct Menus {
    open: Arc<Mutex<Vec<ObjectId>>>,
}

impl Menus {
    /// Starts listening, and returns once apps have been told to send the
    /// event, so a menu the first click opens is heard.
    pub fn watch() -> Result<Self> {
        let open = Arc::new(Mutex::new(Vec::new()));
        let (ready, registered) = mpsc::channel();
        let list = Arc::clone(&open);
        thread::Builder::new().name("menus".into()).spawn(move || {
            let listened = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(listen(&list, &ready)));
            // Before registering, the error goes to `watch`; after, nobody
            // is waiting, and the menus simply stop being followed.
            let _ = ready.send(listened);
        })?;
        registered
            .recv_timeout(READY_TIMEOUT)
            .map_err(|_| anyhow!("the accessibility bus didn't answer"))??;
        Ok(Self { open })
    }

    /// The menus open now, oldest first.
    pub fn open(&self) -> Vec<ObjectId> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Registers for the event, says so on `ready`, then follows the menus
/// into `open` until the bus goes away.
async fn listen(open: &Mutex<Vec<ObjectId>>, ready: &mpsc::Sender<Result<()>>) -> Result<()> {
    let a11y = AccessibilityConnection::new().await?;
    let conn = a11y.connection();
    // The stream first: it gets every message from its making on.
    let mut events = pin!(a11y.event_stream());
    zbus::fdo::DBusProxy::new(conn)
        .await?
        .add_match_rule(MatchRule::try_from(RULE)?)
        .await?;
    RegistryProxy::new(conn)
        .await?
        .register_event(EVENT)
        .await
        .context("registering for menus")?;
    let _ = ready.send(Ok(()));
    while let Some(event) = events.next().await {
        let Ok(Event::Object(ObjectEvents::StateChanged(event))) = event else {
            continue;
        };
        if event.state != State::Showing {
            continue;
        }
        let object = ObjectId {
            bus: event.item.name_as_str().unwrap_or_default().to_string(),
            path: event.item.path_as_str().to_string(),
        };
        // Gone, the object can't say what it was: forget it whatever it was.
        if !event.enabled {
            lock(open).retain(|m| *m != object);
        } else if is_menu(conn, &object).await {
            lock(open).push(object);
        }
    }
    Ok(())
}

fn lock(open: &Mutex<Vec<ObjectId>>) -> std::sync::MutexGuard<'_, Vec<ObjectId>> {
    open.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `object` is a menu: Qt's are popup menus, Firefox's menus.
async fn is_menu(conn: &zbus::Connection, object: &ObjectId) -> bool {
    let role = async {
        let proxy: AccessibleProxy = zbus::proxy::Builder::new(conn)
            .destination(object.bus.clone())?
            .path(object.path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        proxy.get_role().await
    };
    matches!(
        tokio::time::timeout(ROLE_TIMEOUT, role).await,
        Ok(Ok(Role::PopupMenu | Role::Menu))
    )
}
