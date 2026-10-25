// Icons on a 24 px grid with a 1.5 px stroke and round caps: the Ogun workshop set plus the interface icons the
// shell needs (search, play, bolt, chevron, appearance), and the Ogun and Verne marks.

const PATHS = {
  anvil:
    '<path d="M2.5 7.5h4.2l1.3-1h13.5v2.6l-2.8.6c-1.6.4-2.4 1.6-2.4 2.8 0 1 .6 1.8 1.6 2.2l.6.8H8.2l.6-.8c1-.4 1.6-1.2 1.6-2.2 0-1.4-1.1-2.6-2.8-3.1-2-.6-3.8-1.4-5.1-1.9z"/><path d="M6 19.5h12"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2.8v2.4M12 18.8v2.4M2.8 12h2.4M18.8 12h2.4M5.5 5.5l1.7 1.7M16.8 16.8l1.7 1.7M5.5 18.5l1.7-1.7M16.8 7.2l1.7-1.7"/><circle cx="12" cy="12" r="6.6"/>',
  bucket: '<path d="M5 8h14l-1.6 12H6.6z"/><path d="M5 8c0-3.6 14-3.6 14 0M12 3v2"/>',
  drawers:
    '<rect x="3.5" y="3.5" width="17" height="17" rx="1.5"/><path d="M3.5 9.2h17M3.5 14.8h17M10.4 6.4h3.2M10.4 12h3.2M10.4 17.6h3.2"/>',
  sieve:
    '<path d="M3 8h18l-2.4 7a4 4 0 0 1-3.8 2.8H9.2A4 4 0 0 1 5.4 15z"/><path d="M8 11.4h.01M12 11.4h.01M16 11.4h.01M10 14.4h.01M14 14.4h.01M12 18v3"/>',
  seal: '<circle cx="12" cy="10" r="6.2"/><path d="M9.4 15.4 8 21.6l4-2 4 2-1.4-6.2"/><path d="M9.6 10l1.6 1.6 3.2-3.2"/>',
  ledger:
    '<path d="M5 3.5h12.5a1.5 1.5 0 0 1 1.5 1.5v15.5H6.5A1.5 1.5 0 0 1 5 19z"/><path d="M5 17.2h14M8.6 7.4h7M8.6 10.6h7M8.6 13.8h4"/>',
  compass: '<path d="M12 3v3M12 6l-6 15M12 6l6 15M8.4 15h7.2"/><circle cx="12" cy="6" r="1.4"/>',
  hourglass:
    '<path d="M6.4 3h11.2M6.4 21h11.2M7.6 3c0 4.4 4.4 5.6 4.4 9s-4.4 4.6-4.4 9M16.4 3c0 4.4-4.4 5.6-4.4 9s4.4 4.6 4.4 9"/>',
  dag: '<circle cx="5" cy="6" r="2"/><circle cx="5" cy="18" r="2"/><circle cx="12" cy="12" r="2"/><circle cx="19" cy="12" r="2"/><path d="M6.6 7.2 10.4 10.8M6.6 16.8l3.8-3.6M14 12h3"/>',
  cube: '<path d="M12 2.8 20 7.2v9.6L12 21.2 4 16.8V7.2z"/><path d="M4 7.2l8 4.4 8-4.4M12 11.6v9.6"/>',
  shield: '<path d="M12 3l7.6 2.8v5.6c0 4.8-3.2 8.2-7.6 9.6-4.4-1.4-7.6-4.8-7.6-9.6V5.8z"/><path d="M9 12l2.2 2.2 4-4.2"/>',
  matrix: '<rect x="3.5" y="3.5" width="17" height="17" rx="1.2"/><path d="M3.5 9.2h17M3.5 14.8h17M9.2 3.5v17M14.8 3.5v17"/>',
  database:
    '<ellipse cx="12" cy="5.6" rx="7.4" ry="2.6"/><path d="M4.6 5.6v12.8c0 1.4 3.3 2.6 7.4 2.6s7.4-1.2 7.4-2.6V5.6M4.6 12c0 1.4 3.3 2.6 7.4 2.6s7.4-1.2 7.4-2.6"/>',
  lock: '<rect x="4.5" y="10.5" width="15" height="10" rx="1.6"/><path d="M8 10.5V7.6a4 4 0 0 1 8 0v2.9M12 14.6v2.2"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.3-4.3"/>',
  gauge: '<path d="M4 14a8 8 0 1 1 16 0"/><path d="M12 14l3.5-4.5"/><path d="M4 18h16"/>',
  play: '<path d="M7 4.8v14.4a.6.6 0 0 0 .9.5l11.5-7.2a.6.6 0 0 0 0-1L7.9 4.3a.6.6 0 0 0-.9.5z"/>',
  wave: '<path d="M3 12c2 0 2-4 4-4s2 8 4 8 2-8 4-8 2 4 4 4"/>',
  bolt: '<path d="M13 2 4.5 13.5H11L10 22l8.5-11.5H13z"/>',
  refresh: '<path d="M20 11A8 8 0 0 0 5.6 6.6L4 8"/><path d="M4 3v5h5"/><path d="M4 13a8 8 0 0 0 14.4 4.4L20 16"/><path d="M20 21v-5h-5"/>',
  chevron: '<path d="m6 9 6 6 6-6"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5M12 8h.01"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4"/>',
  moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
  contrast: '<circle cx="12" cy="12" r="8.5"/><path d="M12 3.5v17A8.5 8.5 0 0 0 12 3.5z" fill="currentColor"/>',
  x: '<path d="M6 6l12 12M18 6 6 18"/>',
  layers: '<path d="M12 3 3 8l9 5 9-5-9-5z"/><path d="m3 12 9 5 9-5M3 16l9 5 9-5"/>',
  lock2: '<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>',
  upload: '<path d="M12 16V4M7 9l5-5 5 5M4 20h16"/>',
  download: '<path d="M12 4v12M7 11l5 5 5-5M4 20h16"/>',
  up: '<path d="M12 19V5M6 11l6-6 6 6"/>',
  down: '<path d="M12 5v14M6 13l6 6 6-6"/>',
  sliders: '<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h12M20 18h0"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>',
  palette: '<path d="M12 3a9 9 0 1 0 0 18c1.2 0 2-.8 2-1.8 0-.6-.3-1-.6-1.4-.3-.4-.6-.8-.6-1.3 0-1 .8-1.7 1.8-1.7H17a4 4 0 0 0 4-4C21 6.2 17 3 12 3z"/><circle cx="7.5" cy="11" r="1"/><circle cx="10" cy="7" r="1"/><circle cx="15" cy="7" r="1"/>',
  keyboard: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  plug: '<path d="M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0V8zM12 17v4"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7v5l3 2"/>',
  archive: '<rect x="3" y="4" width="18" height="4" rx="1"/><path d="M5 8v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8M10 12h4"/>',
  sparkle: '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/><path d="M18 16l.7 1.8 1.8.7-1.8.7L18 21l-.7-1.8-1.8-.7 1.8-.7z"/>',
  arrowleft: '<path d="M19 12H5M11 6l-6 6 6 6"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>',
  check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  panel: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>',
  external: '<path d="M14 4h6v6M20 4l-9 9M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/>',
  flask: '<path d="M9 3h6M10 3v6L5 19a1.5 1.5 0 0 0 1.3 2.2h11.4A1.5 1.5 0 0 0 19 19l-5-10V3"/>',
  code: '<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 6l-3 12"/>',
  globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.5 2.4 3.6 5.2 3.6 8.5s-1.1 6.1-3.6 8.5c-2.5-2.4-3.6-5.2-3.6-8.5S9.5 5.9 12 3.5z"/>',
  bell: '<path d="M6 17V11a6 6 0 1 1 12 0v6l1.5 2h-15zM10 21h4"/>',
  compose: '<path d="M4 20h4L19 9a2.1 2.1 0 0 0-3-3L5 17v3z"/><path d="m14.5 7.5 3 3"/>',
  send: '<path d="M12 19V5M6 11l6-6 6 6"/>',
  stop: '<rect x="6.5" y="6.5" width="11" height="11" rx="2"/>',
  attach: '<path d="m20 11-8.2 8.2a5 5 0 0 1-7-7L13 4a3.3 3.3 0 0 1 4.7 4.7L9.5 17a1.7 1.7 0 0 1-2.4-2.4L14 7.7"/>',
  chat: '<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4h13A1.5 1.5 0 0 1 20 5.5v9a1.5 1.5 0 0 1-1.5 1.5H10l-4.5 4v-4A1.5 1.5 0 0 1 4 14.5z"/><path d="M8 9h8M8 12h5"/>',
  panelright: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/>',
  wrench: '<path d="M14.7 6.3a4 4 0 0 0-5.4 5.2L3.5 17.3a1.8 1.8 0 0 0 2.5 2.5l5.8-5.8a4 4 0 0 0 5.2-5.4l-2.6 2.6-2.1-.5-.5-2.1z"/>',
  at: '<circle cx="12" cy="12" r="3.6"/><path d="M15.6 8.4v4.6a2.2 2.2 0 0 0 4.4 0V12a8 8 0 1 0-3.2 6.4"/>',
  chevronright: '<path d="m9 6 6 6-6 6"/>',
  eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="2.8"/>',
  database2: '<ellipse cx="12" cy="6" rx="7" ry="2.6"/><path d="M5 6v12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6V6"/>',
} as const

export type IconName = keyof typeof PATHS

export function Icon({ name }: { name: IconName }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: PATHS[name] }}
    />
  )
}

const GEAR =
  'M 14.81 -2.01 L 20.75 -1.49 L 20.75 1.49 L 14.81 2.01 L 14.22 4.62 L 19.34 7.66 L 18.04 10.35 L 12.48 8.24 L 10.81 10.33 L 14.1 15.29 L 11.77 17.15 L 7.67 12.83 L 5.25 14 L 6.07 19.89 L 3.16 20.56 L 1.34 14.89 L -1.34 14.89 L -3.16 20.56 L -6.07 19.89 L -5.25 14 L -7.67 12.83 L -11.77 17.15 L -14.1 15.29 L -10.81 10.33 L -12.48 8.24 L -18.04 10.35 L -19.34 7.66 L -14.22 4.62 L -14.81 2.01 L -20.75 1.49 L -20.75 -1.49 L -14.81 -2.01 L -14.22 -4.62 L -19.34 -7.66 L -18.04 -10.35 L -12.48 -8.24 L -10.81 -10.33 L -14.1 -15.29 L -11.77 -17.15 L -7.67 -12.83 L -5.25 -14 L -6.07 -19.89 L -3.16 -20.56 L -1.34 -14.89 L 1.34 -14.89 L 3.16 -20.56 L 6.07 -19.89 L 5.25 -14 L 7.67 -12.83 L 11.77 -17.15 L 14.1 -15.29 L 10.81 -10.33 L 12.48 -8.24 L 18.04 -10.35 L 19.34 -7.66 L 14.22 -4.62 Z'
const ANVIL =
  'M -8.4 -1.4 C -7 -1.4 -5.5 -1.6 -4.6 -1.8 L 6.4 -1.8 L 6.4 -0.4 L 4.8 -0.2 C 3.8 .2 3.3 1.2 3.2 2 C 3.1 2.8 3.5 3.3 4.4 3.7 L 5 4.3 L -4 4.3 L -3.4 3.7 C -2.6 3.3 -2.3 2.8 -2.4 2 C -2.6 1.1 -3.4 .4 -4.6 .1 C -6 -.2 -7.4 -.7 -8.4 -1.4 Z'
const SPARK = 'M 1.6 -9 L 2.2 -5.6 L 5 -6.6 L 2.6 -4.6 L 4.6 -2.6 L 1.6 -3.8 L .4 -1.2 L .4 -4.4 L -2.6 -4.2 L -.4 -5.8 L -2 -8 L .9 -6.2 Z'

/** The Ogun mark: a 14-tooth gear ring around an anvil, with one ember spark. */
export function Mark({ title = 'Ogun' }: { title?: string }) {
  return (
    <svg viewBox="-22 -22 44 44" role="img" aria-label={title}>
      <path d={GEAR} fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" />
      <circle r="12.75" fill="none" stroke="currentColor" stroke-width=".8" opacity=".55" />
      <path d={ANVIL} fill="currentColor" />
      <path d={SPARK} fill="var(--primary)" />
    </svg>
  )
}

const VERNE =
  'M 62 517 C 34 470 20 418 20 360 C 20 175 165 20 350 20 C 478 20 562 78 622 152 C 676 220 700 268 742 296 C 790 328 852 340 920 340 C 990 340 1056 320 1104 286 C 1146 256 1170 214 1212 166 C 1290 70 1370 20 1492 20 C 1676 20 1830 172 1830 356 C 1830 548 1680 690 1492 690 C 1384 690 1288 640 1228 556 C 1176 484 1114 442 1035 442 C 938 442 886 510 886 580 C 886 606 893 630 905 652 C 1010 846 1118 1030 1222 1222 C 1248 1270 1260 1324 1260 1378 C 1260 1566 1104 1710 890 1710 C 784 1710 690 1632 632 1530 C 444 1192 254 856 62 517 Z'

/** Verne's loop V, in Verne's gradient: the only place Verne's colours appear on a product surface. */
export function VerneMark() {
  return (
    <svg viewBox="-60 -60 1972 1852" aria-hidden="true">
      <defs>
        <linearGradient id="verne-g" x1="0" y1="0" x2="1852" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0" stop-color="#00F5C8" />
          <stop offset=".5" stop-color="#00B4DC" />
          <stop offset="1" stop-color="#005BFF" />
        </linearGradient>
      </defs>
      <path d={VERNE} fill="none" stroke="url(#verne-g)" stroke-width="130" stroke-linejoin="round" />
    </svg>
  )
}
