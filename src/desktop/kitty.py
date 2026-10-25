# What each kitty window shows, for wayhint's text mode. wayhint writes this
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
        width = wcwidth(ord(c))
        # A combining mark sits on the character before it.
        if width <= 0 and cols:
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


def pane(window, cell_height):
    g, screen = window.geometry, window.screen
    # A title bar takes a row, on top or under the text.
    top = g.top
    if screen.lines < g.ynum and get_options().window_title_bar == 'top':
        top += cell_height
    return {
        'id': window.id,
        'left': g.left,
        'top': top,
        'lines': [line(screen, y) for y in range(screen.lines)],
    }


@result_handler(no_ui=True)
def handle_result(args, answer, target_window_id, boss):
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
            'panes': [pane(w, size['cell_height']) for w in shown],
        })
    return json.dumps(out)
