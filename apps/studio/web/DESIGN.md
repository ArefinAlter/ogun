# Studio interface conventions

Ogun Studio follows the interface conventions of T3 Code (the desktop app for coding agents by T3 Tools, MIT),
as read from its shipped stylesheet and from screenshots of the installed app (captured with
`tools/ui-reference/capture_t3.py`). This file is the distilled list, so the desktop, web and mobile shells, and
any later Ogun surface, stay consistent. Tokens live in `src/styles/tokens.css`; components in
`src/styles/app.css`.

## Tokens (the names T3 Code uses)

| Role | Light | Dark |
|---|---|---|
| `--background` | zinc 25 (`oklch(99.2% 0 0)`) | neutral 950 (`#0a0a0a`) |
| `--foreground` | zinc 800 | neutral 100 |
| `--card`, `--popover` | white | background mixed with 3 to 5 % white |
| `--muted`, `--accent` (hover) | zinc 50, zinc 100 | white at 3 %, 4 % |
| `--muted-foreground` | zinc 500 | `#818181` |
| `--border`, `--input` | zinc 200, zinc 300 | white at 6 %, 8 % |
| `--primary` | ember `#9c4419` on white | ember `#e8662e` on near-black |
| `--success`, `--warning`, `--error`, `--info` and their `-foreground` | emerald, amber, red, blue 500 / 700 | the 400 tones |
| `--sidebar`, `--sidebar-row-hover`, `--sidebar-row-selected` | zinc 50, zinc 25, white | card, accent, muted |
| `--radius` | 10 px; sm 6, md 8, xl 14 | same |

One coloured role only: `--primary` is Ogun's ember and appears on the active icon, the primary button, running
states, links and the spark in the mark. Everything else is neutral. Fonts are the system stack (Segoe UI,
San Francisco, system-ui) and the system monospace, exactly as in T3 Code; nothing is fetched or bundled.

## Layout

- **Sidebar, 272 px**, tinted (`--sidebar`), hairline on the right. Brand row 40 px with the sidebar toggle at its
  left (the toggle moves to the top bar when the sidebar is closed); a New thread row where the daemon has agent
  threads; a Search row with
  `Ctrl` `K` chips; groups with a sentence-case label and a hairline ("Operate", "Trust", "Data"); 32 px rows
  with a 16 px icon, rounded 8 px; the selected row is white with a hairline ring and an ember icon. A list of
  stored workflows follows, each row with a state dot, the name and "v2 · 25 min" at the right, the way T3 Code
  lists threads. Icon buttons and the connection status sit at the bottom.
- **Top bar, 40 px**: a breadcrumb (`profile / Page / id`), the right side for status pills and controls.
  In the desktop app the brand row and this bar are the window's title bar (below).
- **Content** centred, up to 1 240 px, 32 px side padding, 14 px text.
- **Phone**: the sidebar becomes a bottom bar; everything stacks.

## The window frame (desktop)

T3 Code has no separate operating-system title bar: its top row is the title bar, and the minimize, maximize and
close glyphs sit at the right end of it. Studio does the same in the Windows shell.

- The main window has no native frame (`decorations: false` in `tauri.windows.conf.json`, with the system shadow).
  The sidebar's brand row and the top bar carry `data-tauri-drag-region="deep"`: a press moves the window, a double
  click maximizes or restores it, and a button or a link in the bar keeps its own press.
- Controls are 46 px wide and the bar's height, thin 10 px glyphs, the accent tint under the pointer, and the system
  red on close (`src/ui/WindowControls.tsx`). They are drawn only when `isDecorated()` is false, so a window that still
  has its native frame never gets a second set.
- Resizing uses the invisible 8 px band that the system keeps around an undecorated window.
- macOS asks for an overlay title bar (`tauri.macos.conf.json`), so the system draws the traffic lights over the top
  left and the bars leave 76 px for them. Linux keeps its native frame. Neither is tested yet.
- The capability grants only `minimize`, `toggle-maximize`, `internal-toggle-maximize`, `close`, `start-dragging`,
  `is-maximized` and `is-decorated` to the main window. `tools/ui-reference/chrome_check.py` presses every control with
  real mouse input.

## The agent screen

A conversation in the manner of T3 Code's thread view, on Ogun's rules (O-17).

- **Empty thread:** the mark, "What should Ogun work on?", a list of ready-made starting points, and the composer.
- **Composer:** a rounded card, 20 px radius, a textarea that grows to about 9 lines, and a row under it: model picker,
  access picker, attach (`@`), and a round send button that becomes Stop while the agent works. Enter sends,
  Shift+Enter breaks the line, `@` points at a workflow, run, approval or connector, `/` lists ready-made requests.
- **Timeline:** a centred 760 px column. A person's message is a soft bubble at the right; the agent's words are plain
  text at full width, rendered from a safe Markdown subset (no HTML, links only to Studio or `https`).
- **Tool calls** fold into one quiet line, "Worked · 3 tool calls · 1.7 s", that opens to each call with its input and
  what the daemon answered. A refused call says why and is counted in the line.
- **Cards** carry everything with consequences and are made from the daemon's answers: a proposed workflow (validated,
  with its hash and what waits for a person), a backfill plan, a retry, pending approvals, a run, a table, a connector.
  A card's button is pressed by the person and named for it ("Store as you"). A card of approvals has no decide button.
- **Side panel (320 px):** state, model, tool-call count, rename and delete, the access in this thread, the tool list
  with the closed ones locked, what was pointed at, and what no setting changes.
- **Access** is per thread: Read only (the default), Author, Operate. The agent never raises its own.
- The scripted agent is called "Ogun agent"; its entry in the model picker and the side panel say the answers are prepared and no model is called.

## Components

- Cards: white, 1 px border, 10 px radius, 1 px shadow, 16 to 20 px padding. Settings-style "rows" (title,
  description, control at the right) share one card with hairline dividers.
- Tables inside cards: 12 px muted headers, 13 px cells, hover tint; ids and digests monospaced.
- Buttons 32 px, 8 px radius, 13 px medium; primary is ember; danger is red text; small is 28 px; ghost has no
  border. Inputs 32 px with a chevron select; a 3 px ring on focus.
- State pills: a dot, the exact state word in monospace, a tinted surface (running ember, succeeded emerald,
  failed and retry red, waiting-for-user amber, waiting-for-session blue, the rest neutral).
- Notices are quiet grey boxes; warnings and errors tint the border. Text from outside Ogun sits in a dashed amber
  box labelled "not verified by Ogun".
- Command palette on Ctrl+K: a 580 px panel, grouped items with icons, `kbd` hints in the footer.
- Toasts top right, 380 px, a dot for severity, a title and one line.
- Dialogs 620 px, 14 px radius; approval dialogs stay plain: facts in a fixed order, two equal buttons, the
  confirm button names the decision and arms after 1.5 s.

## Rules that are Ogun's, not T3 Code's

- States are always the exact word with colour as a second channel.
- Approval surfaces carry no ornament, no animation and nothing automatic.
- Secrets never reach the page. Studio shows the *name* of a secret and the command that stores it
  (`ogun secrets set NAME`); no screen has a field for a value.
- No telemetry and no third-party asset. Appearance, keybindings and notification choices are device preferences in
  the webview's own storage (`ogun-studio:prefs:v1`); the hosted web demo keeps them in memory only, because the
  site it lives on promises no cookies and no local storage (`VITE_OGUN_NO_STORAGE=1`).
- Settings that are fixed by a rule (approvals are human-only, the lineage rule, the sandbox) are shown as rows
  marked "fixed", not hidden and not editable.
- A settings page whose daemon method does not exist yet is read-only and says "this daemon does not ...".
- The webview runs with a frozen `Object.prototype` (Tauri `freezePrototype`). The build patches Preact's one
  object copy that assigns `constructor` (`vite.config.ts`); `smoke_studio.py --freeze-prototype` checks it.

## Settings

Sections follow T3 Code's settings, mapped to what Ogun has: General, Appearance, Keybindings, Evidence (T3's
snapshots), Providers (the `llm.json` ladder), Connectors (the catalog of external systems, O-16), Connections
(named uses of a connector), Schedules, Workflow sources (T3's source control), Storage and archive, Environments
(T3's connections: this machine, a hub, the Zeus bridge) and Security. Specified in O-16 section 6.
