# wayhint

Keyboard hints for everything clickable on a Wayland desktop.

Press a keybind and every button, link, tab and field in the windows on
your screen gets a short letter tag. Type a tag and wayhint clicks that
control, then tags the windows again for the next one. `Esc` ends it.

Status: early development, Hyprland only. It works daily on the author's
setup (one monitor at scale 1) and has not been packaged yet.

## What it offers

### Tags on real controls

wayhint reads the accessibility tree (AT-SPI), the one screen readers use,
so tags sit on the controls apps report, not on boxes guessed from a
screenshot. Every window on the focused monitor gets tags, not only the
focused one. A control under another window gets none. Bars, launchers and
other layer-shell surfaces (a Quickshell shell, say) get tags, and so do
their popups and the menus apps open while wayhint runs.

### Windows with no tree still get tags

kitty, canvases, games and many Electron apps get tags found in their
pixels: Hyprland renders the window alone into a buffer and wayhint looks
for shapes that read as controls and words. A paragraph gets one tag,
which opens into a tag per line. A terminal gets a single tag that focuses
it, and a spreadsheet or text document (LibreOffice, or a Google Sheets
grid in a browser) gets a single tag that puts the cursor in it.

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

## Requirements

- Hyprland, with the protocols `zwlr_layer_shell_v1` and
  `zwlr_virtual_pointer_v1`. `zwp_input_method_manager_v2` is used when
  present. Without it, or when fcitx or IBus holds the input method,
  wayhint takes keyboard focus instead, and open menus close.
  `zwp_virtual_keyboard_v1` is needed for modifier clicks.
- libxkbcommon and fontconfig (`fc-match`).
- The AT-SPI bus (`at-spi-bus-launcher`, from `at-spi2-core`).
- Apps on that bus, for exact tags. GTK and Qt apps, Firefox and Dolphin
  were checked. Chromium and Electron apps join it with
  `--force-renderer-accessibility`; on Arch, put that line in
  `~/.config/chromium-flags.conf` or `~/.config/electron-flags.conf`.
- Optional: Tesseract, for text mode in windows with no tree:
  `pacman -S tesseract tesseract-data-eng`.

## Build

```fish
cargo build --release
```

Then bind it to a key. In `hyprland.conf`:

```
bind = SUPER, slash, exec, wayhint
```

Pressing the keybind again closes wayhint. On an empty workspace it keeps
listening for controls to appear.

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

A lower-case query ignores case; one capital letter makes it exact.
Accents are ignored both ways (`cafe` finds "café", `α` finds "άλλο"). A
space matches any run of spaces or a line break, and matches can start
inside words, so `con` finds "falcon".

Before the click wayhint reads the text at that spot again and skips the
click if it changed.

kitty has no accessibility tree, so wayhint asks kitty what its panes
show. That needs a socket, in `kitty.conf`:

```conf
allow_remote_control socket-only
listen_on unix:${XDG_RUNTIME_DIR}/kitty-{kitty_pid}.sock
```

Other windows with no tree are read by Tesseract when text mode starts.
On a monitor with a scale under 2, the picture is doubled first, since
Tesseract misreads small interface text at its real size. Reading took
about 0.5 s for a terminal and 1.5 s for a full-screen spreadsheet on a
Ryzen 7 7700; the query box shows `text ...` until it's done, and keys
typed meanwhile wait. For Greek too, install `tesseract-data-ell` and set
`ocr = "eng+ell"` under `[text]`.

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

```
bind = SUPER CTRL, W, exec, wayhint --text
```

### Warnings and debugging

When a run started from a keybind fails or warns, the reason shows as a
desktop notification (Hyprland's own notification if no server answers).
From a terminal it's printed.

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

[text.input]
placeholder = "find"
position = "top"
```

The sections:

| Section | Sets |
|---|---|
| `[keys]` | Tag letters, verb keys, repeat and text keys, aliases |
| `[style]` | The tags: font, size, colours, border, radius, padding, position on the control, offset |
| `[style.leader]` | The line and dot from a moved tag or a nudged click to where it lands |
| `[text]` | OCR languages |
| `[text.matches]`, `[text.matches.current]` | The boxes over matches and over the current one |
| `[text.input]` | The query box: look, placeholder, prefix and suffix, screen position and offset |
| `[pointer]` | Whether the cursor goes back after a click, nudge step, scroll notches |
| `[pixels]` | The pixel detector (`full`, `fast` or `off`) and which programs count as terminals |
| `[scan]` | Refresh interval, and how close hints must be to share one tag |

Colours are `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`, where A is opacity.
Sizes are logical pixels.

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

## Limitations

### Setup

- Hyprland only. Other compositors aren't supported.
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
