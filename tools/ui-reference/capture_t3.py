#!/usr/bin/env python3
"""Capture reference screenshots of the T3 Code desktop app on Windows.

Drives the running "T3 Code" window the way a person would: brings it to the front, presses the
app's own shortcuts or clicks at positions given in a plan file, and saves a PNG per state. Nothing
is typed into the app; only the keys and clicks the plan names are sent, and only after the tool has
checked that the T3 Code window really is the foreground window (otherwise the keys would land in
whatever app is in front, and the tool stops instead).

The picture is rendered from the window itself (PrintWindow with full content), so it is correct even
when another window covers T3 Code. A screen grab is the fallback.

    python capture_t3.py --out ../../.design-reference/t3code            # default plan
    python capture_t3.py --out DIR --plan plan.json                       # your own states
    python capture_t3.py --list-windows                                   # find the window title

Plan file: {"states": [{"name": "settings", "do": [{"key": "ctrl+comma"}], "settle_ms": 700,
                         "after": [{"key": "escape"}]}]}
Actions: {"key": "ctrl+shift+p"}, {"click": [fx, fy]} (fractions of the client area),
         {"click_px": [x, y]} (client pixels), {"wait": ms}, {"scroll": -3}.
Key names: letters, digits, f1..f12, escape, enter, tab, space, backspace, arrows, home, end,
pageup, pagedown, comma, period, slash, semicolon, quote, lbracket, rbracket, backslash, minus,
equal, grave; modifiers ctrl, shift, alt, win.

Needs Pillow. Everything else is ctypes against user32, gdi32, kernel32 and dwmapi.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
kernel32 = ctypes.windll.kernel32
dwmapi = ctypes.windll.dwmapi

# ---------------------------------------------------------------- Win32 bits

SW_RESTORE = 9
SW_MINIMIZE = 6
SW_MAXIMIZE = 3
SW_SHOWNORMAL = 1
VK_MENU = 0x12
KEYEVENTF_EXTENDEDKEY = 0x0001
KEYEVENTF_KEYUP = 0x0002
INPUT_MOUSE = 0
INPUT_KEYBOARD = 1
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_WHEEL = 0x0800
MOUSEEVENTF_VIRTUALDESK = 0x4000
MOUSEEVENTF_ABSOLUTE = 0x8000
SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN = 76, 77, 78, 79
DWMWA_EXTENDED_FRAME_BOUNDS = 9
PW_RENDERFULLCONTENT = 0x00000002
DIB_RGB_COLORS = 0
BI_RGB = 0
ASFW_ANY = -1
EXTENDED_KEYS = {0x25, 0x26, 0x27, 0x28, 0x24, 0x23, 0x21, 0x22, 0x2D, 0x2E}

VK = {
    'escape': 0x1B, 'esc': 0x1B, 'enter': 0x0D, 'tab': 0x09, 'space': 0x20, 'backspace': 0x08,
    'left': 0x25, 'up': 0x26, 'right': 0x27, 'down': 0x28, 'home': 0x24, 'end': 0x23,
    'pageup': 0x21, 'pagedown': 0x22, 'insert': 0x2D, 'delete': 0x2E,
    'comma': 0xBC, 'period': 0xBE, 'slash': 0xBF, 'semicolon': 0xBA, 'quote': 0xDE,
    'lbracket': 0xDB, 'rbracket': 0xDD, 'backslash': 0xDC, 'minus': 0xBD, 'equal': 0xBB, 'grave': 0xC0,
    'ctrl': 0x11, 'control': 0x11, 'shift': 0x10, 'alt': 0x12, 'win': 0x5B, 'meta': 0x5B, 'cmd': 0x11, 'mod': 0x11,
}
VK[','] = VK['comma']
VK['.'] = VK['period']
VK['/'] = VK['slash']
VK[';'] = VK['semicolon']
VK['['] = VK['lbracket']
VK[']'] = VK['rbracket']
VK['-'] = VK['minus']
VK['='] = VK['equal']
VK['`'] = VK['grave']
for _i in range(1, 13):
    VK[f'f{_i}'] = 0x6F + _i
for _c in '0123456789':
    VK[_c] = ord(_c)
for _c in 'abcdefghijklmnopqrstuvwxyz':
    VK[_c] = ord(_c.upper())


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ('wVk', wt.WORD),
        ('wScan', wt.WORD),
        ('dwFlags', wt.DWORD),
        ('time', wt.DWORD),
        ('dwExtraInfo', ctypes.POINTER(ctypes.c_ulong)),
    ]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ('dx', wt.LONG),
        ('dy', wt.LONG),
        ('mouseData', wt.DWORD),
        ('dwFlags', wt.DWORD),
        ('time', wt.DWORD),
        ('dwExtraInfo', ctypes.POINTER(ctypes.c_ulong)),
    ]


class _INPUTUNION(ctypes.Union):
    _fields_ = [('ki', KEYBDINPUT), ('mi', MOUSEINPUT)]


class INPUT(ctypes.Structure):
    _fields_ = [('type', wt.DWORD), ('u', _INPUTUNION)]


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ('biSize', wt.DWORD),
        ('biWidth', wt.LONG),
        ('biHeight', wt.LONG),
        ('biPlanes', wt.WORD),
        ('biBitCount', wt.WORD),
        ('biCompression', wt.DWORD),
        ('biSizeImage', wt.DWORD),
        ('biXPelsPerMeter', wt.LONG),
        ('biYPelsPerMeter', wt.LONG),
        ('biClrUsed', wt.DWORD),
        ('biClrImportant', wt.DWORD),
    ]


class WINDOWPLACEMENT(ctypes.Structure):
    _fields_ = [
        ('length', wt.UINT),
        ('flags', wt.UINT),
        ('showCmd', wt.UINT),
        ('ptMinPosition', wt.POINT),
        ('ptMaxPosition', wt.POINT),
        ('rcNormalPosition', wt.RECT),
    ]


def send_inputs(items: list) -> None:
    arr = (INPUT * len(items))(*items)
    sent = user32.SendInput(len(items), arr, ctypes.sizeof(INPUT))
    if sent != len(items):
        raise OSError(f'SendInput sent {sent} of {len(items)}')


def key_input(vk: int, up: bool) -> INPUT:
    scan = user32.MapVirtualKeyW(vk, 0)
    flags = KEYEVENTF_KEYUP if up else 0
    if vk in EXTENDED_KEYS:
        flags |= KEYEVENTF_EXTENDEDKEY
    inp = INPUT()
    inp.type = INPUT_KEYBOARD
    inp.u.ki = KEYBDINPUT(vk, scan, flags, 0, None)
    return inp


def press(combo: str, hold_ms: int = 40) -> None:
    parts = [p.strip().lower() for p in combo.split('+') if p.strip()]
    vks = []
    for p in parts:
        if p not in VK:
            raise ValueError(f'unknown key {p!r} in {combo!r}')
        vks.append(VK[p])
    send_inputs([key_input(v, False) for v in vks])
    time.sleep(hold_ms / 1000)
    send_inputs([key_input(v, True) for v in reversed(vks)])


def mouse_input(flags: int, dx: int = 0, dy: int = 0, data: int = 0) -> INPUT:
    inp = INPUT()
    inp.type = INPUT_MOUSE
    inp.u.mi = MOUSEINPUT(dx, dy, data, flags, 0, None)
    return inp


def move_to(x: int, y: int) -> None:
    vx, vy = user32.GetSystemMetrics(SM_XVIRTUALSCREEN), user32.GetSystemMetrics(SM_YVIRTUALSCREEN)
    vw, vh = user32.GetSystemMetrics(SM_CXVIRTUALSCREEN), user32.GetSystemMetrics(SM_CYVIRTUALSCREEN)
    ax = int((x - vx) * 65535 / max(vw - 1, 1))
    ay = int((y - vy) * 65535 / max(vh - 1, 1))
    send_inputs([mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, ax, ay)])


def click_at(x: int, y: int) -> None:
    move_to(x, y)
    time.sleep(0.06)
    send_inputs([mouse_input(MOUSEEVENTF_LEFTDOWN)])
    time.sleep(0.04)
    send_inputs([mouse_input(MOUSEEVENTF_LEFTUP)])


def wheel(notches: int) -> None:
    send_inputs([mouse_input(MOUSEEVENTF_WHEEL, data=(notches * 120) & 0xFFFFFFFF)])


def enum_windows() -> list:
    out = []
    proc = ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)

    def cb(hwnd, _):
        if not user32.IsWindowVisible(hwnd):
            return True
        n = user32.GetWindowTextLengthW(hwnd)
        if n == 0:
            return True
        buf = ctypes.create_unicode_buffer(n + 1)
        user32.GetWindowTextW(hwnd, buf, n + 1)
        pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        out.append((hwnd, buf.value, pid.value))
        return True

    user32.EnumWindows(proc(cb), 0)
    return out


def process_image(pid: int) -> str:
    """The executable path of a process, or '' when it cannot be read."""
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    h = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return ''
    try:
        buf = ctypes.create_unicode_buffer(1024)
        size = wt.DWORD(1024)
        if kernel32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
            return buf.value
        return ''
    finally:
        kernel32.CloseHandle(h)


def window_area(hwnd: int) -> int:
    r = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return (r.right - r.left) * (r.bottom - r.top)


def find_window(title_part: str, exe_part: str) -> tuple:
    """The main window whose title contains `title_part` and whose process executable contains `exe_part`."""
    hits = []
    for hwnd, title, pid in enum_windows():
        if title_part.lower() not in title.lower():
            continue
        image = process_image(pid)
        if exe_part and exe_part.lower() not in image.lower():
            continue
        hits.append((hwnd, title, pid, image))
    if not hits:
        raise SystemExit(f'no visible window titled like {title_part!r} from an executable like {exe_part!r}; try --list-windows')
    # Helper windows carry the same title; the largest one is the app.
    hits.sort(key=lambda w: window_area(w[0]), reverse=True)
    return hits[0]


def window_rect(hwnd: int) -> tuple:
    r = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return r.left, r.top, r.right, r.bottom


def frame_rect(hwnd: int) -> tuple:
    r = wt.RECT()
    ok = dwmapi.DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, ctypes.byref(r), ctypes.sizeof(r))
    if ok != 0:
        return window_rect(hwnd)
    return r.left, r.top, r.right, r.bottom


def client_rect(hwnd: int) -> tuple:
    r = wt.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(r))
    pt = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    return pt.x, pt.y, pt.x + r.right, pt.y + r.bottom


def is_foreground(hwnd: int) -> bool:
    return user32.GetForegroundWindow() == hwnd


def bring_front(hwnd: int) -> bool:
    """Makes `hwnd` the foreground window, trying the documented tricks in turn. Returns whether it worked."""
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
        time.sleep(0.4)
    for attempt in range(3):
        if is_foreground(hwnd):
            return True
        user32.AllowSetForegroundWindow(ASFW_ANY)
        fg = user32.GetForegroundWindow()
        fg_thread = user32.GetWindowThreadProcessId(fg, None) if fg else 0
        me = kernel32.GetCurrentThreadId()
        attached = False
        if fg_thread and fg_thread != me:
            attached = bool(user32.AttachThreadInput(me, fg_thread, True))
        try:
            # Windows lets a process take the foreground only after it has sent input: a bare Alt tap does that.
            send_inputs([key_input(VK_MENU, False), key_input(VK_MENU, True)])
            user32.SetForegroundWindow(hwnd)
            user32.BringWindowToTop(hwnd)
            user32.SetActiveWindow(hwnd)
        finally:
            if attached:
                user32.AttachThreadInput(me, fg_thread, False)
        time.sleep(0.3)
        if is_foreground(hwnd):
            return True
        if attempt == 1:
            # Restoring from minimised is the one transition Windows always brings to the front.
            user32.ShowWindow(hwnd, SW_MINIMIZE)
            time.sleep(0.3)
            user32.ShowWindow(hwnd, SW_RESTORE)
            time.sleep(0.5)
    return is_foreground(hwnd)


def print_window(hwnd: int):
    """Renders the window's own content (PrintWindow, full content) into a PIL image, or None on failure."""
    from PIL import Image

    l, t, r, b = window_rect(hwnd)
    w, h = r - l, b - t
    if w <= 0 or h <= 0:
        return None
    hdc_win = user32.GetWindowDC(hwnd)
    hdc_mem = gdi32.CreateCompatibleDC(hdc_win)
    bmp = gdi32.CreateCompatibleBitmap(hdc_win, w, h)
    old = gdi32.SelectObject(hdc_mem, bmp)
    try:
        ok = user32.PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT)
        if not ok:
            return None
        bmi = BITMAPINFOHEADER()
        bmi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bmi.biWidth = w
        bmi.biHeight = -h  # top-down
        bmi.biPlanes = 1
        bmi.biBitCount = 32
        bmi.biCompression = BI_RGB
        buf = ctypes.create_string_buffer(w * h * 4)
        lines = gdi32.GetDIBits(hdc_mem, bmp, 0, h, buf, ctypes.byref(bmi), DIB_RGB_COLORS)
        if lines != h:
            return None
        img = Image.frombuffer('RGB', (w, h), buf, 'raw', 'BGRX', 0, 1)
        # Trim the invisible resize borders Windows adds around the visible frame.
        fl, ft, fr, fb = frame_rect(hwnd)
        img = img.crop((max(fl - l, 0), max(ft - t, 0), min(fr - l, w), min(fb - t, h)))
        if img.getbbox() is None:  # all black: the window did not render into the DC
            return None
        return img
    finally:
        gdi32.SelectObject(hdc_mem, old)
        gdi32.DeleteObject(bmp)
        gdi32.DeleteDC(hdc_mem)
        user32.ReleaseDC(hwnd, hdc_win)


def grab(hwnd: int, path: Path, allow_screen: bool, force_screen: bool = False) -> dict:
    # A WebView2 window (the Tauri shells) answers PrintWindow with its blank frame and no page, so --screen-grab skips it.
    img = None if force_screen else print_window(hwnd)
    method = 'print_window'
    if img is None:
        if not (allow_screen or force_screen):
            raise RuntimeError('PrintWindow rendered nothing and the screen fallback is off')
        if not is_foreground(hwnd):
            raise RuntimeError('PrintWindow rendered nothing and the window is not in front; a screen grab would show another app')
        from PIL import ImageGrab

        l, t, r, b = frame_rect(hwnd)
        vx, vy = user32.GetSystemMetrics(SM_XVIRTUALSCREEN), user32.GetSystemMetrics(SM_YVIRTUALSCREEN)
        img = ImageGrab.grab(bbox=(l - vx, t - vy, r - vx, b - vy), all_screens=True)
        method = 'screen'
    img.save(path)
    return {'frame': list(frame_rect(hwnd)), 'size': [img.width, img.height], 'method': method}


# ---------------------------------------------------------------- plan

DEFAULT_PLAN = {
    'states': [
        {'name': '01-main', 'do': [], 'settle_ms': 600},
        {'name': '02-settings', 'do': [{'key': 'ctrl+comma'}], 'settle_ms': 1000, 'after': [{'key': 'escape'}]},
    ]
}


def require_front(hwnd: int, what: str) -> None:
    if not is_foreground(hwnd):
        if not bring_front(hwnd):
            raise RuntimeError(f'refusing to send {what}: the T3 Code window is not the foreground window')


def run_action(hwnd: int, a: dict) -> None:
    if 'key' in a:
        require_front(hwnd, f'key {a["key"]!r}')
        press(a['key'])
    elif 'click' in a:
        require_front(hwnd, 'a click')
        l, t, r, b = client_rect(hwnd)
        fx, fy = a['click']
        click_at(int(l + (r - l) * fx), int(t + (b - t) * fy))
    elif 'click_px' in a:
        require_front(hwnd, 'a click')
        l, t, _, _ = client_rect(hwnd)
        x, y = a['click_px']
        click_at(l + int(x), t + int(y))
    elif 'wait' in a:
        time.sleep(a['wait'] / 1000)
    elif 'scroll' in a:
        require_front(hwnd, 'a scroll')
        wheel(int(a['scroll']))
    else:
        raise ValueError(f'unknown action {a!r}')
    time.sleep(0.15)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--title', default='T3 Code', help='part of the window title (default: "T3 Code")')
    ap.add_argument('--exe', default='t3code', help='part of the process executable path (default: "t3code")')
    ap.add_argument('--out', default='.design-reference/t3code', help='output directory')
    ap.add_argument('--plan', help='plan JSON file (default: main window and settings)')
    ap.add_argument('--only', help='comma-separated state names to capture')
    ap.add_argument('--no-maximize', action='store_true', help='keep the window at its current size')
    ap.add_argument('--allow-screen-grab', action='store_true', help='fall back to a screen grab if PrintWindow fails')
    ap.add_argument('--screen-grab', action='store_true', help='always grab the screen region (needed for WebView2 windows); the window must be in front')
    ap.add_argument('--list-windows', action='store_true')
    args = ap.parse_args()

    # Physical pixels everywhere, whatever the display scale.
    try:
        user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    except Exception:
        user32.SetProcessDPIAware()

    if args.list_windows:
        for hwnd, title, pid in enum_windows():
            safe = title.encode('ascii', 'replace').decode('ascii')
            print(f'{hwnd:>10}  pid {pid:>6}  {safe}   [{process_image(pid)}]')
        return 0

    plan = DEFAULT_PLAN
    if args.plan:
        plan = json.loads(Path(args.plan).read_text(encoding='utf-8'))
    only = set(args.only.split(',')) if args.only else None

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    hwnd, title, pid, image = find_window(args.title, args.exe)
    print(f'window {hwnd} "{title}" pid {pid} ({image})')

    placement = WINDOWPLACEMENT()
    placement.length = ctypes.sizeof(WINDOWPLACEMENT)
    user32.GetWindowPlacement(hwnd, ctypes.byref(placement))
    maximized = False
    if not args.no_maximize and placement.showCmd != SW_MAXIMIZE:
        user32.ShowWindow(hwnd, SW_MAXIMIZE)
        maximized = True
        time.sleep(0.5)

    front = bring_front(hwnd)
    print('foreground:', 'yes' if front else 'no (captures still render from the window; keys and clicks will be refused)')
    manifest = {
        'captured_at': datetime.now(timezone.utc).isoformat(timespec='seconds'),
        'window': title,
        'process': image,
        'states': [],
    }
    try:
        for st in plan['states']:
            if only and st['name'] not in only:
                continue
            try:
                for a in st.get('do', []):
                    run_action(hwnd, a)
                time.sleep(st.get('settle_ms', 500) / 1000)
                path = out / f"{st['name']}.png"
                meta = grab(hwnd, path, args.allow_screen_grab, args.screen_grab)
                manifest['states'].append({'name': st['name'], 'file': path.name, **meta})
                print(f'{path}  {meta["size"][0]}x{meta["size"][1]}  ({meta["method"]})')
            except RuntimeError as e:
                print(f'{st["name"]}: skipped: {e}')
                manifest['states'].append({'name': st['name'], 'error': str(e)})
            finally:
                for a in st.get('after', []):
                    try:
                        run_action(hwnd, a)
                    except RuntimeError as e:
                        print(f'{st["name"]}: cleanup skipped: {e}')
                time.sleep(0.2)
    finally:
        if maximized:
            user32.SetWindowPlacement(hwnd, ctypes.byref(placement))
    (out / 'manifest.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    return 0


if __name__ == '__main__':
    sys.exit(main())
