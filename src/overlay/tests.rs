use super::*;

#[test]
#[ignore = "briefly shows the live overlay and grabs the keyboard; run alone"]
fn scroll_keys_act_on_press_and_remain_held_until_release() -> Result<()> {
    let monitors = desktop::monitors()?;
    let monitor = monitors
        .iter()
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
        monitor,
    )?;
    let generation = overlay.state.gate.redrawn();
    overlay.state.gate.synced(generation);
    let code = KeyCode::PAGE_DOWN;
    overlay.state.key(code.evdev(), true);
    let event = overlay.next_event(Some(Instant::now() + Duration::from_millis(30)))?;
    anyhow::ensure!(
        matches!(event, Some(Event::Key(key, _)) if key.code == code),
        "a held scroll key must act before release; got {event:?}"
    );
    overlay.state.key(code.evdev(), false);
    Ok(())
}

#[test]
#[ignore = "briefly shows the live overlay and grabs the keyboard; run alone"]
fn holding_a_typed_key_repeats_until_release() -> Result<()> {
    check_typed_key_repeat(false)
}

#[test]
#[ignore = "briefly takes live keyboard focus; run alone"]
fn holding_a_typed_key_repeats_with_keyboard_focus() -> Result<()> {
    check_typed_key_repeat(true)
}

/// Exercise the real key dispatcher and event-loop timer in both keyboard modes.
fn check_typed_key_repeat(fallback: bool) -> Result<()> {
    let monitors = desktop::monitors()?;
    let monitor = monitors
        .iter()
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
        monitor,
    )?;
    if fallback {
        overlay.release_keyboard()?;
        overlay.state.input_methods = None;
        overlay.grab_keyboard()?;
    }
    anyhow::ensure!(
        matches!(&overlay.state.grab, Grab::Exclusive) == fallback,
        "the test must use its requested keyboard route"
    );
    overlay.start_typing()?;
    let generation = overlay.state.gate.redrawn();
    overlay.state.gate.synced(generation);
    let code = KeyCode::from_evdev(30);
    overlay.state.key(code.evdev(), true);
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut typed = String::new();
    while let Some(event) = overlay.next_event(Some(deadline))? {
        if let Event::Typed(key, _, Some(ch)) = event
            && key.code == code
        {
            typed.push(ch);
            if typed.chars().count() >= 3 {
                break;
            }
        }
    }
    overlay.state.key(code.evdev(), false);
    anyhow::ensure!(
        typed.chars().count() >= 3,
        "holding a typed key produced only {typed:?}"
    );
    let after_release = overlay.next_event(Some(Instant::now() + Duration::from_millis(100)))?;
    anyhow::ensure!(
        !matches!(after_release, Some(Event::Typed(..))),
        "release must stop repeat"
    );

    let later = Instant::now() + Duration::from_secs(1);
    overlay.state.key(KeyCode::BACKSPACE.evdev(), true);
    anyhow::ensure!(
        matches!(overlay.state.repeat_due(later), Some(Event::Typed(key, _, None)) if key.code == KeyCode::BACKSPACE),
        "held Backspace must repeat too"
    );
    overlay.state.key(KeyCode::BACKSPACE.evdev(), false);
    anyhow::ensure!(
        overlay.state.repeat_due(later).is_none(),
        "Backspace release stops repeat"
    );

    overlay.state.events.clear();
    overlay.state.key(code.evdev(), true);
    overlay.state.pressed = 1;
    anyhow::ensure!(
        matches!(overlay.state.repeat_due(later), Some(Event::Typed(key, _, Some('A'))) if key.shift),
        "repeat must use the current Shift state"
    );
    overlay.state.key(code.evdev(), false);
    overlay.state.pressed = 0;

    overlay.state.key(KeyCode::ENTER.evdev(), true);
    anyhow::ensure!(
        overlay.state.repeat.next().is_none(),
        "Enter must never repeat"
    );
    overlay.state.key(KeyCode::ENTER.evdev(), false);

    overlay.state.key(code.evdev(), true);
    overlay.new_tags();
    anyhow::ensure!(
        overlay.state.repeat_due(later).is_none(),
        "new tags cancel repeat"
    );
    overlay.state.key(code.evdev(), false);
    let generation = overlay.state.gate.redrawn();
    overlay.state.gate.synced(generation);
    overlay.state.key(code.evdev(), true);
    overlay.stop_typing();
    anyhow::ensure!(
        overlay.state.repeat.next().is_none(),
        "leaving text mode cancels repeat"
    );
    Ok(())
}

#[test]
#[ignore = "briefly shows the live overlay and grabs the keyboard; run alone"]
fn input_autohide_redraws_without_an_event_and_preserves_messages() -> Result<()> {
    let monitors = desktop::monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| m.focused)
        .context("no focused monitor")?;
    let conn = Connection::connect_to_env()?;
    let style = Style::default();
    let mut input = Input::new(&style);
    input.autohide = Duration::from_millis(30);
    let mut overlay = Overlay::new(
        &conn,
        &style,
        &Marks::new(&style),
        &input,
        &Info::default(),
        monitor,
    )?;
    overlay.release_keyboard()?;
    let frame = Frame {
        status: Status {
            input: "typed query".into(),
            info: Status::saying(crate::style::Notice::NoMatch, "small error").info,
            ..Status::plain("typed query")
        },
        ..Frame::default()
    };
    overlay.update(&frame)?;
    let shown = overlay.pixmap.clone();
    let deadline = Instant::now() + Duration::from_millis(80);
    while Instant::now() < deadline {
        overlay.next_event(Some(deadline))?;
    }
    anyhow::ensure!(
        overlay.autohide.hidden,
        "the idle wait must expire the timer"
    );
    anyhow::ensure!(
        overlay.frame.status.input == "typed query",
        "the query stays active"
    );
    anyhow::ensure!(shown.data() != overlay.pixmap.data(), "timeout must redraw");
    let mut expected = overlay.pixmap.clone();
    overlay.painter.paint(
        &mut expected,
        &Frame {
            status: frame.status.visible(false, true),
            ..Frame::default()
        },
    );
    anyhow::ensure!(
        expected.data() == overlay.pixmap.data(),
        "the message remains drawn"
    );
    overlay.state.events.push_back(Event::Typed(
        Key {
            code: KeyCode::BACKSPACE,
            shift: false,
        },
        Mods::NONE,
        None,
    ));
    overlay.next_event(Some(Instant::now() + Duration::from_millis(10)))?;
    anyhow::ensure!(!overlay.autohide.hidden, "typing reveals the input");
    anyhow::ensure!(
        shown.data() == overlay.pixmap.data(),
        "the full input is restored"
    );
    Ok(())
}

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
