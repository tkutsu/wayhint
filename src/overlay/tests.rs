use super::*;

/// Read through hyprctl independently of the guard's IPC implementation.
fn cursor_hiding() -> bool {
    let output = std::process::Command::new("hyprctl")
        .args(["getoption", "cursor:hide_on_key_press", "-j"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["bool"]
        .as_bool()
        .unwrap_or_else(|| value["int"].as_i64().unwrap() != 0)
}

#[test]
#[ignore = "briefly grabs the live Hyprland keyboard; run alone with hiding on keypress enabled"]
fn cursor_hiding_is_scoped_to_the_keyboard_grab() -> Result<()> {
    assert!(
        cursor_hiding(),
        "enable hide_on_key_press for this regression check"
    );
    let monitor = desktop::monitors()?
        .into_iter()
        .find(|m| m.focused)
        .context("no focused monitor")?;
    let conn = Connection::connect_to_env()?;
    let style = Style::default();
    let mut overlay = Overlay::new(
        &conn,
        &style,
        &Marks::new(&style),
        &Input::new(&style),
        &Info::default(),
        &monitor,
    )?;
    assert!(!cursor_hiding(), "keys must not hide Wayhint's pointer");
    overlay.grab_keyboard()?;
    assert!(!cursor_hiding(), "a repeated grab keeps the override");
    overlay.release_keyboard()?;
    assert!(
        cursor_hiding(),
        "ordinary typing regains the user's setting"
    );
    overlay.release_keyboard()?;
    assert!(cursor_hiding(), "repeated cleanup preserves the preference");
    overlay.grab_keyboard()?;
    assert!(!cursor_hiding(), "the next round suspends hiding again");
    // This is the same drop path as an early error or an unwinding panic.
    drop(overlay);
    assert!(cursor_hiding(), "dropping the overlay restores the setting");

    // Simulate a user who already keeps the pointer visible. The overlay
    // must not turn hiding on when it returns their keyboard.
    let preference = desktop::CursorVisibility::new()?;
    let overlay = Overlay::new(
        &conn,
        &style,
        &Marks::new(&style),
        &Input::new(&style),
        &Info::default(),
        &monitor,
    )?;
    drop(overlay);
    assert!(!cursor_hiding(), "an existing false preference stays false");
    drop(preference);
    assert!(
        cursor_hiding(),
        "the live test restores its original setting"
    );
    Ok(())
}
