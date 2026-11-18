# <img src="docs/icon.svg" alt="" width="40" align="top"> wayhint

[![CI](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml/badge.svg)](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Click things on your Hyprland desktop without reaching for the mouse.

Run `wayhint`, type a word you can see on screen, and press `Enter` to click
it. Icons and buttons with no text get short letter hints instead, so you
type the hint. Both work at once, across every visible window on the focused
monitor.

To find what's clickable, wayhint reads the accessibility tree that screen
readers use, plus Kitty's remote control. Windows that expose nothing get
hints guessed from their pixels, drawn in teal so you know they're guesses.

It's early, and it only runs on Hyprland. I use it on one monitor at scale 1,
so other setups haven't been tested.

## What it looks like

Type `doc` and every match lights up in teal across the browser, the file
manager and the editor, with the current one filled in a little stronger.
`Tab` moves between them and `Enter` clicks.

![Wayhint finding doc across Chromium, Dolphin and Neovim, with Calculator alongside](docs/images/text-search.png)

`wayhint --hints` puts a label on every control, icon buttons included. Type
a label to click what it points at.

![Wayhint showing yellow keyboard hints across four apps](docs/images/keyboard-hints.png)

## Requirements

- Hyprland, with a working Wayland session and session D-Bus.
- Rust 1.88 or newer to build, plus libxkbcommon and fontconfig (`fc-match`).
- `at-spi2-core` for the accessibility bus, and apps with accessibility
  turned on. Setup is covered below.
- Kitty and its `kitten` command if you want to search terminal text.
  Other terminals have no text to offer unless they expose it through AT-SPI.

The overlay and the clicks need layer-shell, xdg-shell and the virtual
pointer protocol, which Hyprland provides. With the input-method protocol,
wayhint can read keys without closing an app's open menu. If that protocol
is missing, or fcitx or IBus already holds it, wayhint takes keyboard focus
instead. Modifier clicks need the virtual keyboard protocol, pixel hints use
Hyprland's window-export protocol, and fractional scaling uses viewporter.

## Install and launch

```sh
cargo install --locked --git https://github.com/tkutsu/wayhint
wayhint doctor
```

From a clone, `cargo build --release` produces `target/release/wayhint`.
`wayhint doctor` checks your config, compositor, Wayland protocols,
accessibility bus, fonts and Kitty connection, and explains failed checks.

Bind it in `hyprland.conf`:

```conf
bind = SUPER, W, exec, wayhint
```

For a Lua config:

```lua
hl.bind("SUPER + W", hl.dsp.exec_cmd("wayhint"))
```

Press the binding again, or `Esc`, to close it. Clicking a text field ends
the run so you can type into the app. Clicking most other things starts
another round, so you can keep going. `--once` exits after one click.

If you want a separate key for hints only, bind `wayhint --hints` too. Each
launch stays in its own mode, and launching the other one switches over. On
an empty workspace, wayhint waits for something clickable to show up.

## Turn on accessibility

wayhint can only see what apps publish on the AT-SPI bus, so good hints and
searchable text depend on it. `wayhint dump --surfaces` lists each window
with its hint count, which is the quickest way to spot an app that isn't
showing up.

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

After changing a flag or an environment variable, quit the app completely
and open it again. With Chromium that includes its background processes and
PWAs, because a new window can reuse an old process that never got the
flag. Apps started before the accessibility bus, or left running while it
restarted, may need reopening too. You don't need to restart the bus itself.

The [Chromium accessibility guide](https://www.chromium.org/developers/design-documents/accessibility/)
and [Qt's accessibility docs](https://doc.qt.io/qt-6/qaccessible.html) have
the details on when each turns accessibility on.

## Turn on Kitty remote control

Kitty has no accessibility tree, so wayhint asks it directly: `kitten` reads
the pane text and cursor position over a local socket. Add these lines to
`~/.config/kitty/kitty.conf`:

```conf
allow_remote_control socket-only
listen_on unix:${XDG_RUNTIME_DIR}/kitty-{kitty_pid}.sock
```

Then quit Kitty completely and start it again (save your work first).
Reloading the config doesn't create the socket. Each Kitty process gets its
own socket, and programs it starts find the address in `KITTY_LISTEN_ON`.

Run `wayhint doctor` again to check the connection. With remote control,
terminal text becomes searchable, and `[pixels] terminal_hint = true` adds a
hint at the active pane's cursor. If wayhint can't connect, or the cursor is
hidden, that hint falls back to 10 px from the window's bottom-left corner
and takes the pixel hint colour; a cursor Kitty reported gets the normal
colour. Kitty's quick-access panel is a layer surface and isn't searched.

`socket-only` takes commands through the socket and refuses them from inside
the terminal. Anything that can connect to the socket can read and control
Kitty, so keep that in mind. See [Kitty's remote-control settings](https://sw.kovidgoyal.net/kitty/conf/#opt-kitty.listen_on).

## Text and hints

Type the beginning of a visible word: `con` finds "contact" and "Control",
while `save fi` continues into the next word. Case and accents are
ignored, so `cafe` finds "Café". Spaces match whitespace and line breaks.
Punctuation can start a query too. If the first symbol is also a verb,
both interpretations stay available: with `_` bound to hold, `_hello`
finds `_hello.md` for the default action and `hellooooo` for hold. Use
`Tab` to choose, then `Enter` to act. Icon buttons are also searchable by
their accessibility names. A name match highlights the button and acts
at its center, even when the name is not drawn. By default,
`[words] icon_names = false` searches a
button's name only when its screenshot appears to show letters. Set
`icon_names = true` to search names without a screenshot; hint placement
and pixel target detection still use them. The items of an open menu are
searched by their names.

Set `[words] match_start = "word"`, `"line"` or `"sentence"` to choose where
matching can begin. The default, `"word"`, searches from any word start.
`"line"` searches from the first word after each line break in the text;
visually wrapped rows continue the same line. `"sentence"` searches from
the first word of each Unicode sentence. Leading whitespace and opening
punctuation can be skipped, or you can type the opening punctuation too.
Each text object starts a new line and sentence. Queries can continue
across word and line boundaries. First-character markers follow the
selected setting.

Standard mode relies on typing for Latin text by default, including accented
letters. Targets whose text contains letters from another writing system
also get hints. Add scripts you can type with `[words] scripts = ["Latin", "Greek"]`.
The list replaces the default; Unicode script names and codes such as
`"Grek"` are accepted. Mixed text gets a hint unless every script is listed.
Numbers and punctuation need no extra script. This uses writing systems, so
French and Spanish text count as `"Latin"` too. Text in every script remains
searchable if you type it. These hints follow `[words] fallback_hints`;
`--hints` still tags every control.

Matches get teal boxes, with a stronger fill on the current match, and the
pointer moves onto the current match. Text is read when a round starts,
and again whenever the windows change while nothing is typed. Once you type, it stays as read until the
next round. Before acting, wayhint reads the selected text again and
skips the action if it changed.

| Key | Action |
|---|---|
| `Enter` | Act on the current match. With nothing typed, act at the pointer. |
| `Tab`, `Shift+Tab` | Move between matches, taking the pointer along. |
| Arrows | Select the nearest match in that direction without wrapping. With nothing typed, just a verb, or no match, move the pointer. Hold to repeat. |
| A complete hint | Act on a control without searchable text. |
| `Backspace` | Edit the query; with it empty, cancel the armed verb. |
| `Ctrl` or `Alt` held on `Enter` | Apply that modifier to the click. |
| `/` | With nothing typed, list the keys. |
| `Esc` | Exit, or release a held mouse button first. |

Hints use letters that no searchable word starts with. Typing therefore
narrows both text and hints until one identifies a target. By default,
hints act as soon as they are complete, and text matches
wait for `Enter`. `[hints] auto_accept` and `[words] auto_accept` change
each.

### Verbs

A verb changes the next action. In text mode, type its key before the
query: `Dsave Enter` double-clicks "save". Named keys work at any point.
In hints-only mode, a verb can come anywhere before the hint's last letter.

Verb bindings accept capitals such as `R`, symbols such as `,`, `.`, `"`
and `_`, or named keys such as `Home`, `PageDown` and `Space`. Lowercase letters
and digits are rejected. A capital leaves its lowercase letter to the
hints. Repeat and help follow the same binding rules. Close can also use
a lowercase letter. `default_action` chooses the action for a bare tag.

| Default key | Action |
|---|---|
| No verb | Left click. |
| `R` | Right click. |
| `M` | Middle click. |
| `D` | Double click. |
| `O` | Hover, leaving the pointer there. |
| `Home`, `End` | Mouse back, forward. |
| `PageUp`, `PageDown` | Immediately scroll the focused window up, down by two notches. |
| `H` | Hold the left button; the next target releases it there to drag. |
| `Space` | Repeat the last command on its target. |

Named or capital scroll keys act immediately in either mode, without a
hint or `Enter`. In text mode, a symbol bound to scroll follows the same
literal-or-verb matching rule.
`Ctrl` and `Alt` apply to the scroll. Other verbs stay armed when pressed
again; use Enter with nothing typed to act at the pointer. The repeat key
repeats the last command on its target. In text mode, it repeats only when
the query is empty; otherwise its character becomes part of the query.
The close key releases a hold in place.
These defaults describe a US layout. Keys are matched by position using
your first keyboard layout; a default that produces a lowercase letter or digit on
that layout stays unbound. `wayhint config` shows the actual bindings.

With nothing typed, just a verb, or no text match, the arrows move the
pointer itself, 10 pixels per press, faster when held. In hints-only mode,
they always move the pointer. Pointer movement stops 5 pixels inside the
monitor's edge; during a hold the arrows drag.
Held arrows share one motion, so two arrows move diagonally without
alternating between axes. Releasing the arrows stops the current movement.
Pointer travel is animated, including returns to the saved position.
By default, selecting a text match leaves the cursor in place until an
action is triggered. Set `[pointer] move_on_select = true` to move it as
the selection changes through typing, Tab or arrows. In hints-only mode,
`Enter` acts at the pointer when no hint is typed, or
confirms a completed hint. An incomplete hint does nothing on Enter.
Space can be bound to a verb, for example `hover = "Space"` under `[keys]`.
With Space bound to repeat, it inserts a space into a nonempty text query.
An unbound Space always types a space. `/` shows the key legend.
Hold `Ctrl` or `Alt` on the hint's last letter to apply it, or on `Enter`
when confirmation is on. Shift is never held through a click.

## Configuration

Settings go in `~/.config/wayhint/config.toml`. To start from a commented
copy of the defaults (this overwrites the file, so only do it once):

```sh
mkdir -p ~/.config/wayhint
wayhint config --default > ~/.config/wayhint/config.toml
```

`wayhint config` checks your settings and prints what they resolve to. Every
option is explained in the [default config](config.default.toml). A small
example:

```toml
[keys]
hover = '"'
hold = "_"
repeat = "Q"

[hints]
size = 10
auto_accept = false

[words]
cursor = "start"
auto_accept = true     # act on a unique match after 3 characters

[pointer]
scroll_steps = 2
```

By default the cursor stays wherever wayhint left it. Set
`[pointer] restore_on_exit = true` to put it back where it was when wayhint
started, or `[pointer] restore_on_action = true` to put it back after every
click. A hover stays put, and during a hold the cursor comes back on
release. Both default to `false`. The old `after = "restore"` and
`after = "leave"` still work and mean `true` and `false`.

Each window gets a hint near its top right corner, nudged clear of other
hints and small controls when there's room. It focuses the window without
clicking, has no arrow, and hides while a verb is armed or a drag is going.
`[hints] window_hint = false` turns it off. Terminal cursor hints are off by
default; `[pixels] terminal_hint = true` turns them on, falling back to the
bottom-left corner when the cursor can't be read.

Your config only needs the settings you want to change; everything else
uses the default. The input is a centred, translucent dark box with 18 px
text and rounded corners, and takes the hints' font and shadow.
Match boxes inherit the hints' radius, with their own colors and padding.
`[words.matches.current]` uses a stronger teal fill and outline. Other match
settings and an explicitly set match border are inherited. An explicit
`background` makes the current match solid unless `background_end` is also set.

Under `[keys]`, `default_action = "left"` chooses what a bare tag or Enter
with nothing typed does. Set a shortcut to a key, or `false` to disable it; empty strings are
rejected. For example, `default_action = "right"` makes a bare tag right-click,
and `left = "."` gives left-click a shortcut. The close key must stay bound.

Set `[hints] auto_accept = false` to confirm completed hints with
`Enter`, in either mode. Set `[words] fallback_hints = false` to show only text
matches. `[input] show_typed_hints` controls whether typed tag letters move
into the input box. `[messages]` controls the status messages below it;
their visibility is independent of `[input] visible`. `[input] autohide = 3`
hides the typed line after three seconds without typing, keeping the query active;
use `0` to keep it visible. `[messages] size = 13` keeps messages small even with
larger input text. Other durations use `_ms`, and pointer distances and merge
distance use `_px`.

A no-match message stays up until you fix the query, either so it matches
text or a fallback hint, or by clearing it. A rejected tag message stays until
your next real keypress. `duration_ms` is still accepted but no longer makes
these messages go away. Message visibility still follows `[messages] visible`.
The current match and total appear on the message line, for example `match 1 of 3`,
using `info_icon` and `info_color`. Warnings and help take priority over the count.

Appearance is configured under `[hints]`, `[words.matches]`,
`[words.first_character]`, `[input]` and `[messages]`; colors accept hex RGB or RGBA.
`text` colors hint letters; `verb` colors the armed verb in hints and the input.
Invalid settings are reported before wayhint takes the keyboard.

Hints default to bold black letters on a pale yellow vertical gradient, with
an 11 px font, rounded gray borders and soft shadows. Pixel hints use dark teal letters.
Set `shadow = "#00000000"` under `[hints]` to hide the shadow.
Use `shadow_offset = [0, 2]` for its displacement in logical pixels (right, down)
and `shadow_size = 3` for its outward fade distance. A size of `0` gives a hard
edge. `[hints.pixels]` and `[input]` can override these settings. To keep the
shadow below the input's top edge, make its downward offset at least its size.
Shadows stay outside the rounded box so translucent backgrounds keep an even fill.

Hint backgrounds and letters can each use a gradient. `gradient = "hint"`
(the default) repeats it inside each tag. Use `gradient = "global"` to span
each monitor, visible only inside the tags or letters:

```toml
[hints]
gradient = "global"
background = "#1e1e2ef0"
background_end = "#45475af0"
background_direction = "right"
text = "#ffffffff"
text_end = "#94e2d5ff"
text_direction = "bottom-right"
```

Directions point toward the second colour: `left`, `right`, `top`, `bottom`,
`top-left`, `top-right`, `bottom-left` or `bottom-right`. Text defaults to
`right`, backgrounds to `bottom`. An explicit `text` or `background` without
its `_end` field is solid. Setting either colour in `[hints.pixels]` makes
that part of pixel hints solid too. The gradient setting applies to both
backgrounds and letters. Typed letters, armed verbs and the input box use
solid colours.

Word match boxes use the same monitor-wide gradients under `[words.matches]`
and `[words.matches.current]`: set `background_end` and `background_direction`.
First-character markers share `color`, `color_end` and `direction` under
`[words.first_character]`, whether the shape is an underline, box or dot:

```toml
[words.matches]
background = "#1e1e2ec8"
background_end = "#45475ac8"
background_direction = "bottom-right"

[words.first_character]
mark = "underline"
color = "#89b4faff"
color_end = "#94e2d5ff"
direction = "right"
```

The `[words]` tables control searchable words; the `text` color fields under
`[hints]` and `[input]` control rendered letters.

A hint tries just outside its target: left, below, above, then right,
centered along the chosen edge. `[hints] max_cover = 50` sets how much of each
neighboring target or hint's area it may cover, as an integer percentage from
0 to 100 (default 50). Set it to 0 to avoid covering neighbors or 100 to allow full
coverage. Between hints, the percentage applies to the smaller label. A hint
with too much overlap tries another position. Aligned rows and columns keep
their shared placement even when hints overlap beyond the limit.
Aligned columns share the left or right side nearest the screen edge;
aligned rows share top or bottom.
One blocked hint keeps the group's side. Two or more blocked hints make
the whole group try another side. Shared placements stay anchored,
including while typing. If every side is blocked, the group centers its
hints inside the controls. A filled pointer joins the hint's nearest edge,
using the hint's background gradient and border by default. It points toward
the target and touches it when the target is within the configured length.
Configure its `color`, `outline`, `length` and `outline_width` under
`[hints.arrow]`. Set `length = 0` to hide arrows. Centered fallbacks have no arrow.
Set `[hints] position` to a point such as `"bottom-right"` to put every
hint there instead.
Hints for layer surfaces, including bars, can use space outside the surface
while staying on the monitor.
`[hints] hide_hidden = true` (the default) omits controls whose accessibility
provider reports full transparency. Set it to `false` to include them.
Unknown visibility is kept: Firefox reports Sidebery's transparent close
buttons as opaque, so those may still get hints.
`[words.first_character] mark = "underline"`, `"box"` or `"dot"` marks the first
character of each word that text mode can find, while nothing is typed.
`thickness` sets the underline height or dot diameter. Dots sit centered one
logical pixel below the character. Box borders, radius and padding keep their
own settings; use alpha in `color` to make the fill translucent.

## Limits and troubleshooting

Pixel hints estimate controls from shapes and words; they can miss a
control or point at decoration. They do not provide searchable text.
Clicking one ends the run, and its final check verifies the window's
position and visibility rather than the control's identity.

A menu that was already open when wayhint started is not seen. Some apps
expose only an item's name, so its whole box is highlighted rather than
individual letters.
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
otherwise. The pixel contour detector and filtering rules are based on
[wl-kbptr](https://github.com/moverest/wl-kbptr). The colour pass is ported
from [Neru](https://github.com/y3owk1n/neru), whose MIT notice is in
[src/pixels/LICENSE-NERU](src/pixels/LICENSE-NERU). The Hyprland protocol
in [protocols/](protocols/) carries its BSD 3-clause notice.
