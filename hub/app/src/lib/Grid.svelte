<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import TileFrame from "./TileFrame.svelte";
  import {
    openTiles, healthy, orphan, tileGeom,
    canvasView, panBy, zoomBy, resetView, MIN_ZOOM, MAX_ZOOM, viewportSize,
  } from "./store";
  let { scrollback = 10000 }: { scrollback?: number } = $props();

  let viewportEl: HTMLDivElement;

  // Keep `viewportSize` (screen px) in sync so store.ts's ensureGeom() can
  // anchor a newly-opened tile's spawn position to whatever part of the
  // canvas is currently visible. Mirrors the ResizeObserver pattern
  // Terminal.svelte uses for pty sizing.
  onMount(() => {
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) viewportSize.set({ w: r.width, h: r.height });
    });
    ro.observe(viewportEl);
    return () => ro.disconnect();
  });

  // id -> origin, for the window title bar label + dot color.
  let originOf = $derived.by(() => {
    const m: Record<number, string> = {};
    for (const s of [...$healthy, ...$orphan]) m[s.id] = s.origin;
    return m;
  });

  // id -> cwd, for the titlebar's truncated-path span (spec:
  // 2026-07-23-shell-integration-design.md §6). Same shape/pattern as
  // `originOf` above -- SessionList.svelte's `refresh()` already sets
  // `healthy`/`orphan` from `reconcile()`, so this map only changes when a
  // session's own `cwd` actually does, and TileFrame's `$derived` (keyed by
  // `{#each $openTiles as id (id)}`, unchanged below) only re-patches that
  // one tile's text -- no whole-grid re-render.
  let cwdOf = $derived.by(() => {
    const m: Record<number, string> = {};
    for (const s of [...$healthy, ...$orphan]) m[s.id] = s.cwd;
    return m;
  });

  // --- pan: drag empty canvas background ---
  // $state so `class:panning` (grab vs grabbing cursor) actually re-renders.
  let panning = $state(false);
  let lastX = 0, lastY = 0;
  function onPointerDown(e: PointerEvent) {
    // Only the empty background pans -- tiles are children with their own
    // pointer handling, so a pointerdown on a tile has a different target.
    if (e.target !== viewportEl) return;
    panning = true;
    lastX = e.clientX;
    lastY = e.clientY;
    viewportEl.setPointerCapture(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    if (!panning) return;
    panBy(e.clientX - lastX, e.clientY - lastY);
    lastX = e.clientX;
    lastY = e.clientY;
  }
  function onPointerUp(e: PointerEvent) {
    panning = false;
    try { viewportEl.releasePointerCapture(e.pointerId); } catch {}
  }

  // --- wheel: ⌘/Ctrl+wheel zooms toward the cursor; plain wheel over empty
  // canvas pans (over a terminal, plain wheel scrolls that terminal instead). ---
  function onWheel(e: WheelEvent) {
    const rect = viewportEl.getBoundingClientRect();
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const factor = Math.exp(-e.deltaY * 0.0015); // smooth, exponential
      zoomBy(factor, e.clientX - rect.left, e.clientY - rect.top);
      return;
    }
    const overTile = (e.target as HTMLElement).closest?.(".tilewrap");
    if (overTile) return; // let xterm handle scrollback
    e.preventDefault();
    panBy(-e.deltaX, -e.deltaY);
  }

  // --- zoom buttons zoom toward the viewport center ---
  function zoomButton(factor: number) {
    const r = viewportEl.getBoundingClientRect();
    zoomBy(factor, r.width / 2, r.height / 2);
  }

  // --- fit: frame all open tiles in view ---
  function fit() {
    const geoms = Object.values(get(tileGeom));
    if (geoms.length === 0) { resetView(); return; }
    const minX = Math.min(...geoms.map((g) => g.x));
    const minY = Math.min(...geoms.map((g) => g.y));
    const maxX = Math.max(...geoms.map((g) => g.x + g.w));
    const maxY = Math.max(...geoms.map((g) => g.y + g.h));
    const pad = 60;
    const vw = viewportEl.clientWidth, vh = viewportEl.clientHeight;
    const zoom = Math.min(
      MAX_ZOOM,
      Math.max(MIN_ZOOM, Math.min(vw / (maxX - minX + pad * 2), vh / (maxY - minY + pad * 2))),
    );
    const panX = vw / 2 - ((minX + maxX) / 2) * zoom;
    const panY = vh / 2 - ((minY + maxY) / 2) * zoom;
    canvasView.set({ panX, panY, zoom });
  }

  let zoomPct = $derived(Math.round($canvasView.zoom * 100));
</script>

<div
  class="grid"
  class:panning
  bind:this={viewportEl}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onwheel={onWheel}
  style="background-position: {$canvasView.panX}px {$canvasView.panY}px; background-size: {22 * $canvasView.zoom}px {22 * $canvasView.zoom}px;"
  role="application"
  aria-label="terminal canvas"
>
  <div
    class="world"
    style="transform: translate({$canvasView.panX}px, {$canvasView.panY}px) scale({$canvasView.zoom});"
  >
    {#each $openTiles as id (id)}
      <TileFrame {id} {scrollback} origin={originOf[id] ?? ""} cwd={cwdOf[id] ?? ""} />
    {/each}
  </div>

  {#if $openTiles.length === 0}
    <div class="hint">
      Open a session from the list to bring up a terminal.<br />
      Drag a title bar to move it, drag the corner to resize.<br />
      Drag empty space to pan · ⌘/Ctrl-scroll to zoom.
    </div>
  {/if}

  <!-- Zoom controls (not transformed) -->
  <div class="controls">
    <button onclick={() => zoomButton(1 / 1.2)} title="Zoom out" aria-label="zoom out">−</button>
    <button class="pct" onclick={resetView} title="Reset to 100%">{zoomPct}%</button>
    <button onclick={() => zoomButton(1.2)} title="Zoom in" aria-label="zoom in">+</button>
    <button class="fit" onclick={fit} title="Fit all terminals">Fit</button>
  </div>
</div>

<style>
  .grid {
    position: relative;
    flex: 1;
    height: 100vh;
    overflow: hidden;
    isolation: isolate; /* keep per-tile z-index below the sidebar/modals */
    background-color: #0d0d0d;
    background-image: radial-gradient(circle at 1px 1px, #1c1c1c 1px, transparent 0);
    cursor: grab;
    touch-action: none;
  }
  .grid.panning { cursor: grabbing; }
  .world {
    position: absolute;
    top: 0;
    left: 0;
    width: 0;
    height: 0;
    transform-origin: 0 0;
    will-change: transform;
  }
  .hint {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    max-width: 380px;
    text-align: center;
    color: #666;
    font-size: 13px;
    line-height: 1.6;
    pointer-events: none;
  }
  .controls {
    position: absolute;
    right: 12px;
    bottom: 12px;
    display: flex;
    gap: 4px;
    padding: 4px;
    background: rgba(20, 20, 20, 0.85);
    border: 1px solid #333;
    border-radius: 8px;
    z-index: 50;
    backdrop-filter: blur(4px);
  }
  .controls button {
    min-width: 30px;
    height: 26px;
    border: 0;
    border-radius: 5px;
    background: #2a2a2a;
    color: #ddd;
    font-size: 14px;
    cursor: pointer;
    padding: 0 8px;
  }
  .controls button:hover { background: #383838; }
  .controls .pct { min-width: 48px; font-size: 12px; font-variant-numeric: tabular-nums; }
  .controls .fit { font-size: 12px; }
</style>
