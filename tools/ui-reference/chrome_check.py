#!/usr/bin/env python3
"""Checks the integrated window frame of Ogun Studio's Windows shell with real mouse input.

    python chrome_check.py --title "Ogun Studio" --exe ogun-studio            # leaves the app running
    python chrome_check.py --title "Ogun Studio" --exe ogun-studio --close    # ends by pressing the close button

The window has no native title bar; the page draws its own controls. This presses them the way a person does, with
SendInput: minimize, maximize and restore, a double click on the bar, dragging the bar to move the window, dragging
an edge to resize it, and (with --close) close. Every press is made only while the window is the foreground window,
so nothing lands in another app. Windows only; needs the capture tool beside it.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import capture_t3 as cap  # noqa: E402

user32 = cap.user32
GWL_STYLE = -16
WS_CAPTION = 0x00C00000
WS_THICKFRAME = 0x00040000

results: list[tuple[bool, str]] = []


def check(ok: bool, name: str, detail: str = '') -> None:
    results.append((bool(ok), name))
    print(('  ok   ' if ok else '  FAIL ') + name + (f'  [{detail}]' if detail and not ok else ''))


def rect(h: int) -> tuple[int, int, int, int]:
    """The client area: the page. (The window rectangle also holds the invisible resize border.)"""
    return cap.client_rect(h)


def size(h: int) -> tuple[int, int]:
    l, t, r, b = rect(h)
    return r - l, b - t


def front(h: int) -> None:
    if not cap.is_foreground(h) and not cap.bring_front(h):
        raise SystemExit('the window cannot be brought to the front; nothing was sent')


def left_down() -> None:
    cap.send_inputs([cap.mouse_input(cap.MOUSEEVENTF_LEFTDOWN)])


def left_up() -> None:
    cap.send_inputs([cap.mouse_input(cap.MOUSEEVENTF_LEFTUP)])


def drag(h: int, x1: int, y1: int, x2: int, y2: int) -> None:
    front(h)
    cap.move_to(x1, y1)
    time.sleep(0.15)
    left_down()
    time.sleep(0.15)
    steps = 12
    for i in range(1, steps + 1):
        cap.move_to(x1 + (x2 - x1) * i // steps, y1 + (y2 - y1) * i // steps)
        time.sleep(0.03)
    time.sleep(0.15)
    left_up()
    time.sleep(0.5)


def click(h: int, x: int, y: int) -> None:
    front(h)
    cap.click_at(x, y)
    time.sleep(0.7)


def double_click(h: int, x: int, y: int) -> None:
    front(h)
    cap.move_to(x, y)
    time.sleep(0.1)
    for _ in range(2):
        left_down()
        time.sleep(0.03)
        left_up()
        time.sleep(0.06)
    time.sleep(0.8)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--title', default='Ogun Studio')
    ap.add_argument('--exe', default='ogun-studio')
    ap.add_argument('--out', default='.design-reference/chrome')
    ap.add_argument('--close', action='store_true', help='finish by pressing the close button and checking that the app exits')
    a = ap.parse_args()
    try:
        user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    except Exception:
        user32.SetProcessDPIAware()

    h, title, pid, image = cap.find_window(a.title, a.exe)
    print(f'window "{title}" pid {pid}')
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    if user32.IsZoomed(h) or user32.IsIconic(h):
        user32.ShowWindow(h, cap.SW_RESTORE)
        time.sleep(0.6)
    front(h)
    # Start from a known place: fully on the screen, whatever the window remembers.
    user32.SetWindowPos(h, 0, 80, 40, 0, 0, 0x0001 | 0x0004)  # SWP_NOSIZE | SWP_NOZORDER
    time.sleep(0.6)
    front(h)

    scale = user32.GetDpiForWindow(h) / 96
    W = lambda n: int(n * scale)  # noqa: E731  CSS pixels to physical pixels

    print('frame')
    fl, ft, fr, fb = cap.frame_rect(h)
    cl, ct, cr, cb = cap.client_rect(h)
    check(ct - ft <= 2 and cb - ct >= (fb - ft) - 4, 'the page starts at the top of the window: there is no native title bar', f'frame top {ft}, page top {ct}')
    w0, h0 = size(h)
    l, t, r, b = rect(h)
    ycenter = t + W(20)

    def button(n: int) -> tuple[int, int]:  # n = 1 close, 2 maximize, 3 minimize, counted from the right edge
        return r - W(46) * n + W(23), ycenter

    print('maximize and restore')
    click(h, *button(2))
    check(bool(user32.IsZoomed(h)), 'the maximize button maximizes the window')
    wm, hm = size(h)
    mon = ctypes.windll.user32.MonitorFromWindow(h, 2)

    class MONITORINFO(ctypes.Structure):
        _fields_ = [('cbSize', wt.DWORD), ('rcMonitor', wt.RECT), ('rcWork', wt.RECT), ('dwFlags', wt.DWORD)]

    mi = MONITORINFO()
    mi.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(mon, ctypes.byref(mi))
    work_w, work_h = mi.rcWork.right - mi.rcWork.left, mi.rcWork.bottom - mi.rcWork.top
    check(abs(wm - work_w) <= 16 and abs(hm - work_h) <= 16, 'a maximized window fills the work area and no more', f'{wm}x{hm} vs {work_w}x{work_h}')
    time.sleep(0.4)
    cap.grab(h, out / 'maximized.png', True, True)
    ml, mt, mr, mb = rect(h)
    # The restore button is the same button in the same place.
    click(h, mr - W(46) * 2 + W(23), mt + W(20))
    check(not user32.IsZoomed(h), 'pressing it again restores the window')
    w1, h1 = size(h)
    check(abs(w1 - w0) <= 4 and abs(h1 - h0) <= 4, 'and restores its size', f'{w1}x{h1} vs {w0}x{h0}')

    print('moving')
    l, t, r, b = rect(h)
    drag(h, l + W(520), t + W(20), l + W(520) + W(180), t + W(20) + W(60))
    l2, t2, _, _ = rect(h)
    check(abs((l2 - l) - W(180)) <= 6 and abs((t2 - t) - W(60)) <= 6, 'dragging the top bar moves the window', f'moved by {l2 - l},{t2 - t}')
    # Put it back.
    drag(h, l2 + W(520), t2 + W(20), l2 + W(520) - W(180), t2 + W(20) - W(60))

    print('double click')
    l, t, r, b = rect(h)
    double_click(h, l + W(520), t + W(20))
    check(bool(user32.IsZoomed(h)), 'a double click on the top bar maximizes the window')
    ml, mt, mr, mb = rect(h)
    double_click(h, ml + W(520), mt + W(20))
    check(not user32.IsZoomed(h), 'a second double click restores it')

    print('resizing')
    # The resize handles are an invisible band just outside the page, inside the window rectangle. The window has just
    # been restored from maximized, so let it settle, and press again once if the system was still animating.
    time.sleep(1.0)
    front(h)

    def corner_drag(dx: int, dy: int) -> tuple[tuple[int, int], tuple[int, int]]:
        ol, ot, orr, ob = cap.window_rect(h)
        before = (orr - ol, ob - ot)
        for _ in range(2):
            drag(h, orr - 3, ob - 3, orr - 3 + dx, ob - 3 + dy)
            nl, nt, nr, nb = cap.window_rect(h)
            after = (nr - nl, nb - nt)
            if after != before:
                break
            time.sleep(0.8)
        return before, after

    (w_before, h_before), (w_after, h_after) = corner_drag(-W(120), -W(90))
    check(w_after < w_before - W(60) and h_after < h_before - W(40), 'dragging the corner resizes the window', f'{w_before}x{h_before} -> {w_after}x{h_after}')
    (_, _), (w_back, h_back) = corner_drag(W(120), W(90))
    check(w_back > w_after + W(60), 'and it grows again', f'{w_after}x{h_after} -> {w_back}x{h_back}')

    print('minimize')
    l, t, r, b = rect(h)
    click(h, r - W(46) * 3 + W(23), t + W(20))
    check(bool(user32.IsIconic(h)), 'the minimize button minimizes the window')
    user32.ShowWindow(h, cap.SW_RESTORE)
    time.sleep(0.8)
    front(h)

    if a.close:
        print('close')
        l, t, r, b = rect(h)
        click(h, r - W(23), t + W(20))
        time.sleep(1.5)
        alive = any(p == pid for _, _, p in cap.enum_windows())
        check(not alive, 'the close button ends the app')

    bad = [n for ok, n in results if not ok]
    print(f'\n{len(results) - len(bad)}/{len(results)} checks passed')
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main())
