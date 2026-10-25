#!/usr/bin/env python3
"""Drives any Windows desktop app one step at a time and saves what is in front, dialogs included.

    python capture_app.py --title "Everything" --exe Everything64 --out .design-reference/everything \
        key alt+t  wait 400  shot 03-tools-menu  key escape
    python capture_app.py --title "Word" --exe WINWORD click 120,60 shot ribbon-insert

Steps, in order:
    key COMBO        a key combination (ctrl+shift+p, alt+f, escape, f10)
    click X,Y        a click at client pixels of the app's main window
    clickfg X,Y      a click at client pixels of the window that is in front now (a dialog)
    dclick X,Y       a double click at client pixels of the main window
    rclick X,Y       a right click at client pixels of the main window
    type TEXT        types text (use quotes for spaces)
    wait MS          waits
    shot NAME        saves the window in front (a dialog, a menu's owner) as NAME.png
    shotmain NAME    saves the main window
    shotscreen NAME  saves the whole screen (menus that are separate popup windows)
    front            brings the main window to the front again

Every key and click is sent only while the app (or a window of the same process) is in front, so nothing lands in another
program. Reuses the Win32 helpers of capture_t3.py. Windows only.
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

sys.path.insert(0, str(Path(__file__).parent))
import capture_t3 as cap  # noqa: E402

u = cap.user32
KEYEVENTF_UNICODE = 0x0004
MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010


def pid_of(h: int) -> int:
    p = wt.DWORD()
    u.GetWindowThreadProcessId(h, ctypes.byref(p))
    return p.value


def guard(main: int) -> None:
    fg = u.GetForegroundWindow()
    if fg != main and pid_of(fg) != pid_of(main):
        if not cap.bring_front(main):
            raise SystemExit('the app is not in front and cannot be brought there; nothing was sent')


def type_text(text: str) -> None:
    items = []
    for ch in text:
        for up in (False, True):
            inp = cap.INPUT()
            inp.type = cap.INPUT_KEYBOARD
            inp.u.ki = cap.KEYBDINPUT(0, ord(ch), KEYEVENTF_UNICODE | (cap.KEYEVENTF_KEYUP if up else 0), 0, None)
            items.append(inp)
    cap.send_inputs(items)


def grab(h: int, path: Path) -> tuple[int, int]:
    from PIL import ImageGrab

    l, t, r, b = cap.frame_rect(h)
    img = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True)
    img.save(path)
    return img.size


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--title', required=True)
    ap.add_argument('--exe', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('steps', nargs='*')
    a = ap.parse_args()
    try:
        u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    except Exception:
        u.SetProcessDPIAware()
    main_h, title, pid, image = cap.find_window(a.title, a.exe)
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    # A dialog of the same app that is already in front stays in front.
    if pid_of(u.GetForegroundWindow()) != pid_of(main_h):
        cap.bring_front(main_h)
        time.sleep(0.3)
    manifest_path = out / 'manifest.json'
    manifest = json.loads(manifest_path.read_text(encoding='utf-8')) if manifest_path.exists() else {'app': title, 'process': image, 'shots': []}
    s = a.steps
    i = 0
    while i < len(s):
        op = s[i]
        arg = s[i + 1] if i + 1 < len(s) else None
        i += 1
        if op == 'front':
            cap.bring_front(main_h)
            time.sleep(0.3)
            continue
        if op == 'wait':
            time.sleep(int(arg) / 1000)
            i += 1
            continue
        if op in ('key', 'click', 'clickfg', 'dclick', 'rclick', 'type'):
            guard(main_h)
        if op == 'key':
            cap.press(arg)
            time.sleep(0.35)
        elif op in ('click', 'dclick', 'rclick', 'clickfg'):
            x, y = (int(v) for v in arg.split(','))
            target = u.GetForegroundWindow() if op == 'clickfg' else main_h
            l, t, _, _ = cap.client_rect(target)
            if op == 'rclick':
                cap.move_to(l + x, t + y)
                time.sleep(0.08)
                cap.send_inputs([cap.mouse_input(MOUSEEVENTF_RIGHTDOWN)])
                time.sleep(0.04)
                cap.send_inputs([cap.mouse_input(MOUSEEVENTF_RIGHTUP)])
            else:
                cap.click_at(l + x, t + y)
                if op == 'dclick':
                    time.sleep(0.05)
                    cap.click_at(l + x, t + y)
            time.sleep(0.45)
        elif op == 'type':
            type_text(arg)
            time.sleep(0.3)
        elif op in ('shot', 'shotmain', 'shotscreen'):
            time.sleep(0.25)
            path = out / f'{arg}.png'
            if op == 'shotscreen':
                from PIL import ImageGrab

                img = ImageGrab.grab(all_screens=True)
                img.save(path)
                size = img.size
                win = 'screen'
            else:
                h = main_h if op == 'shotmain' else u.GetForegroundWindow()
                if op == 'shot' and pid_of(h) != pid_of(main_h):
                    print(f'{arg}: the window in front belongs to another process; saving the main window instead')
                    h = main_h
                n = u.GetWindowTextLengthW(h)
                buf = ctypes.create_unicode_buffer(n + 1)
                u.GetWindowTextW(h, buf, n + 1)
                win = buf.value
                size = grab(h, path)
            manifest['shots'] = [x for x in manifest['shots'] if x['name'] != arg] + [{'name': arg, 'window': win, 'size': list(size), 'at': datetime.now(timezone.utc).isoformat(timespec='seconds')}]
            print(f'{path}  {size[0]}x{size[1]}  [{win}]')
        else:
            raise SystemExit(f'unknown step {op!r}')
        if op in ('key', 'click', 'clickfg', 'dclick', 'rclick', 'type', 'shot', 'shotmain', 'shotscreen'):
            i += 1
    manifest_path.write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    return 0


if __name__ == '__main__':
    sys.exit(main())
