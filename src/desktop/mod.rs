//! Everything about the world outside wayhint: the compositor and the
//! accessibility tree. The rest of the program sees plain data (`Window`,
//! `Monitor`, later the scan's snapshot), never a socket or a D-Bus proxy.

mod hyprland;

pub use hyprland::{Monitor, Window, cursor, focused_window, keyboard_layout, monitors};
