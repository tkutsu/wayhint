# wayhint

[![CI](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml/badge.svg)](https://github.com/tkutsu/wayhint/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Keyboard hints for everything clickable on a Wayland desktop.

Press a keybind and every button, link, tab and field in the windows on
your screen gets a short letter tag. Type a tag and wayhint clicks that
control, then tags the windows again for the next one. `Esc` ends it.

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
the current one. The text comes from the accessibility tree, from kitty's
own remote control, or from Tesseract OCR for windows with neither.

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
  `pacman -S tesseract tesseract-data-eng`.

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
| Anything else with no tree | Pixel tags, and Tesseract for text mode |

An app joins the bus when it starts. Apps started before the bus, or
before you turned a switch on, need reopening; wayhint says so when it
finds an app that started before the bus.

kitty has no accessibility tree, so text mode asks kitty what its panes
show. That needs a socket, in `kitty.conf`:

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
| `Backspace` | Takes back the last letter or verb |
| `Esc` | Exits; while a hold is down, lets go where it is instead |

### Verbs and modifiers

A verb can come anywhere before a tag's last letter: `a t s` double-clicks
`as`, as `t a s` does. The typed verb shows in front of every tag (`tAS`)
and its name shows at the bottom of the screen. Typing another verb
replaces it. Typing the same verb again before any letter does it where
the last command was, or at the pointer if there was none yet, so
`PageDown PageDown` scrolls the same pane again.

Ctrl-click opens a link in a background tab, Shift-click extends a
selection, Ctrl-scroll zooms. Right Alt doesn't count as Alt, since on many
layouts it's AltGr.

### Moving a click

Arrows typed before the last letter move the click: `Right Right Down`
then a tag clicks 20 px right of the control and 10 px below. Held, an
arrow repeats after a quarter of a second and speeds up to four times as
fast. A dot marks where each tag would land, and a line joins it to its
tag. A move stops 5 px inside the monitor's edge. With `Space`, the dot
follows the pointer.

### Holding and dragging

A hold keeps its button down across rounds. The tags come back with
`drop` at the bottom of the screen, and whatever you pick next (a tag,
`Space`, a text match, a verb typed twice) is where it lets go, moving
there in small steps so apps see a drag. `, ,` or `Esc` lets go where it
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

`[text] cursor` picks where in the match `Enter` acts. The default,
`"center"`, is the middle of the first visible line. `"start"` is that
line's left edge, and `"end"` the rightmost pixel of the last visible
line, each halfway down its line and inside the match's box. With
`pick_unique = true`, a query of 3 or more characters that leaves one
match acts on it without `Enter`.

A lower-case query ignores case; one capital letter makes it exact.
Accents are ignored both ways (`cafe` finds "café", `α` finds "άλλο"). A
space matches any run of spaces or a line break, and matches can start
inside words, so `con` finds "falcon".

Before the click wayhint reads the text at that spot again and skips the
click if it changed.

Windows with no tree, other than kitty, are read by Tesseract when text
mode starts. On a monitor with a scale under 2, the picture is doubled
first, since Tesseract misreads small interface text at its real size.
Reading took about 0.5 s for a terminal and 1.5 s for a full-screen
spreadsheet on a Ryzen 7 7700; the query box shows `text ...` until it's
done, and keys typed meanwhile wait. For Greek too, install
`tesseract-data-ell` and set `ocr = "eng+ell"` under `[text]`.

### Windows tagged from their pixels

On 30 windows whose accessibility tree gave the right answers, pixel tags
put 84% of the controls one tag away, and about a third of the tags landed
on nothing clickable. Reading a 2560x1400 window takes about 11 ms on the
author's laptop, plus 10 to 25 ms for Hyprland to render it.

A click in such a window always ends the run, since nothing says whether
it landed in a text field. A paragraph's tag opens into a tag per line
instead of clicking, and a verb typed before it carries over.

A terminal gets one tag near its bottom centre, on empty pixels where it
can find them. Picking it clicks there and exits, so your typing goes to
the terminal. An editable sheet or document gets one tag on the empty spot
nearest its middle, which does the same. Links and controls around it keep
their own tags.

### When a run ends

A run ends on `Esc`, on the keybind, after a click on a text field (so
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

When a run started from a keybind fails or warns, the reason shows as a
desktop notification (Hyprland's own notification if no server answers),
one per app. From a terminal it's printed.

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
alphabet = "ciaehtns"   # tag letters, most used first; default: the home row
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
| `[keys]` | Tag letters, verb keys, repeat, text and help keys, aliases |
| `[style]` | The tags: font, size, colours, border, radius, padding, letter case, position on the control, offset |
| `[style.leader]` | The line and dot from a moved tag or a nudged click to where it lands |
| `[style.pixels]` | How tags found in pixels differ from the rest; by default only their letters, in teal |
| `[input]` | The box for what you type: look, placeholder, prefix and suffix, screen position and offset. Once called `[text.input]`, which still loads |
| `[info]` | The messages under the input box: which show, for how long, their icons and colours |
| `[text]` | Where `Enter` acts on a match, `pick_unique`, OCR languages |
| `[text.matches]`, `[text.matches.current]` | The boxes over matches and over the current one |
| `[pointer]` | Whether the cursor goes back after a click, nudge step, scroll notches, settle time |
| `[pixels]` | The pixel detector (`full`, `fast` or `off`) and which programs count as terminals |
| `[scan]` | Refresh interval, and how close hints must be to share one tag |

Colours are `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`, where A is opacity.
Sizes are logical pixels. The `[info]` icons are Nerd Font glyphs, drawn
with `Symbols Nerd Font` by default; an icon the font lacks is left out.

Keys are written as your keyboard's first layout prints them, which must
be a Latin one (US, Colemak, AZERTY and so on), and matched by position. A
key is a tag letter or a command, never both: a command key you set leaves
the alphabet, and a default command key that clashes with your alphabet
goes unbound. `Esc`, `Backspace`, `Space`, `Enter`, `Tab` and the arrows
can't be rebound.

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

These settings are read and checked, but nothing acts on them yet:

- `[info]`: the "scanning", "no targets" and "no match" messages, and
  dropping a key that matches no tag.
- `[input] hints`: typed tag letters moving from the tags to the input
  box. For now they show dimmed on the tags.
- `[keys] help`: the key list in the info box.
- `[pointer] settle`: the wait after a click is still a fixed 150 ms.

Also planned: tags drawn sharp on scaled monitors, and `wayhint doctor`,
which checks the setup (the protocols, the accessibility bus, Tesseract,
kitty's socket) and says what's missing.

## Limitations

### Setup

- Hyprland only (see [Other compositors](#other-compositors)).
- One monitor at scale 1 is the tested setup. Only the focused monitor
  gets tags. On a scaled monitor the tags are drawn at scale 1 and look
  blurry. Several monitors and rotated ones are untested.
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
- A terminal with no empty patch in its lower half gets its tag on the
  least busy one, and clicking it can still trigger the running TUI's
  mouse handling.

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
