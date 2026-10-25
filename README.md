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

A window with no accessibility tree (kitty, canvases, games, many Electron
apps) is tagged from its pixels instead: Hyprland renders the window alone
into a buffer, and wayhint looks for shapes that read as controls and words.
A paragraph gets one tag, which opens into a tag per line. On 30 windows
whose accessibility tree gave the right answers, the pixel tags put 84% of
the controls one tag away, and about a third of their tags landed on
nothing clickable. Reading a 2560x1400 window takes about 11 ms on the
author's laptop, plus 10 to 25 ms for Hyprland to render the picture.

## Requirements

- Hyprland. Other compositors are not supported yet.
- The compositor protocols `zwlr_layer_shell_v1` and
  `zwlr_virtual_pointer_v1`. `zwp_input_method_manager_v2` is used when
  present; without it, or when fcitx or IBus holds the input method, wayhint
  takes keyboard focus instead, and open menus close.
- libxkbcommon and fontconfig (`fc-match`).
- The AT-SPI bus running (`at-spi-bus-launcher`, part of `at-spi2-core`).
- Apps that expose an accessibility tree, for exact tags. GTK and Qt apps,
  Firefox and Dolphin were checked. Apps that are not on the bus (kitty,
  many Electron and Chromium apps started without accessibility) get tags
  from their pixels, through Hyprland's `hyprland_toplevel_export_v1`.

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
| `Space` | Does the default action, or the verb typed before it, where the pointer is |
| arrows, before the tag's last letter or `Space` | Move where that one click lands, 10 px per press; held, they keep going, faster |
| the repeat key, if you set one | Does the last command again on the same control, even after its tag changed |
| `Backspace` | Removes the last letter or verb typed |
| `Esc` | Clears typed letters, the verb and the arrows' move; with none, exits |

A verb can also come between a tag's letters: `a t s` double-clicks
`as`, as `t a s` does. A typed verb shows in front of every tag (`tAS`
after `t`, or after `a t`), and its name
shows at the bottom of the screen, until the tag is typed. Typing another
verb replaces it. Right Alt doesn't count as Alt, since on many layouts
it's AltGr.

Arrows let a tag click anywhere near it: `→ → ↓` then a tag clicks 20 px
right of the control and 10 px below. Hold an arrow to go further: it
repeats after a quarter of a second and speeds up to four times as fast. A
dot marks where each tag would land, so the tags stay readable. A move
stops 5 px inside the edge of its monitor, so its dot stays in sight.
It is for that one click, and the repeat key does it again. With `Space`,
the dot follows the pointer at each key you press, and the click lands on
the dot.

While the tags are up, wayhint scans the windows again every half second
(`[scan] refresh`), so a page that finishes loading or a window that opens
gets tags without a key. A control found again keeps its tag, and the
letters you typed for it stay typed; only new controls take new tags.
Moved controls get their tags at the new position; hidden controls lose
theirs. If a control changes just as you pick it, wayhint skips that click
and refreshes the tags.

A key held across a change to the targets is ignored. Press it again
after the new tags appear. The same rule applies when a paragraph opens
into tags on its lines.

Press the keybind again to close wayhint. On an empty workspace it keeps
listening for controls to appear, and the keybind still closes it.

Clicking a text field exits, so your typing goes to the field. So does a
click wayhint can't vouch for: one an arrow moved outside its control, or
one made with `Space`, since a field there would take the focus while
wayhint kept your keys. `wayhint --once` exits after one action, and
`wayhint --verb right` starts with a verb already typed, so a keybind can
go straight to right-clicking.

`wayhint --static` scans once and keeps those hints until you act or exit.
It closes after one action or a workspace switch. To bind it to Super+Alt+W:

```
bind = SUPER ALT, W, exec, wayhint --static
```

In a window tagged from its pixels, a click always exits, since nothing
says whether it landed in a text field. A paragraph's tag shows a tag on
each of its lines instead of clicking; a verb typed before it carries over
to the line you pick. Pixel tags can't be repeated: there is nothing to
find the same control by in the next scan.

When a run started from a keybind fails, the reason shows as a Hyprland
notification. From a terminal it's printed instead.

`wayhint dump` prints what wayhint sees in the focused window as JSON, and
`wayhint dump --explain` adds every control it left out and why.
`wayhint dump --surfaces` lists everything on the focused monitor, bottom
to top, with each one's tag count and what couldn't be read; add
`--pixels fast` to tag windows without a tree from their pixels, as a run
does. The output can contain private text from your windows.
`wayhint dump --pixels` prints the tags the window's pixels give, whether
or not it has a tree.

## Configuration

`~/.config/wayhint/config.toml`. Every key is optional; `wayhint config`
prints the settings in effect, as a file you can save and edit to start
one.

Hints whose click centers are within `scan.merge_distance` logical pixels
form a group, including chains of nearby centers. Each group keeps the
existing hint nearest its average center. Ties keep the first hint in
reading order. The default distance is `0`, which shows every hint.

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
nudge = 10              # px one arrow press moves a click

[pixels]
detector = "full"       # windows with no accessibility tree: "fast", or "off"
terminal_processes = ["kitty", "foot", "footclient", "alacritty", "ghostty", "com.mitchellh.ghostty", "wezterm", "wezterm-gui", "org.wezfurlong.wezterm", "xterm"]

[scan]
refresh = 500           # ms the tags stay up before a rescan; 0 for only after a click
merge_distance = 0      # logical px between nearby hint centers; 0 keeps every hint

[style]
font = "Inter:bold"     # a fontconfig pattern, as fc-match takes it
size = 13
text = "#ffffffff"
matched = "#ffffff66"   # letters already typed
background = "#1e1e2ef0"
border = "#d0d0d0ff"
position = "bottom-right"  # on the target: center, top, bottom, left, right, or a corner
offset = [0, 0]         # px from there, right and down
```

Keys are read with your keyboard's own keymap, the one the compositor
gives every app, so a custom layout or a keymap file (Hyprland's
`kb_file`) needs no setting. The config is written in its first layout,
which must be a Latin one (US, Colemak, AZERTY and so on). Keys are matched
by position, so switching to a second layout, in any script, does not
change the tags.

`detector = "fast"` reads a window's pixels in one pass instead of two,
for slower CPUs. `"full"` runs its two passes side by side, so on the
author's laptop the difference is 7 ms against 11, but `"fast"` does about
half the work. It finds nearly as many controls (82% one tag away) but
gives about 15% more tags, and 43% of its tags land on nothing clickable,
against 33% for `"full"`.

Terminals get one focus hint near the bottom center, about one character
height above the edge. It shifts to nearby empty pixels to avoid text and
status bars. Selecting it clicks there and exits, handing typing back to
the terminal. Code, output, gutters, and TUI controls get no separate hints.
The terminal's cursor shape and blink state do not affect this hint.

An editable spreadsheet or text document, such as a LibreOffice Calc sheet
or Writer page, also gets one hint instead of one per cell or paragraph. It
sits on the empty spot nearest the middle of the visible sheet or page, and
selecting it clicks there and exits, so typing goes to the document. Links,
fields and other controls in and around the document keep their own hints.

`pixels.terminal_processes` replaces the default terminal list. Add or remove
executable names or window classes; matching ignores case. Executable names
also cover terminals launched with a custom class. Set it to `[]` to use
the ordinary detector for all windows. `detector = "off"` disables pixel
hints, including terminal focus hints.

When labels overlap, one moves into nearby clear space and gains a line
and dot pointing to its click target. Displaced labels avoid captured ink,
small controls, and other labels. Their positions stay fixed while you
type a tag. Placement searches up to 256 pixels within the target's window;
if no clear slot fits, the label keeps its original position. Failed
captures protect the whole window rather than treating it as empty.

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
  an open popup could be anywhere, even across monitors, so it hides
  every tag on whichever monitor you hint until it closes. The same
  happens if the app doesn't answer in time.
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
  are untested. Pixel tags were tuned at scale 1; a window at scale 2 or
  more is averaged down first, and scales between 1 and 2 are read as
  they are.
- Pixel tags are guesses. A third of them sit on text or decoration, and a
  control drawn faintly, or packed against its neighbours, can be missed.
  Before a click, wayhint only checks that the window hasn't moved, been
  resized or been covered; a page that scrolled under its tags since the
  last refresh gets the click where the tag was.
- A terminal with no empty patch in its lower half uses the least busy
  patch there for its single focus hint. Clicking a terminal can still
  trigger the running TUI's mouse behavior.
- A window's rounded corners show up as small pixel tags at its corners.
- A document's hint clicks a cell or a line of text, which moves the
  active cell or the text cursor there. Web sheets such as Google Sheets
  don't report a spreadsheet document, so they keep their usual tags.

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
