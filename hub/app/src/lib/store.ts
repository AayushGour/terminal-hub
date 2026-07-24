// Task 7: shared session-list state. SessionList.svelte writes into these on
// refresh(); App.svelte reads openTiles to know which Terminal tiles to mount.
import { get, writable } from "svelte/store";
import type { SessionInfo, GhostRecord } from "./api";
import { hubIsInstalled } from "./api";

export const healthy = writable<SessionInfo[]>([]);
export const ghost = writable<GhostRecord[]>([]);
export const orphan = writable<SessionInfo[]>([]);
export const openTiles = writable<number[]>([]); // session ids currently in the grid

// --- loaders / busy state (issue #4) ---
// `true` while a session-list reconcile is in flight (spinner by the header).
export const listBusy = writable<boolean>(false);
// session ids with an in-flight kill/detach op, so their row shows a spinner
// and the buttons disable (no double-fire).
export const busyIds = writable<Set<number>>(new Set());
export function setBusy(id: number, on: boolean) {
  busyIds.update((s) => {
    const next = new Set(s);
    if (on) next.add(id);
    else next.delete(id);
    return next;
  });
}

// --- shared install state (issue #6) ---
// One source of truth for "is hub installed", so the first-run consent popup
// and Settings can never disagree: installing from EITHER flips this, and the
// popup auto-dismisses. `null` = not yet known.
export const installed = writable<boolean | null>(null);
export async function refreshInstalled(): Promise<boolean> {
  try {
    const ok = await hubIsInstalled();
    installed.set(ok);
    return ok;
  } catch {
    installed.set(false); // fail-safe: unknown => treat as not installed
    return false;
  }
}

// --- sidebar collapse toggle (persisted) ---
const SIDEBAR_KEY = "hub.sidebarCollapsed";
export const sidebarCollapsed = writable<boolean>(
  typeof localStorage !== "undefined" && localStorage.getItem(SIDEBAR_KEY) === "1",
);
export function toggleSidebar() {
  sidebarCollapsed.update((v) => {
    const next = !v;
    if (typeof localStorage !== "undefined") localStorage.setItem(SIDEBAR_KEY, next ? "1" : "0");
    return next;
  });
}

// --- free-form tile geometry (drag to move / resize) ---
// Each open tile is a floating window with a position + size + stacking order.
// Kept in-memory (resets on reload); a tile gets a cascaded default on open.
export interface TileGeom { x: number; y: number; w: number; h: number; z: number; }
export const tileGeom = writable<Record<number, TileGeom>>({});
let topZ = 0;
// Gap (world px) between a new tile and its neighbors, and inset from the
// viewport edge for the very first candidate position.
const MARGIN = 24;
export const DEFAULT_W = 520;
export const DEFAULT_H = 340;

// Screen-px size of the Grid viewport DOM node, kept in sync by a
// ResizeObserver in Grid.svelte (mirrors the ResizeObserver pattern
// Terminal.svelte uses for pty sizing). Lets ensureGeom() anchor a new tile's
// search to whatever part of the unbounded canvas the user is currently
// looking at, instead of a fixed world coordinate they'd have to pan to find.
// {w: 0, h: 0} (not yet measured) is a valid state -- callers fall back to a
// sane default.
export const viewportSize = writable<{ w: number; h: number }>({ w: 0, h: 0 });

export function bringToFront(id: number) {
  topZ += 1;
  const z = topZ;
  tileGeom.update((g) => (g[id] ? { ...g, [id]: { ...g[id], z } } : g));
}
export function setGeom(id: number, patch: Partial<TileGeom>) {
  tileGeom.update((g) => (g[id] ? { ...g, [id]: { ...g[id], ...patch } } : g));
}

type Box = { x: number; y: number; w: number; h: number };
function boxesOverlap(a: Box, b: Box): boolean {
  return a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
}

// Scan a grid of DEFAULT_W x DEFAULT_H candidate slots, row-major, starting at
// (originX, originY) and growing outward (more rows, more columns) until one
// doesn't overlap any existing tile. This always terminates: MAX_ROWS is a
// hard safety valve (unreachable in practice -- it'd mean every one of
// maxCols * MAX_ROWS slots is occupied), so a spot is always returned.
function findFreeSpot(existing: Box[], originX: number, originY: number, maxCols: number): { x: number; y: number } {
  const stepX = DEFAULT_W + MARGIN;
  const stepY = DEFAULT_H + MARGIN;
  const cols = Math.max(1, maxCols);
  const MAX_ROWS = 500;
  for (let row = 0; row < MAX_ROWS; row++) {
    for (let col = 0; col < cols; col++) {
      const x = originX + col * stepX;
      const y = originY + row * stepY;
      const candidate: Box = { x, y, w: DEFAULT_W, h: DEFAULT_H };
      if (!existing.some((g) => boxesOverlap(candidate, g))) return { x, y };
    }
  }
  // Unreachable in practice; still a deterministic, non-overlapping-with-origin
  // fallback so we never fail to return a position.
  return { x: originX, y: originY + MAX_ROWS * stepY };
}

function ensureGeom(id: number) {
  tileGeom.update((g) => {
    if (g[id]) return g;
    topZ += 1;
    const existing = Object.values(g);
    // Anchor the search to the world coords currently visible in the
    // viewport (screen = world*zoom + pan, so world = (screen - pan)/zoom),
    // inset by MARGIN from the top-left corner -- new tiles land where the
    // user is looking, not off in some far-away fixed spot.
    const { panX, panY, zoom } = get(canvasView);
    const vp = get(viewportSize);
    const originX = (MARGIN - panX) / zoom;
    const originY = (MARGIN - panY) / zoom;
    const viewCols = vp.w > 0 ? Math.floor(vp.w / zoom / (DEFAULT_W + MARGIN)) : 0;
    const maxCols = Math.min(10, Math.max(1, viewCols || 4));
    const { x, y } = findFreeSpot(existing, originX, originY, maxCols);
    return { ...g, [id]: { x, y, w: DEFAULT_W, h: DEFAULT_H, z: topZ } };
  });
}
function dropGeom(ids: Set<number>) {
  tileGeom.update((g) => {
    const next: Record<number, TileGeom> = {};
    for (const k of Object.keys(g)) {
      const id = Number(k);
      if (!ids.has(id)) next[id] = g[id];
    }
    return next;
  });
}

// --- canvas viewport: pan + zoom over an unbounded surface ---
// The world (tiles) is rendered under `transform: translate(panX,panY)
// scale(zoom)` with transform-origin 0 0, so panX/panY are screen-px offsets of
// the world origin and `zoom` scales everything. Tiles keep their world coords
// in `tileGeom`; only this transform moves the camera.
export interface CanvasView { panX: number; panY: number; zoom: number; }
export const canvasView = writable<CanvasView>({ panX: 0, panY: 0, zoom: 1 });
export const MIN_ZOOM = 0.2;
export const MAX_ZOOM = 2.5;

export function panBy(dx: number, dy: number) {
  canvasView.update((v) => ({ ...v, panX: v.panX + dx, panY: v.panY + dy }));
}
// Zoom by `factor`, keeping the screen point (cx,cy) — relative to the viewport
// top-left — fixed under the cursor.
export function zoomBy(factor: number, cx: number, cy: number) {
  canvasView.update((v) => {
    const zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, v.zoom * factor));
    const k = zoom / v.zoom; // actual ratio after clamping
    return { zoom, panX: cx - (cx - v.panX) * k, panY: cy - (cy - v.panY) * k };
  });
}
export function resetView() {
  canvasView.set({ panX: 0, panY: 0, zoom: 1 });
}

export function openTile(id: number) {
  openTiles.update((t) => (t.includes(id) ? t : [...t, id]));
  ensureGeom(id);
  bringToFront(id);
}
export function closeTile(id: number) {
  openTiles.update((t) => t.filter((x) => x !== id));
  dropGeom(new Set([id]));
}

// A dead session (killed / shell exited / became a ghost) must not linger as a
// tile: drop every id in `deadIds` from the grid. Used both by Terminal on a
// hub://closed|error for its own id, and by the reconcile poll to prune tiles
// whose session is no longer alive (ghost/gone).
export function closeTiles(deadIds: Iterable<number>) {
  const dead = new Set(deadIds);
  if (dead.size === 0) return;
  openTiles.update((t) => t.filter((x) => !dead.has(x)));
  dropGeom(dead);
}
