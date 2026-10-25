//! Warnings for a run nobody watches: on stderr, and as a desktop
//! notification from the shell's notification server, over D-Bus. Hyprland's
//! own notification is the fallback when no server answers.

use std::{collections::HashMap, io::IsTerminal as _, time::Duration};

use anyhow::{Context as _, Result};
use zbus::zvariant::Value;

use crate::desktop::hyprland;

/// How long a notification stays: long enough to read one sentence. The
/// server may keep it longer, as a shell does while the pointer is on it.
const NOTIFY_MS: i32 = 6000;

/// A notification server that doesn't answer in this long isn't there.
const TIMEOUT: Duration = Duration::from_secs(1);

/// Says `message` on stderr and, unless that's a terminal, in a
/// notification too: a run started from a keybind has nowhere else to say
/// what went wrong, since nobody reads its stderr. From a terminal, once is
/// enough.
pub fn warn(message: &str) {
    eprintln!("{message}");
    if !std::io::stderr().is_terminal() && notify(message).is_err() {
        let _ = hyprland::notify(message, NOTIFY_MS);
    }
}

/// Sends `message` to the notification server, titled "wayhint" (its
/// `wayhint: ` prefix goes), lines and all. On a thread with a runtime of
/// its own: a warning can come from inside the scan's runtime, and blocking
/// on a second one there panics.
fn notify(message: &str) -> Result<()> {
    let body = message.strip_prefix("wayhint: ").unwrap_or(message);
    // The body may hold markup, so `<`, `>` and `&` are written as entities.
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let send = async {
        let connection = zbus::Connection::session().await?;
        let actions: Vec<&str> = Vec::new();
        let hints: HashMap<&str, Value> = HashMap::new();
        connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "wayhint",
                    0u32,
                    "dialog-warning",
                    "wayhint",
                    body.as_str(),
                    actions,
                    hints,
                    NOTIFY_MS,
                ),
            )
            .await?;
        anyhow::Ok(())
    };
    std::thread::scope(|s| {
        s.spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime
                // The timer is made inside the runtime: it needs its clock.
                .block_on(async { tokio::time::timeout(TIMEOUT, send).await })
                .context("the notification server didn't answer")?
        })
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}
