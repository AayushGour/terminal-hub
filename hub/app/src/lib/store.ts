// Task 7: shared session-list state. SessionList.svelte writes into these on
// refresh(); App.svelte reads openTiles to know which Terminal tiles to mount.
import { writable } from "svelte/store";
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

// --- free-form tile geometry (drag to move / resize) ---
// Each open tile is a floating window with a position + size + stacking order.
// Kept in-memory (resets on reload); a tile gets a cascaded default on open.
export interface TileGeom { x: number; y: number; w: number; h: number; z: number; }
export const tileGeom = writable<Record<number, TileGeom>>({});
let topZ = 0;
const CASCADE = 30;
export const DEFAULT_W = 520;
export const DEFAULT_H = 340;
export function bringToFront(id: number) {
  topZ += 1;
  const z = topZ;
  tileGeom.update((g) => (g[id] ? { ...g, [id]: { ...g[id], z } } : g));
}
export function setGeom(id: number, patch: Partial<TileGeom>) {
  tileGeom.update((g) => (g[id] ? { ...g, [id]: { ...g[id], ...patch } } : g));
}
function ensureGeom(id: number) {
  tileGeom.update((g) => {
    if (g[id]) return g;
    const n = Object.keys(g).length;
    topZ += 1;
    return {
      ...g,
      [id]: { x: 24 + (n % 6) * CASCADE, y: 24 + (n % 6) * CASCADE, w: DEFAULT_W, h: DEFAULT_H, z: topZ },
    };
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
