# wayhint

Keyboard hints for everything clickable on a Wayland desktop.

Press a keybind and every button, link, tab and field in the focused window
gets a short letter tag. Type a tag and wayhint clicks that control, then
tags the window again for the next one. `Esc` ends it.

Status: early development. Continuous clicking in the focused window, on
Hyprland, works on the author's setup and has not been packaged yet.

## How it works

wayhint reads the accessibility tree (AT-SPI), the same one screen readers
use, so tags sit on real controls, not on boxes guessed from a screenshot.
Keys come through an input-method keyboard grab, so an open menu stays open
while you type its tag. Clicks go through the compositor's virtual pointer,
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
| `2` then a tag | Double click |
| `o` then a tag | Hover: moves the pointer there and leaves it |
| `[` / `]` then a tag | The mouse's back and forward buttons |
| `Space` | Does the default action, or the verb typed first, where the pointer is |
| `Backspace` | Removes the last typed letter, then the verb |
| `Esc` | Clears typed letters and the verb; with none, exits |

A verb key shows its name at the bottom of the screen until the tag is
typed. Typing another verb replaces it. Clicking a text field exits, so
your typing goes to the field. `wayhint --once` exits after one action, and
`wayhint --verb right` starts with a verb already typed, so a keybind can
go straight to right-clicking.

`wayhint dump` prints what wayhint sees in the focused window as JSON, and
`wayhint dump --explain` adds every control it left out and why. The output
can contain private text from your windows.

## Configuration

`~/.config/wayhint/config.toml`. Every key is optional; `wayhint config`
prints the settings in effect.

```toml
[keys]
alphabet = "ciaehtns"   # tag letters, most used first; default: the home row
# layout = "us"         # layout the letters are written in; default: Hyprland's first

right = ""              # "" makes this what a bare tag does (default: left)
left = "."              # every other verb gets a key: one character or a name
hover = "_"             # Shift works: _ is Shift and -
back = "Home"           # names: Up Down Left Right PgUp PgDn Home End Delete

[keys.aliases]
";" = "c"               # another key that types the same letter

[pointer]
after = "restore"       # cursor back after a click; "leave" keeps it on the target

[style]
font = "Inter:bold"     # a fontconfig pattern, as fc-match takes it
size = 13
text = "#ffffffff"
matched = "#ffffff66"   # letters already typed
background = "#1e1e2ef0"
border = "#d0d0d0ff"
```

Keys are matched by position, so switching to a second layout does not
change the tags.

A key is a tag letter or a verb, never both. A verb key you write is taken
out of the alphabet. A default verb key that is in the alphabet you wrote
stays off instead, and `wayhint config` lists which verbs have no key.
`Esc`, `Backspace`, `Space`, `Enter` and `Tab` are never verb keys.

## Known gaps

- A menu that opens after a click, such as a right-click menu, is usually a
  separate window to the accessibility tree and is not scanned yet, so its
  items get no tags.
- Modifier clicks (Ctrl-click, Shift-click) are not supported yet.

- Elements hidden with `opacity: 0` on web pages still get a tag.
- A control covered by something else inside the same app (a web modal)
  can still get a tag, and the click lands on whatever covers it.
- One monitor at scale 1 is the tested setup. On a scaled monitor the tags
  are drawn at scale 1 and look blurry; several monitors are untested.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
