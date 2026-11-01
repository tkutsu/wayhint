use super::*;

#[test]
#[ignore = "briefly grabs the keyboard and moves the live pointer without clicking; run alone"]
fn releasing_an_arrow_interrupts_an_active_nudge() -> Result<()> {
    use crate::pointer::Pointer;
    use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};

    let original = desktop::cursor()?;
    let monitors = desktop::monitors()?;
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.focused)
        .context("no focused monitor")?;
    let extent = crate::geometry::Rect::bounding(monitors.iter().map(|monitor| monitor.rect))
        .context("no monitors")?;
    let conn = Connection::connect_to_env()?;
    let style = Style::default();
    let mut overlay = Overlay::new(
        &conn,
        &style,
        &Marks::new(&style),
        &Input::new(&style),
        &Info::default(),
        monitor,
    )?;
    let mut pointer = Pointer::new(&conn, extent, crate::config::SCROLL)?;
    let code = if original.x < monitor.rect.center().x {
        KeyCode::RIGHT
    } else {
        KeyCode::LEFT
    };
    let dx: i32 = if code == KeyCode::RIGHT { 40 } else { -40 };
    let generation = overlay.state.gate.redrawn();
    overlay.state.gate.synced(generation);
    overlay.state.key(code.evdev(), true);
    overlay.state.events.clear();
    overlay
        .state
        .events
        .push_back(Event::Held(Point::new(dx.signum(), 0), 1));
    overlay
        .event_loop
        .handle()
        .insert_source(
            Timer::from_duration(Duration::from_millis(4)),
            move |_, (), state: &mut State| {
                state.key(code.evdev(), false);
                TimeoutAction::Drop
            },
        )
        .map_err(|error| anyhow::anyhow!("release timer: {}", error.error))?;
    let target = original.offset(Point::new(dx, 0));
    let result = (|| {
        pointer.nudge(original, target, &mut overlay.motion())?;
        let stopped = desktop::cursor()?;
        anyhow::ensure!(
            !overlay.holding(),
            "the release must be handled during motion"
        );
        anyhow::ensure!(
            stopped.x.abs_diff(original.x) < 30,
            "the pointer completed its nudge after release: {stopped:?}"
        );
        std::thread::sleep(Duration::from_millis(30));
        let event = overlay.next_event(Some(Instant::now() + Duration::from_millis(30)))?;
        anyhow::ensure!(
            !matches!(event, Some(Event::Held(..))),
            "no repeat may remain queued after release"
        );
        anyhow::ensure!(
            desktop::cursor()? == stopped,
            "no movement may remain queued after release"
        );
        overlay.state.key(code.evdev(), true);
        overlay.state.events.clear();
        overlay
            .state
            .events
            .push_back(Event::Held(Point::new(dx.signum(), 0), 1));
        overlay
            .event_loop
            .handle()
            .insert_source(
                Timer::from_duration(Duration::ZERO),
                move |_, (), state: &mut State| {
                    state.key(code.evdev(), false);
                    TimeoutAction::Drop
                },
            )
            .map_err(|error| anyhow::anyhow!("pending release timer: {}", error.error))?;
        let event = overlay.next_event(Some(Instant::now() + Duration::from_millis(10)))?;
        anyhow::ensure!(
            !matches!(event, Some(Event::Held(..))),
            "a pending release must take priority over a queued repeat"
        );
        anyhow::ensure!(
            !overlay.holding(),
            "the pending release must clear the held key"
        );
        Ok(())
    })();
    pointer.put_back(original)?;
    result
}

#[test]
#[ignore = "moves the live Hyprland pointer without clicking; run alone"]
fn held_diagonal_reaches_the_compositor_as_one_path() -> Result<()> {
    use crate::{config::Config, pick, pointer::Pointer};

    struct Restore {
        pointer: Pointer,
        at: Point,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = self.pointer.put_back(self.at);
        }
    }

    let original = desktop::cursor()?;
    let monitors = desktop::monitors()?;
    let screens: Vec<_> = monitors.iter().map(|monitor| monitor.rect).collect();
    let screen = screens
        .iter()
        .find(|screen| screen.contains(original))
        .context("cursor outside monitors")?;
    let extent = crate::geometry::Rect::bounding(screens.iter().copied()).context("no monitors")?;
    let conn = Connection::connect_to_env()?;
    let mut restore = Restore {
        pointer: Pointer::new(&conn, extent, crate::config::SCROLL)?,
        at: original,
    };
    let horizontal = if original.x < screen.center().x {
        KeyCode::RIGHT
    } else {
        KeyCode::LEFT
    };
    let vertical = if original.y < screen.center().y {
        KeyCode::DOWN
    } else {
        KeyCode::UP
    };
    let config = Config::parse("", &crate::keys::Layout::named("us", ""))?;
    let mut holds = hold::Holds::default();
    let start = Instant::now();
    holds.press(vertical, start);
    holds.press(horizontal, start + Duration::from_millis(12));
    let mut at = original;
    eprintln!("DIAGONAL_TRACE_BEGIN");
    for tick in 0..6 {
        let (direction, count) = holds
            .due(start + Duration::from_millis(250 + tick * 25))
            .unwrap();
        assert_ne!(direction.x, 0);
        assert_ne!(direction.y, 0);
        let to = pick::moved(at, pick::held_arrow(direction, count, &config), &screens);
        restore.pointer.nudge(at, to, &mut || Ok(true))?;
        let actual = desktop::cursor()?;
        assert!(
            actual.x.abs_diff(to.x) <= 1 && actual.y.abs_diff(to.y) <= 1,
            "compositor coordinate rounding must stay within one pixel: {actual:?}, wanted {to:?}"
        );
        at = to;
    }
    eprintln!("DIAGONAL_TRACE_END");
    drop(restore);
    let actual = desktop::cursor()?;
    assert!(actual.x.abs_diff(original.x) <= 1 && actual.y.abs_diff(original.y) <= 1);
    Ok(())
}

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
