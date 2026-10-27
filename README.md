# wayhint

[![CI](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml/badge.svg)](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Use your keyboard to click what you see on a Hyprland desktop.

Launch `wayhint --text`, type a word on screen, and press `Enter` to click
it. Icons and controls without searchable text keep short letter hints:
type a hint to reach those too. Text and hints work together across the
visible windows on the focused monitor.

wayhint reads the accessibility tree used by screen readers, plus Kitty's
remote control. Windows without accessible controls get hints estimated
from their pixels. These use teal letters because they are guesses.

Early development, Hyprland only. The author's daily setup is one monitor
at scale 1; other monitor arrangements are untested.

## Requirements

- Hyprland, with a working Wayland session and session D-Bus.
- Rust 1.88 or newer to build, plus libxkbcommon and fontconfig (`fc-match`).
- `at-spi2-core` for the accessibility bus, and apps with accessibility
  enabled. Setup instructions are below.
- Kitty and its `kitten` command if you want to search terminal text.
  Other terminals have no text source unless they expose it through AT-SPI.

The overlay and clicks require layer-shell, xdg-shell and the virtual
pointer protocol. Hyprland normally provides these. The input-method
protocol lets wayhint read keys without closing an app's menus; if it is
unavailable or already held by fcitx or IBus, wayhint takes keyboard focus
instead. Modifier clicks need the virtual keyboard protocol. Pixel hints
use Hyprland's window-export protocol; fractional scaling uses viewporter.

## Install and launch

```sh
cargo install --locked --git https://github.com/tkutsu/wayhint
wayhint doctor
```

From a clone, `cargo build --release` produces `target/release/wayhint`.
`wayhint doctor` checks your config, compositor, Wayland protocols,
accessibility bus, fonts and Kitty connection, and explains failed checks.

Bind the combined text-and-hints mode in `hyprland.conf`:

```conf
bind = SUPER, W, exec, wayhint --text
```

For a Lua config:

```lua
hl.bind("SUPER + W", hl.dsp.exec_cmd("wayhint --text"))
```

Press the binding again to close wayhint. `Esc` closes it too. Clicking a
text field ends the run so you can type into the app; other verified
controls usually start another round. `--once` exits after one action.

For a separate hints-only binding, launch `wayhint` without `--text`.
Each launch stays in its chosen mode; launching the other mode switches
to it. An empty workspace keeps listening for controls to appear.

## Enable accessibility

Apps must publish their controls on the AT-SPI bus for accurate hints and
searchable text. Start with `wayhint dump --surfaces`: it lists windows
and their hint counts, which helps identify apps missing from the tree.

| App | Setup |
|---|---|
| GTK 4 | Usually works without changes. Make sure `GTK_A11Y=none` is not set. |
| GTK 3 and Firefox | Usually work without changes. If controls are missing, enable toolkit accessibility with the command below. |
| Qt, including Dolphin and Quickshell | If controls are missing, launch with `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`. |
| Chromium, Chrome and Electron | Launch with `--force-renderer-accessibility`. On Arch, Chromium reads `~/.config/chromium-flags.conf`; put the flag on its own line. Electron apps may need it added to their launcher command. |
| Kitty | Enable remote control as described below. |

To enable the GNOME toolkit setting, where its schema is installed:

```sh
gsettings set org.gnome.desktop.interface toolkit-accessibility true
```

For Qt apps, you can put this in `hyprland.conf` before starting them:

```conf
env = QT_LINUX_ACCESSIBILITY_ALWAYS_ON,1
```

Fully quit and reopen affected apps after changing flags or environment
variables. Chromium's existing background processes and PWAs must exit
too: opening another window can reuse a process without the new flag.
Apps started before the accessibility bus, or left running through a bus
restart, may also need reopening. There is no need to restart the bus to
apply these settings.

See the upstream [Chromium accessibility guide](https://www.chromium.org/developers/design-documents/accessibility/)
and [Qt accessibility documentation](https://doc.qt.io/qt-6/qaccessible.html)
for their activation rules.

## Enable Kitty remote control

Kitty has no accessibility tree. wayhint reads its pane text and cursor
position through a local socket, using `kitten`. Add these lines to
`~/.config/kitty/kitty.conf`:

```conf
allow_remote_control socket-only
listen_on unix:${XDG_RUNTIME_DIR}/kitty-{kitty_pid}.sock
```

Save your terminal work, fully quit Kitty, then launch it again. Reloading
its config does not create the socket. Each Kitty process gets its own
socket; programs it starts inherit the address in `KITTY_LISTEN_ON`.

Run `wayhint doctor` again to check the connection. Remote control enables
text searches and a hint at the active pane's cursor. Without a usable
connection, Kitty has no cursor hint or searchable text. Its quick-access
panel is a layer surface and is not searched.

`socket-only` accepts control through the socket and rejects requests
through the terminal. Programs able to connect to that socket can read
and control Kitty. See [Kitty's remote-control settings](https://sw.kovidgoyal.net/kitty/conf/#opt-kitty.listen_on).

## Text and hints

Type the beginning of a visible word: `con` finds "contact" and "Control",
while `save fi` continues into the next word. Lowercase searches ignore
case; a capital makes the search case-sensitive. Accents are ignored, so
`cafe` finds "café". Spaces match whitespace and line breaks. Punctuation
can start a query too, unless its first key is bound to a verb.

Matches get yellow boxes, with the current match in orange. Text is read
at the start of each round; text that changes while you type is available
in the next round. Before acting, wayhint reads the selected text again
and skips the action if it changed.

| Key | Action |
|---|---|
| `Enter` | Click the current match. |
| `Tab`, `Shift+Tab`, arrows | Move between matches. |
| A complete hint | Act on a control without searchable text. |
| `Backspace` | Edit the query; with it empty, cancel the armed verb. |
| `Shift`, `Ctrl`, `Alt` held on `Enter` | Apply that modifier to the click. |
| `Esc` | Exit, or release a held mouse button first. |

Hints use letters that no searchable word starts with. Typing therefore
narrows both text and hints until one identifies a target. By default,
hints act as soon as they are complete; text matches wait for `Enter`.
These have separate `auto_accept` settings.

### Verbs

A verb changes the next action. In text mode, type its symbol before the
query: `.save Enter` double-clicks "save". Named keys work at any point.
In hints-only mode, a verb can come anywhere before the hint's last letter.

Verb bindings accept symbols such as `,`, `.`, `"` and `_`, or named keys
such as `Home` and `PageDown`. Letters and digits are rejected. `""` makes
one verb the default action; exactly one verb must have it. Repeat, help
and close are separate commands and can still use letters.

| Default key | Action |
|---|---|
| No verb | Left click. |
| `,` | Right click. |
| `/` | Middle click. |
| `.` | Double click. |
| `'` | Hover, leaving the pointer there. |
| `Home`, `End` | Mouse back, forward. |
| `PageUp`, `PageDown` | Scroll up, down by five notches. |
| `;` | Hold the left button; the next target releases it there to drag. |

Type a verb twice to act again at the last command's position, or at the
pointer before the first action. The close key releases a hold in place.
These defaults describe a US layout. Keys are matched by position using
your first keyboard layout; a default that produces a letter or digit on
that layout stays unbound. `wayhint config` shows the actual bindings.

In hints-only mode, `Space` acts at the pointer and arrows move the click
point by 10 pixels per press. `?` shows its key legend. Hold a modifier on
the hint's last letter to apply it, or on `Enter` when confirmation is on.

## Configuration

Settings live in `~/.config/wayhint/config.toml`. Create the directory first
if needed. Print a commented starting file with:

```sh
wayhint config --default > ~/.config/wayhint/config.toml
```

This replaces that file, so use it only when starting a new config.
`wayhint config` validates your existing settings and prints the resolved
values. The [default config](config.default.toml) documents every option.
A small example:

```toml
[keys]
hover = '"'
hold = "_"
repeat = "r"

[hints]
auto_accept = true

[text]
hints = true
cursor = "start"       # start, center (default), or end of the match
auto_accept = false    # true acts on a unique match after 3 characters
```

Set `[hints] auto_accept = false` to confirm completed hints with `Enter`.
This applies in either mode. Set `[text] hints = false` to show only text
matches. Appearance is configured under `[style]`, `[text.matches]`,
`[input]` and `[info]`; colours accept hex RGB or RGBA. Invalid settings
are reported before wayhint takes the keyboard.

## Limits and troubleshooting

Pixel hints estimate controls from shapes and words; they can miss a
control or point at decoration. They do not provide searchable text.
Clicking one ends the run, and its final check verifies the window's
position and visibility rather than the control's identity.

Text search does not cover open menus. Some apps expose only an item's
name, so its whole box is highlighted rather than individual letters.
Overlapping controls inside an app and some layer-shell popups cannot be
resolved reliably. Several monitors and rotated displays are untested.

For setup problems, run `wayhint doctor`. To inspect what an app exposes,
use `wayhint dump` or `wayhint dump --text`; their JSON can contain private
screen text. Routine diagnostics go to
`~/.local/state/wayhint/wayhint.log` (or `$XDG_STATE_HOME/wayhint/wayhint.log`).
Failures launched from a keybind also appear as desktop notifications.

## License

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Contributions are licensed under the same terms unless explicitly stated
otherwise. Pixel detection includes code ported from
[Neru](https://github.com/y3owk1n/neru), with its MIT notice in
[src/pixels/LICENSE-NERU](src/pixels/LICENSE-NERU). The Hyprland protocol
in [protocols/](protocols/) carries its BSD 3-clause notice.
