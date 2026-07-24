# Collapsible Sidebar — Design Spec

## 1. Purpose

The Hub GUI's left sidebar (`app/src/App.svelte`'s `.side` column: Toolbar +
SessionList + Settings) is a fixed 280px column that always takes space from
the terminal grid. Users want to reclaim that width when they don't need the
session list in view (e.g. focusing on one big terminal tile).

## 2. Behavior

- **Collapsed = fully hidden.** `.side` shrinks to 0 width (not an icon rail).
  The grid expands to fill the freed space.
- **Persisted** across app restarts (`localStorage`), so leaving it collapsed
  sticks on next launch.
- **Toggle, two controls depending on state** (see rationale below):
  - **Expanded:** a `‹` button inline in `Toolbar.svelte`, first item in its
    row, next to "+ New session". Normal toolbar button, no overlap, no
    special positioning.
  - **Collapsed:** the toolbar disappears along with the rest of `.side`, so a
    separate small `›` button renders **only while collapsed**, to bring the
    sidebar back.

### Why two controls instead of one

Putting the only toggle inside `.side` would make it vanish the moment the
sidebar collapses to 0 width — a dead end with no way back. So the collapsed
state needs a control that lives outside `.side`'s DOM entirely.

### Collapsed-state button requirements (explicit, per user request)

The `›` button must be **fully visible** whenever the sidebar is collapsed:
- `position: fixed` (not inside `Grid`'s pan/zoom canvas — see
  `store.ts`'s `canvasView`/`panBy`/`zoomBy` — so it never moves or scales
  with the canvas transform and can't be panned/zoomed off-screen).
- Comfortable corner offset (e.g. `top/left: 12px`) so it's never flush
  against the window edge or clipped by macOS window-corner rounding.
- Solid background + border + shadow so it reads clearly against whatever
  terminal content/tiles are behind it (the grid is otherwise dark, low
  contrast).
- `z-index` high enough to sit above tiles (`TileFrame`/`Terminal` stack via
  `bringToFront`/`tileGeom[].z`) so an in-front tile can never cover it.

## 3. Implementation

- **`app/src/lib/store.ts`:**
  - `sidebarCollapsed` — writable, seeded from
    `localStorage.getItem("hub.sidebarCollapsed") === "1"`.
  - `toggleSidebar()` — flips the store and writes the new value back to
    `localStorage`.
- **`app/src/App.svelte`:**
  - `.side` gets `class:collapsed={$sidebarCollapsed}`; collapsed CSS sets
    `width: 0; opacity: 0; border-right: 0; overflow: hidden;` with a short
    (~160ms) transition on `width`/`opacity` for a smooth collapse/expand.
  - Renders the fixed-position `›` re-expand button, gated on
    `{#if $sidebarCollapsed}`.
- **`app/src/lib/Toolbar.svelte`:**
  - New props `collapsed: boolean` and `onToggle: () => void`.
  - Renders the `‹` button first in the `.toolbar` flex row, calling
    `onToggle` (App.svelte wires this to `toggleSidebar()`).

## 4. Out of scope

- No icon-rail / partially-collapsed intermediate state.
- No keyboard shortcut (not requested).
- No changes to `Grid`, `Terminal`, or any Rust/backend code — this is a
  frontend-only layout toggle.

## 5. Testing

- Manual verification in the running Tauri/dev-server app: toggle collapse
  from both controls, confirm grid reflows to fill freed width, confirm
  reload preserves the collapsed/expanded state, confirm the `›` button stays
  fully visible and clickable after panning/zooming the grid canvas and with
  a tile dragged on top of its corner.
