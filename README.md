# wayhint

[![CI](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml/badge.svg)](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Keyboard hints for everything clickable on a Wayland desktop.

Press a keybind and every button, link, tab and field in the windows on
your screen gets a short letter tag. Type a tag and wayhint clicks that
control, then tags the windows again for the next one. The close key
(`Esc` by default) ends it.

It's vimium's link hints for the whole desktop, read from the
accessibility tree that screen readers use, with pixel detection for the
windows that don't have one.

Status: early development, Hyprland only. It works daily on the author's
setup (one monitor at scale 1) and has not been packaged yet.

- [What it offers](#what-it-offers)
- [Install](#install)
- [Getting tags in every app](#getting-tags-in-every-app)
- [Use](#use)
- [Configuration](#configuration)
- [Other compositors](#other-compositors)
- [Roadmap](#roadmap)
- [Limitations](#limitations)

## What it offers

### Tags on real controls

wayhint reads the accessibility tree (AT-SPI), so tags sit on the controls
apps report, not on boxes guessed from a screenshot. Every window on the
focused monitor gets tags, not only the focused one. A control under
another window gets none. Bars, launchers and other layer-shell surfaces
(a Quickshell shell, say) get tags, and so do their popups and the menus
apps open while wayhint runs.

### Windows with no tree still get tags

kitty, canvases, games and many Electron apps get tags found in their
pixels: Hyprland renders the window alone into a buffer and wayhint looks
for shapes that read as controls and words. These tags have teal letters,
since they are guesses. A paragraph gets one tag, which opens into a tag
per line. A terminal gets a single tag that focuses it, and a spreadsheet
or text document (LibreOffice, or a Google Sheets grid in a browser) gets
a single tag that puts the cursor in it.

### More than a left click

Type a verb key before a tag for a right, middle or double click, a hover,
the mouse's back and forward buttons, a scroll, or a press-and-hold that
becomes a drag or a long press. Hold `Shift`, `Ctrl` or `Alt` on the last
letter to click with that modifier. The arrows move where one click lands,
so a tag near a spot can click anywhere around it. A verb typed twice does
it again in the same place.

### Tags that keep up

While the tags are up, wayhint scans again every half second. A page that
finishes loading or a window that opens gets tags without a key press, and
a control found again keeps its tag and whatever you had typed for it.
Switch workspace and the tags follow.

### Text mode

Press `/` and type text you see on screen instead of a tag. Matches get
yellow boxes, `Tab` and the arrows move between them, and `Enter` clicks
the current one. Controls with no text to type, such as icons, keep a
tag, so one mode reaches both. The text comes from the accessibility tree,
from kitty's own remote control, or, if you turn it on, from Tesseract OCR
for windows with neither.

### Safe clicks

Before each click wayhint reads the target again and skips the click if
the control moved, changed or got covered. The cursor goes back where it
was. Keys come through an input-method grab, so an open menu stays open
while you type its tag. No button or modifier is ever left held, even if
wayhint panics. A typo in the config is reported before wayhint takes
the keyboard.

### Your layout

Keys are read with the keymap your compositor already uses and matched by
position, so the tags stay the same when you switch to a Greek or Cyrillic
layout, and text mode searches in whatever layout is active.

## Install

You need:

- Hyprland. It must offer `zwlr_layer_shell_v1` and
  `zwlr_virtual_pointer_v1`, which it does by default.
  `zwp_input_method_manager_v2` is used when present. Without it, or when
  fcitx or IBus holds the input method, wayhint takes keyboard focus
  instead, and open menus close. `zwp_virtual_keyboard_v1` is needed for
  modifier clicks.
- Rust 1.88 or newer, libxkbcommon, and fontconfig (`fc-match`).
- The AT-SPI bus, from `at-spi2-core`. Most desktops already run it.
- Optional: Tesseract, for text mode in windows with no tree:
  `pacman -S tesseract tesseract-data-eng`, then `ocr = "eng"` under
  `[text]`.

Build and install it:

```sh
cargo install --locked --git https://github.com/tkutsu/wayhint
```

or from a clone, `cargo build --release`, which leaves the binary in
`target/release/wayhint`.

Then bind it to a key. In `hyprland.conf`:

```
bind = SUPER, W, exec, wayhint
bind = SUPER CTRL, W, exec, wayhint --text
```

or in a Lua config:

```lua
hl.bind("SUPER + W", hl.dsp.exec_cmd("wayhint"))
hl.bind("SUPER + CTRL + W", hl.dsp.exec_cmd("wayhint --text"))
```

Pressing the keybind again closes wayhint. On an empty workspace it keeps
listening for controls to appear.

`wayhint doctor` checks the setup: the config, Hyprland, each Wayland
protocol above, the accessibility bus, the fonts, Tesseract when OCR is
on, and kitty's remote control. It prints one line per check and says
what to install or set for each one that fails.

## Getting tags in every app

An app gets exact tags only if it puts its controls on the accessibility
bus. Many do out of the box; some need a switch. `wayhint dump --surfaces`
lists every window on the monitor with its tag count, which shows which
apps are missing.

| App | What it needs |
|---|---|
| GTK 4 apps | Nothing |
| GTK 3 apps, Firefox | Usually nothing. If they get no tags, turn on GNOME's accessibility setting: `gsettings set org.gnome.desktop.interface toolkit-accessibility true` |
| Qt apps (Dolphin, KDE apps, Quickshell) | Nothing on the author's setup. If one gets no tags, start it with `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1` set |
| Chromium, Chrome, Electron apps | The flag `--force-renderer-accessibility`. On Arch, put it on its own line in `~/.config/chromium-flags.conf` or `~/.config/electron-flags.conf`; an app that bundles its own Electron takes it on its command line |
| kitty | Remote control, for text mode (below). Its panes get pixel tags either way |
| Anything else with no tree | Pixel tags, and with `[text] ocr = "eng"`, Tesseract for text mode |

An app joins the bus when it starts. Apps started before the bus, or
before you turned a switch on, need reopening; wayhint says so when it
finds an app that started before the bus.

Kitty has no accessibility tree, so wayhint asks it for its text and cursor
position. Text mode and the cursor hint need a socket, in `kitty.conf`:

```conf
allow_remote_control socket-only
listen_on unix:${XDG_RUNTIME_DIR}/kitty-{kitty_pid}.sock
```

## Use

| Key (default) | Does |
|---|---|
| a tag | Left-clicks that control, then shows new tags |
| `r` then a tag | Right click |
| `m` then a tag | Middle click |
| `t` then a tag | Double click ("twice") |
| `o` then a tag | Hover: moves the pointer there and leaves it |
| `b` / `n` then a tag | The mouse's back and forward ("next") buttons |
| `PageUp` / `PageDown` then a tag | Turns the wheel 5 notches up or down there |
| `,` then a tag | Hold: presses the left button and keeps it down; the next tag or `Space` lets go there |
| a verb twice | Does it again where the last command was |
| `Shift`, `Ctrl` or `Alt` on a tag's last letter | The same click with that key held |
| `Space` | The default action, or the verb typed before it, where the pointer is |
| arrows, before the last letter or `Space` | Move where that one click lands, 10 px a press; held, faster |
| the repeat key, if you set one | The last command again on the same control, even after its tag changed |
| `/` | Text mode |
| `?` | Every key and what it does, under the input box; any other key closes it |
| `Backspace` | Takes back the last letter or verb |
| close key (`Esc` by default) | Exits; while a hold is down, lets go where it is instead |

### Verbs and modifiers

A verb can come anywhere before a tag's last letter: `a t s` double-clicks
`as`, as `t a s` does. The verb's name shows in the input box. Typing
another verb replaces it. Typing the same verb again before any letter does it where
the last command was, or at the pointer if there was none yet, so
`PageDown PageDown` scrolls the same pane again.

Ctrl-click opens a link in a background tab, Shift-click extends a
selection, Ctrl-scroll zooms. Right Alt doesn't count as Alt, since on many
layouts it's AltGr.

### The input box

A box near the bottom of the screen shows what you've typed: the verb's
name, dimmed, then the tag letters so far, while the tags keep only the
letters still to type. With `[input] hints = false` the letters stay on
the tags instead, dimmed, after the verb's key (`tAS`). In text mode the
box holds the query and the match count.

Its second line has messages, each with its own icon and colour:

- `scanning`, when reading the windows has taken over half a second.
- `no targets`, while nothing on the monitor has a tag.
- A letter no tag starts with, or text nothing on screen matches. A tag
  key is dropped, so a typo costs nothing; text stays typed, to fix with
  Backspace. The message goes after two seconds.
- The key list, while `?` has it up.

Each message can be turned off under `[info]`.

`[input] visible = false` stops drawing the first line, and
`[info] visible = false` the messages. With both off there's no box. Either
way, a bar listening for the input box still gets all of it (next section).
With `hints` on and the first line hidden, the letters you type show only
in that bar.

### The input box in a bar

wayhint can send the input box to another program, such as a Quickshell
bar, to show in its own style. Whenever the box changes, wayhint writes
the whole of it as one line of JSON to the Unix socket
`$XDG_RUNTIME_DIR/wayhint-$WAYLAND_DISPLAY.sock`. The bar listens there;
wayhint connects when a run starts and closes the connection when it
ends, crash included, so the bar clears its copy when the connection
closes. Nothing is sent while nobody listens, and a listener that stops
reading loses its connection instead of slowing your typing. wayhint
tries again with the next change, and a new connection always starts with
a whole line, so the latest line is all a bar needs.

A line with `right` armed and `AB` typed:

```json
{"version":1,"monitor":"HDMI-A-1","mode":"tags","input":"AB","verb":"right","holding":false,"segments":[{"role":"verb","text":"right ","dimmed":true},{"role":"tag_input","text":"AB","dimmed":false}],"matches":null,"info":null}
```

| Field | Holds |
|---|---|
| `version` | `1`; ignore lines with another |
| `monitor` | The output the run is on, as `HDMI-A-1` |
| `mode` | `tags`, `text`, or `null` between rounds (reading the windows, clicking) |
| `input` | The tag letters typed, or the text query, even when the tags show the letters (`hints = false`) |
| `verb` | The armed verb's name, or `null` |
| `holding` | A hold keeps a button down: the box says `drop` |
| `segments` | The first line as the box draws it, in parts with a `role` (`verb`, `drop`, `tag_input`, `query`, `placeholder`, `prefix`, `suffix`, `match_count`) and whether it's `dimmed`. Joined, the texts give the line |
| `matches` | In text mode once something is typed: `current` (from 1) and `total`, both `null` while the matches are being found (`pending: true`) |
| `info` | The message, `kind` (`scanning`, `no_targets`, `no_match`, `help`) and `text`, or `null`. The key list's `text` has tabs between columns and a newline per key |

A listener in Quickshell 0.3:

```qml
SocketServer {
    active: true
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/wayhint-" + Quickshell.env("WAYLAND_DISPLAY") + ".sock"
    handler: Socket {
        onConnectedChanged: if (!connected) pill.state = null
        parser: SplitParser {
            onRead: line => {
                const state = JSON.parse(line);
                if (state.version === 1)
                    pill.state = state;
            }
        }
    }
}
```

Quickshell replaces a socket file a crashed bar left behind, and drops
half a line when a connection closes. Show each segment with a
`Text { textFormat: Text.PlainText }`, so a typed `<` or `&` shows as
itself.

### Moving a click

Arrows typed before the last letter move the click: `Right Right Down`
then a tag clicks 20 px right of the control and 10 px below. Held, an
arrow repeats after a quarter of a second and speeds up to four times as
fast. A dot marks where each tag would land, and a line joins it to its
tag. A move stops 5 px inside the monitor's edge. With `Space`, the dot
follows the pointer.

### Holding and dragging

A hold keeps its button down across rounds. The tags come back with
`drop` in the input box, and whatever you pick next (a tag,
`Space`, a text match, a verb typed twice) is where it lets go, moving
there in small steps so apps see a drag. `, ,` or the close key lets go where it
went down: a long press. A scroll never clicks, so it never ends the run.

### Text mode

After `/`, every key goes into the query, spaces and verb keys included,
so a verb goes before `/`: `t/save Enter` double-clicks "save". Each
visible match gets a yellow box, and the current one is orange. The
arrows, `Tab` and `Shift+Tab` move between matches and wrap; `Home` and
`End` go to the first and last. `Enter` clicks the middle of the current
match's first line, and `Shift`, `Ctrl` or `Alt` held on `Enter` hold that
key through the click. `Backspace` with nothing typed goes back to the
tags.

Targets with no text you could type keep a tag: icon buttons, controls
whose label wasn't read, and windows tagged from their pixels. A control
whose own text was read (a button's label, a link's words) loses its tag,
since typing the text finds it. These tags use letters that no word on
screen starts with, so a whole tag is never also a query. Typing a whole
tag acts on its target at once, as in tag mode. Until then, what you type
counts for both, and the tags that no longer fit go away. `hints = false`
under `[text]` hides them all again.

`[text] cursor` picks where in the match `Enter` acts. The default,
`"center"`, is the middle of the first visible line. `"start"` is that
line's left edge, and `"end"` the rightmost pixel of the last visible
line, each halfway down its line and inside the match's box. With
`pick_unique = true`, a query of 3 or more characters that leaves one
match acts on it without `Enter`.

A lower-case query ignores case; one capital letter makes it exact.
Accents are ignored both ways (`cafe` finds "café", `α` finds "άλλο"). A
space matches any run of spaces or a line break. A match starts where a
word does: `con` finds "contact" and "Control" but not "falcon", and
`save fi` goes on into the next word. Letters, digits and `_` make up
words, and an apostrophe inside one keeps it whole, so `t` doesn't find
"can't". A query that starts with punctuation, like `--help` or `/home`,
is found anywhere.

Before the click wayhint reads the text at that spot again, with the
character before it, and skips the click if it changed.

OCR is off by default. With `ocr = "eng"` under `[text]`, windows with
no tree, other than kitty, are read by Tesseract when text mode starts.
Only English is read for now. On a monitor with a scale under 2, the picture is doubled
first, since Tesseract misreads small interface text at its real size.
Reading took about 0.5 s for a terminal and 1.5 s for a full-screen
spreadsheet on a Ryzen 7 7700; the query box shows `text ...` until it's
done, and keys typed meanwhile wait.

### Windows tagged from their pixels

On 30 windows whose accessibility tree gave the right answers, pixel tags
put 84% of the controls one tag away, and about a third of the tags landed
on nothing clickable. Reading a 2560x1400 window takes about 11 ms on the
author's laptop, plus 10 to 25 ms for Hyprland to render it.

A click in such a window always ends the run, since nothing says whether
it landed in a text field. A paragraph's tag opens into a tag per line
instead of clicking, and a verb typed before it carries over.

A Kitty terminal gets one tag centered on its active pane's text cursor.
It reads the cursor through Kitty's remote control, without a screenshot,
whether pixel detection is on or off. `[style] position` and `offset` do
not move the terminal tag. If remote control cannot be read, Kitty gets
no tag. Other terminals use a patch near the bottom-right corner, checked
for clear space when pixel detection is on. Picking the tag clicks there
and exits, so your typing goes to the terminal;
`[pixels] terminal_hint = false` hides it. An editable sheet or document
gets one tag on the empty spot
nearest its middle, which does the same. Links and controls around it keep
their own tags.

### When a run ends

A run ends on the close key, on the keybind, after a click on a text field (so
your typing goes to it), and after a click wayhint can't vouch for: one an
arrow moved off its control, one made with `Space`, or one in a window
tagged from its pixels. Hovers and scrolls never end it.

Flags for other keybinds:

| Flag | Does |
|---|---|
| `--once` | Exits after one action |
| `--verb right` | Starts with a verb typed, for a right-click keybind |
| `--text` | Starts in text mode, with no tags first |
| `--static` | Scans once, keeps those tags, and exits after one action or a workspace switch |

### Warnings and debugging

When a run started from a keybind fails, or something risks a stuck key,
a held modifier or the wrong workspace, the reason shows as a desktop
notification (Hyprland's own notification if no server answers). From a
terminal it's printed.

Routine messages (an app with no tags or text, a slow scan, pixel tags, a
menu that can't be watched, nothing to repeat) go to
`~/.local/state/wayhint/wayhint.log` instead, or under `$XDG_STATE_HOME`
when that's set, grouped by app. Serious warnings are logged too. Each
line starts with the Unix time and a severity. The log rotates at 1 MiB
and keeps one previous file as `wayhint.log.1`.

`wayhint dump` prints what wayhint sees in the focused window as JSON;
`--explain` adds every control it left out and why, `--surfaces` lists
everything on the monitor with its tag count, `--pixels` prints the pixel
tags, and `--text con` prints the text text mode would search and where
`con` is in it. The output can contain private text from your windows.

## Configuration

`~/.config/wayhint/config.toml`. Every setting is optional.
[`config.default.toml`](config.default.toml) lists them all at their
defaults, commented out, each with what it does. Start from it:

```sh
wayhint config --default > ~/.config/wayhint/config.toml
```

`wayhint config` prints the settings in effect: your file merged with the
defaults, and which commands have no key.

A short one:

```toml
[keys]
alphabet = "ciaehtns"   # tag letters, easiest to press first; default: the home row
right = ""              # "" makes this what a bare tag does (default: left)
left = "."              # one character or a name: PageUp PageDown Home End Delete
hover = "_"             # Shift works: _ is Shift and -
repeat = "r"            # no key unless you set one

[keys.aliases]
";" = "c"               # another key that types the same letter

[style]
font = "Inter:bold"     # a fontconfig pattern, as fc-match takes it
background = "#1e1e2ee0"
position = "top-left"   # where a tag sits on its control
case = "lower"          # the tags' letters; the keys stay the same

[input]
placeholder = "find"
position = "top"
```

The sections:

| Section | Sets |
|---|---|
| `[keys]` | Tag letters, verb keys, repeat, text, help and close keys, aliases |
| `[style]` | The tags: font, size, colours, border, radius, padding, letter case, position on the control, offset |
| `[style.leader]` | The line and dot from a moved tag or a nudged click to where it lands |
| `[style.pixels]` | How tags found in pixels differ from the rest; by default only their letters, in teal |
| `[input]` | The box for what you type: whether it's drawn, look, placeholder, prefix and suffix, screen position and offset. Once called `[text.input]`, which still loads |
| `[info]` | The messages under the input box: whether they're drawn, which show, for how long, their icons and colours |
| `[text]` | Where `Enter` acts on a match, `pick_unique`, tags on targets with no text, and OCR (`eng` or `off`, the default) |
| `[text.matches]`, `[text.matches.current]` | The boxes over matches and over the current one |
| `[pointer]` | Whether the cursor goes back after a click, nudge step, scroll notches, settle time |
| `[pixels]` | The pixel detector (`full`, `fast` or `off`), the terminal focus tag, and which programs count as terminals |
| `[scan]` | Refresh interval, and how close hints must be to share one tag |

Colours are `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`, where A is opacity.
Sizes are logical pixels. The `[info]` icons are Nerd Font glyphs, drawn
with `Symbols Nerd Font` by default; an icon the font lacks is left out.

Keys are written as your keyboard's first layout prints them, which must
be a Latin one (US, Colemak, AZERTY and so on), and matched by position. A
key is a tag letter or a command, never both: a command key you set leaves
the alphabet, and a default command key that clashes with your alphabet
goes unbound. Set `close = "q"` under `[keys]` to close with `q` instead of
`Esc`. It takes `q` out of the alphabet and also closes text mode, so you
cannot type that character in a query. Named keys such as `Esc`, `F1`
through `F24`, `Space` or `Home` work too. `Backspace`, `Space`, `Enter`,
`Tab` and the arrows keep their built-in behavior unless chosen as the
close key. The launch shortcut always closes wayhint.

`detector = "fast"` reads pixels in one pass instead of two: about half
the work (7 ms against 11 on the author's laptop), with 82% of controls one
tag away instead of 84%, about 15% more tags, and 43% of its tags on
nothing clickable instead of 33%.

When tags overlap, one moves into nearby clear space and gets a line back
to its control. If no clear slot fits within 256 px, it stays put.

## Other compositors

Only Hyprland, for now. Drawing the tags, reading keys and clicking go
through Wayland protocols that most wlroots-based compositors offer too.
Three things come from Hyprland itself:

- Where each window, layer and monitor is. On Wayland, AT-SPI gives a
  control's position inside its window but not where the window is on
  screen, so wayhint asks Hyprland's IPC socket.
- Workspace switches, from Hyprland's event socket, so the tags follow.
- Pictures of single windows for pixel tags and OCR, through
  `hyprland-toplevel-export-v1`.

All three live in `src/desktop/hyprland.rs` and `src/desktop/capture.rs`.
A port to sway or niri would answer the same questions from that
compositor's IPC and capture windows with `ext-image-copy-capture-v1`.

## Roadmap

- An ignore key: type it before a tag and that control (its app, role and
  name) never gets a tag again, with `wayhint ignore --list` and
  `--clear` to undo it.
- A package for the AUR.
- Other compositors, as above.

## Limitations

### Setup

- Hyprland only (see [Other compositors](#other-compositors)).
- One monitor at scale 1 is the tested setup. Only the focused monitor
  gets tags. On a scaled monitor the tags are drawn at its full
  resolution through `wp_viewporter`; without it, a fractional scale is
  drawn at 1 and looks blurry. Several monitors and rotated ones are
  untested.
- If the monitor changes size or scale during a run, the run ends.
- A key held down while the tags change is ignored; press it again once
  the new tags show. The same goes when a paragraph opens into lines.
- Qt apps don't rejoin an accessibility bus that restarted after them;
  wayhint says so, and reopening them fixes it.

### Accessibility tree

- A menu gets tags only if it opened while wayhint was running, such as
  one opened with wayhint's right click. A menu already open when wayhint
  starts gets none and doesn't hide the tags under it. An X11 app's menus
  get none.
- A layer surface gets tags only when it sits in its monitor's top-left
  corner, since elsewhere the tree doesn't say where it is. A bar across
  the top or a full-screen launcher qualifies; a dock with a margin
  doesn't.
- Popups get tags only when all of the app's layers are in the corner of
  the monitor at 0,0. Otherwise (a bar on a second monitor, or an app that
  doesn't answer in time) an open popup hides every tag on the monitor
  until it closes.
- A popup gets no tags where a full-screen overlay or a fullscreen window
  may be above it, so a settings window's own dropdown gets none.
- Two windows of one app with the same title get no tags unless one has
  focus: their trees can't be told apart.
- Web elements hidden with `opacity: 0` still get tags, and a control
  covered by something inside the same app (a web modal) can still get a
  tag, which then clicks whatever covers it.
- An editable document's single tag clicks a cell or a line, which moves
  the active cell or the text cursor there.

### Pixel tags

- They are guesses. About a third sit on text or decoration, and a faint
  control or one packed against its neighbours can be missed. A window's
  rounded corners show up as small tags.
- They can't be repeated: there is nothing to find the same control by in
  the next scan.
- Before a click, wayhint only checks that the window hasn't moved, been
  resized or been covered. A page that scrolled since the last refresh
  gets the click where the tag was.
- They were tuned at scale 1. A window at scale 2 or more is averaged down
  first; scales between 1 and 2 are read as they are.
- A sheet is recognised only when its grid lines show, are 1 px thin, and
  rows are 12 to 40 px apart. Otherwise every cell gets a tag. Links inside
  cells get none.
- Clicking a terminal tag can trigger the running TUI's mouse handling.
  Terminals other than Kitty use the least busy patch in their lower half
  when pixel detection finds no empty one.

### Text mode

- It reads the text once, when it starts. Text that changes while you type
  isn't found until you leave text mode and come back.
- It doesn't search open menus, or kitty's quick-access panel, which is a
  layer rather than a window.
- Qt gives most labels and list items only a name, so text mode searches
  the names of labels, list items, cells, tabs, menu items, links and
  headings, and boxes the whole item, not the matched letters. A button's
  name is searched only when its picture shows letters, since an icon
  button's name isn't on screen.
- OCR misreads some words (`cmp::Ordering` and `main.rs` in one terminal
  capture), and a misread word isn't found. With `[pixels] detector =
  "off"` windows without a tree aren't read at all, and pictures inside a
  window that has a tree never are.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

The pixel detector's edge and colour passes (`src/pixels/contour.rs`,
`src/pixels/segment.rs`) are ported from [Neru](https://github.com/y3owk1n/neru),
MIT license, Copyright (c) 2024 Neru Contributors; its notice is in
[src/pixels/LICENSE-NERU](src/pixels/LICENSE-NERU). The window export
protocol file (`protocols/`) is Hyprland's, under the BSD 3-clause license
written at its top.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
