# UI reference captures

Small tools that take the pictures Ogun's interfaces are designed against and check that they work. Both write PNGs and a
`manifest.json` into `.design-reference/` (git-ignored: the T3 Code pictures show whatever is in the running app,
and the Studio pictures are rebuilt on demand).

## `capture_t3.py`: the installed T3 Code desktop app

Drives the running T3 Code window on Windows the way a person would, and saves one PNG per state.

```sh
python tools/ui-reference/capture_t3.py --out .design-reference/t3code                       # main window, settings
python tools/ui-reference/capture_t3.py --out .design-reference/t3code --plan tools/ui-reference/t3-plan.json
python tools/ui-reference/capture_t3.py --list-windows
```

- It finds the window by title and by the process executable (`t3code`), maximises it while capturing, and
  restores its placement afterwards (`--no-maximize` keeps the size).
- The picture is rendered from the window itself (`PrintWindow` with full content), so it is correct even when
  another window covers T3 Code. `--allow-screen-grab` enables the screen fallback.
- Keys and clicks are sent only after the tool has checked that T3 Code really is the foreground window; if it
  cannot be brought to the front, those steps are skipped and reported, so nothing lands in another app.
- `t3-plan.json` is the state list: search, an open thread, the right and bottom panels, the sidebar collapsed,
  the model and permission pickers, the theme picker (Ctrl+Alt+A), the command palette (Ctrl+K), every settings
  section, source control, usage, and the appearance cycle (Ctrl+Alt+Shift+A, pressed three times so it ends
  where it started). Click positions are client pixels of the maximised 1920 × 1032 window; adjust them for
  another screen.
- Needs Python 3.10+ and Pillow. Everything else is ctypes against user32, gdi32, kernel32 and dwmapi.

## `shoot_studio.py`: Ogun Studio pages through headless Chrome

```sh
cd apps/studio/web && npm run build:demo && npm run preview:demo &      # http://localhost:4174
python tools/ui-reference/shoot_studio.py --base http://localhost:4174 --out .design-reference/studio
```

The built-in plan covers every page on the scripted data, including the backfill run's graph and events,
the armed approval dialog, an artifact's CSV preview and lineage, the command palette, the demo menu, dark mode
and a phone viewport. A plan file can replace it: `hash` navigates, `then` navigates to the string a JavaScript
expression returns, `js` runs JavaScript for its side effects, `click` clicks a selector, `key` presses
Ctrl+key, `theme` sets light or dark, `width` and `height` set the viewport, `full_page: false` keeps the
viewport only. Needs Chrome, `websockets` and `requests`.

## `capture_t3.py` on Ogun's own desktop app

The same tool captures the scripted desktop app (`npm run build:desktop-demo:app` in `apps/studio/web`):

```sh
python tools/ui-reference/capture_t3.py --title "Ogun Studio" --exe ogun-studio --screen-grab   --plan tools/ui-reference/desktop-demo-plan.json --out .design-reference/desktop-demo
```

`--screen-grab` is required for a WebView2 window: `PrintWindow` returns its frame without the page. The window
must be in front, which the tool checks. `desktop-demo-plan.json` uses Studio's own shortcuts (Alt+3 Runs, Alt+5
Approvals, Alt+9 Connectors, Ctrl+K, Ctrl+comma).

## `smoke_studio.py`: Studio's flows through headless Chrome

```sh
python tools/ui-reference/smoke_studio.py --base http://localhost:4174                    # scripted build
python tools/ui-reference/smoke_studio.py --base https://example/demo/ --expect-no-storage # hosted build
python tools/ui-reference/smoke_studio.py --base http://localhost:4174 --freeze-prototype  # as in the Tauri shell
```

It does what a person does (changes a theme, records a shortcut, adds a connection, edits a ladder, approves a
request) and checks the result, then fails on any console error. `--expect-no-storage` also checks that nothing
reached local or session storage. `--freeze-prototype` freezes `Object.prototype` before the page runs, the way
the Tauri shell does. `serve_site.py` serves an `ogun-site` build with its real security headers for a local run.

## `chrome_check.py`: the desktop window frame with real mouse input

```sh
python tools/ui-reference/chrome_check.py --title "Ogun Studio" --exe ogun-studio --close
```

The Windows shell has no native title bar. This presses the page's own controls with `SendInput`, only while the window
is in front: maximize and restore, a double click on the bar, dragging the bar to move the window, dragging the corner
to resize it, minimize and (with `--close`) close. It fails if any of them does nothing.

## `smoke_agent.py` and `hub_check.py`

`smoke_agent.py` is the agent screen's part of `smoke_studio.py`: access levels, Stop, the store-by-press rule, a tool
switched off in Settings, secret-looking instructions, `@` and `/`, the side panel, rename and delete.
`hub_check.py` signs in to a running hub through its identity provider's own form and visits every page of Studio-web
(`deploy/demo-hub`, through an SSH tunnel); the password comes from `OGUN_HUB_PASSWORD` and is never printed.

## What the T3 Code pictures taught us

`apps/studio/web/DESIGN.md` is the distilled list of conventions Studio adopted: the token names and values,
the 272 px grouped sidebar, the 40 px breadcrumb bar, cards with hairline dividers, 32 px controls, state pills,
the Ctrl+K palette, toasts and the dialog rules.
