# What each kitty window shows and where its cursor is. wayhint writes this
# kitten out and runs it inside kitty with `kitten @ kitten`; it answers with
# JSON: each OS window's title, size and cell size, and for each pane on
# screen, where its first cell is and the lines it shows.
import json

from kittens.tui.handler import result_handler
from kitty.fast_data_types import current_focused_os_window_id, get_options, get_os_window_size, wcwidth


def main(args):
    pass


def line(screen, y):
    """Line `y` as shown (scrolled back or not): its text, whether it wraps
    into the next, and the column each character starts at, plus where the
    last one ends, when that isn't simply its index."""
    shown = screen.visual_line(y)
    text = str(shown)
    cols, x = [], 0
    for c in text:
        if c == '\t':
            cols.append(x)
            x = past_tab(shown, x)
            continue
        width = wcwidth(ord(c))
        # A combining mark sits on the character before it. Below 0 is a
        # width kitty can't say (box drawing, private use): one cell.
        if width == 0 and cols:
            cols.append(cols[-1])
            continue
        cols.append(x)
        x += max(width, 1)
    wrapped = bool(shown.last_char_has_wrapped_flag())
    # The text stops at its last ink; a wrapped line's blanks are part of it.
    if wrapped and x < screen.columns:
        text += ' ' * (screen.columns - x)
        cols.extend(range(x, screen.columns))
        x = screen.columns
    cols.append(x)
    out = {'text': text, 'wrapped': wrapped}
    if cols != list(range(len(cols))):
        out['cols'] = cols
    return out


def past_tab(shown, x):
    """The column the text goes on at after the tab at column `x`. kitty
    keeps a tab as its cell, holding how many cells it jumped, then those
    cells as spaces; its text skips up to that many spaces after the tab."""
    cell = shown[x]
    skip = ord(cell[1]) if len(cell) > 1 else 0
    x += 1
    while skip and x < len(shown) and shown[x] == ' ':
        x, skip = x + 1, skip - 1
    return x


def text_top(window, cell_height):
    """The first text row, below a top title bar when one is present."""
    g, screen = window.geometry, window.screen
    # A title bar takes a row, on top or under the text.
    top = g.top
    if screen.lines < g.ynum and get_options().window_title_bar == 'top':
        top += cell_height
    return top


def pane(window, cell_height):
    """The visible text and cell origin of one pane."""
    return {
        'id': window.id,
        'left': window.geometry.left,
        'top': text_top(window, cell_height),
        'lines': [line(window.screen, y) for y in range(window.screen.lines)],
    }


def cursor(window, size):
    """The active cursor cell's origin in physical window pixels."""
    screen = window.screen
    # Pending line wrap can leave x one cell beyond the last column.
    x = min(screen.cursor.x, screen.columns - 1)
    return [
        window.geometry.left + x * size['cell_width'],
        text_top(window, size['cell_height']) + screen.cursor.y * size['cell_height'],
    ]


@result_handler(no_ui=True)
def handle_result(args, answer, target_window_id, boss):
    cursor_only = 'cursor' in args[1:]
    focused = current_focused_os_window_id()
    out = []
    for os_window_id, tabs in boss.os_window_map.items():
        tab = tabs.active_tab
        size = get_os_window_size(os_window_id)
        if tab is None or not size:
            continue
        shown = [w for w, visible in tab.windows.iter_windows_with_visibility() if visible]
        out.append({
            'title': tab.active_window.title if tab.active_window else '',
            'focused': os_window_id == focused,
            'width': size['width'],
            'framebuffer_width': size['framebuffer_width'],
            'cell': [size['cell_width'], size['cell_height']],
            'cursor': cursor(tab.active_window, size) if tab.active_window in shown else None,
            'panes': [] if cursor_only else [pane(w, size['cell_height']) for w in shown],
        })
    return json.dumps(out)
