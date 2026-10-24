# wayhint

Keyboard hints for everything clickable on a Wayland desktop.

Press a keybind and every button, link, tab and field in the windows on
your screen gets a short letter tag. Type a tag and wayhint clicks that
control, then tags the windows again for the next one. `Esc` ends it.

Status: early development. Continuous clicking on Hyprland works on the
author's setup and has not been packaged yet.

## How it works

wayhint reads the accessibility tree (AT-SPI), the same one screen readers
use, so tags sit on real controls, not on boxes guessed from a screenshot.
Hyprland says which windows the focused monitor shows and how they stack,
so a control under a floating window gets no tag. Layer-shell surfaces
(bars, launchers, a Quickshell shell) and their popups get tags too, and
each one covers its whole box: a control behind a launcher gets no tag,
even where the launcher is see-through. Keys come through an
input-method keyboard grab, so an open menu stays open while you type its
tag. Clicks go through the compositor's virtual pointer,
and the cursor goes back where it was.

## Requirements

- Hyprland. Other compositors are not supported yet.
- The compositor protocols `zwlr_layer_shell_v1` and
  `zwlr_virtual_pointer_v1`. `zwp_input_method_manager_v2` is used when
  present; without it, or when fcitx or IBus holds the input method, wayhint
  takes keyboard focus instead, and open menus close.
- libxkbcommon and fontconfig (`fc-match`).
- The AT-SPI bus running (`at-spi-bus-launcher`, part of `at-spi2-core`).
- Apps that expose an accessibility tree. GTK and Qt apps, Firefox and
  Dolphin were checked. Apps that are not on the bus (kitty, many Electron
  and Chromium apps started without accessibility) get no tags.

## Build

```fish
cargo build --release
```

## Use

Bind it to a key. In a classic `hyprland.conf`:

```
bind = SUPER, slash, exec, wayhint
```

| Key (default) | Does |
|---|---|
| a tag | Left-clicks that control, then shows new tags |
| `r` then a tag | Right-click |
| `m` then a tag | Middle click |
| `t` then a tag | Double click ("twice") |
| `o` then a tag | Hover: moves the pointer there and leaves it |
| `b` / `n` then a tag | The mouse's back and forward ("next") buttons |
| `Shift`, `Ctrl` or `Alt` held on a tag's last letter | The same click with that key held: Ctrl-click opens a link in a background tab, Shift-click extends a selection |
| `Space` | Does the default action, or the verb typed first, where the pointer is |
| arrows, before the tag's last letter or `Space` | Move where that one click lands, 14 px per press |
| the repeat key, if you set one | Does the last command again on the same control, even after its tag changed |
| `Backspace` | Removes the last typed letter, then the verb |
| `Esc` | Clears typed letters, the verb and the arrows' move; with none, exits |

A typed verb shows in front of every tag (`tAS` after `t`), and its name
shows at the bottom of the screen, until the tag is typed. Typing another
verb replaces it. Right Alt doesn't count as Alt, since on many layouts
it's AltGr.

Arrows let a tag click anywhere near it: `→ → ↓` then a tag clicks 28 px
right of the control and 14 px below. A dot marks where each tag would
land, so the tags stay readable. A move stops at the edge of its monitor.
It is for that one click, and the repeat key does it again. With `Space`,
the dot follows the pointer at each key you press, and the click lands on
the dot.

Clicking a text field exits, so your typing goes to the field. So does a
click wayhint can't vouch for: one an arrow moved outside its control, or
one made with `Space`, since a field there would take the focus while
wayhint kept your keys. `wayhint --once` exits after one action, and
`wayhint --verb right` starts with a verb already typed, so a keybind can
go straight to right-clicking.

When a run started from a keybind fails, or finds nothing to click, the
reason shows as a Hyprland notification. From a terminal it's printed
instead.

`wayhint dump` prints what wayhint sees in the focused window as JSON, and
`wayhint dump --explain` adds every control it left out and why.
`wayhint dump --surfaces` lists everything on the focused monitor, bottom
to top, with each one's tag count and what couldn't be read. The output
can contain private text from your windows.

## Configuration

`~/.config/wayhint/config.toml`. Every key is optional; `wayhint config`
prints the settings in effect, as a file you can save and edit to start
one.

```toml
[keys]
alphabet = "ciaehtns"   # tag letters, most used first; default: the home row

right = ""              # "" makes this what a bare tag does (default: left)
left = "."              # every other verb gets a key: one character or a name
hover = "_"             # Shift works: _ is Shift and -
back = "Home"           # names: PgUp PgDn Home End Delete
repeat = "q"            # the last command again; no key unless you set one

[keys.aliases]
";" = "c"               # another key that types the same letter

[pointer]
after = "restore"       # cursor back after a click; "leave" keeps it on the target
nudge = 14              # px one arrow press moves a click

[style]
font = "Inter:bold"     # a fontconfig pattern, as fc-match takes it
size = 13
text = "#ffffffff"
matched = "#ffffff66"   # letters already typed
background = "#1e1e2ef0"
border = "#d0d0d0ff"
```

Keys are read with your keyboard's own keymap, the one the compositor
gives every app, so a custom layout or a keymap file (Hyprland's
`kb_file`) needs no setting. The config is written in its first layout,
which must be a Latin one (US, Colemak, AZERTY and so on). Keys are matched
by position, so switching to a second layout, in any script, does not
change the tags.

A key is a tag letter or a command (a verb, or repeat), never both. A
command key you write is taken out of the alphabet. A default verb key that
is in the alphabet you wrote stays off instead, and `wayhint config` lists
which verbs have no key. `Esc`, `Backspace`, `Space`, `Enter`, `Tab` and
the arrows are never command keys.

## Known gaps

- A menu that opens after a click, such as a right-click menu, is usually a
  separate window to the accessibility tree and is not scanned yet, so its
  items get no tags.
- A layer surface gets tags only when it sits in the top-left corner of
  its monitor, because elsewhere the accessibility tree doesn't say where
  it is. A bar across the top or a full-screen launcher qualifies; a dock
  with a margin doesn't, and neither does a layer the size of a dock
  with a margin from the same app.
- Popups get tags only when all of the app's layers are in the corner of
  the monitor at 0,0. Otherwise, such as with a bar on a second monitor,
  an open popup could be anywhere, so it hides every tag on its monitor
  until it closes. The same happens if the app doesn't answer in time.
- A popup gets no tags where a full-screen overlay (a launcher, a
  settings window) or a fullscreen window may be above it. Wayhint can't
  tell which layer a popup belongs to, so a settings window's own
  dropdown gets none.
- A modifier click needs the compositor's virtual keyboard protocol
  (`zwp_virtual_keyboard_v1`); without it, the click happens without the
  modifier and wayhint prints a warning.
- If the monitor changes size or scale during a run, the run ends; start
  it again.
- Two windows of one app with the same title get no tags unless one of
  them has focus: wayhint can't tell their accessibility trees apart.
- Elements hidden with `opacity: 0` on web pages still get a tag.
- A control covered by something else inside the same app (a web modal)
  can still get a tag, and the click lands on whatever covers it.
- One monitor at scale 1 is the tested setup. On a scaled monitor the tags
  are drawn at scale 1 and look blurry; several monitors and rotated ones
  are untested.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
