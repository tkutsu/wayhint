//! Routine diagnostics go to the state log. Serious warnings also go to
//! stderr and the notification server, with Hyprland as the fallback.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, IsTerminal as _, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use zbus::zvariant::Value;

use crate::desktop::hyprland;

/// How long a notification stays: long enough to read one sentence. The
/// server may keep it longer, as a shell does while the pointer is on it.
const NOTIFY_MS: i32 = 6000;

/// A notification server that doesn't answer in this long isn't there.
const TIMEOUT: Duration = Duration::from_secs(1);

/// Keep the current log and one previous file, each about 1 MiB.
const LOG_LIMIT: u64 = 1024 * 1024;
static LOG_LOCK: Mutex<()> = Mutex::new(());
static LOG_FAILED: AtomicBool = AtomicBool::new(false);

/// Records routine diagnostics without sending desktop notifications.
pub fn log(message: &str) {
    record("INFO", message);
}

/// Uses the XDG state directory, falling back to the user's home directory.
fn log_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("wayhint/wayhint.log"))
}

/// Logging must not interrupt a run or create another desktop notification.
fn record(level: &str, message: &str) {
    let result = log_path()
        .context("neither XDG_STATE_HOME nor HOME is set")
        .and_then(|path| write_log(&path, level, message).context("writing wayhint's log"));
    if let Err(error) = result {
        if !LOG_FAILED.swap(true, Ordering::Relaxed) {
            eprintln!("wayhint: {error:#}");
        }
        eprintln!("{message}");
    }
}

/// Appends timestamped lines to a private file, rotating before it grows further.
fn write_log(path: &Path, level: &str, message: &str) -> io::Result<()> {
    let _lock = LOG_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("log has no directory"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    if fs::metadata(path).is_ok_and(|m| m.len() >= LOG_LIMIT) {
        fs::rename(path, path.with_extension("log.1"))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    for line in message.lines() {
        writeln!(
            file,
            "{}.{:03} {level} {line}",
            now.as_secs(),
            now.subsec_millis()
        )?;
    }
    Ok(())
}

/// Says `message` on stderr and, unless that's a terminal, in a
/// notification too: a run started from a keybind has nowhere else to say
/// what went wrong, since nobody reads its stderr. From a terminal, once is
/// enough.
pub fn warn(message: &str) {
    record("WARN", message);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn diagnostics_append_privately_and_rotate() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wayhint-log-{}-{id}", std::process::id()));
        let path = dir.join("wayhint.log");
        write_log(&path, "INFO", "no tags in kitty\nscan incomplete").unwrap();
        write_log(&path, "WARN", "keyboard unavailable").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.contains(" INFO no tags in kitty\n"));
        assert!(text.contains(" INFO scan incomplete\n"));
        assert!(text.contains(" WARN keyboard unavailable\n"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(LOG_LIMIT)
            .unwrap();
        write_log(&path, "INFO", "after rotation").unwrap();
        assert_eq!(
            fs::metadata(path.with_extension("log.1")).unwrap().len(),
            LOG_LIMIT
        );
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains(" INFO after rotation\n")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
