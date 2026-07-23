<script lang="ts">
  // A floating terminal window: drag the title bar to move, drag the
  // bottom-right handle to resize, click anywhere to bring to front. Geometry
  // lives in the shared `tileGeom` store so it survives re-renders and the
  // pointer math stays here. Resizing the body drives Terminal's ResizeObserver
  // -> claim_size, so the pty follows the window size as before.
  import Terminal from "./Terminal.svelte";
  import { tileGeom, setGeom, bringToFront, closeTile, canvasView } from "./store";

  let { id, scrollback = 10000, origin = "" }: { id: number; scrollback?: number; origin?: string } = $props();

  const MIN_W = 240;
  const MIN_H = 150;

  let g = $derived($tileGeom[id]);
  // The world is scaled by zoom, so a screen-px pointer delta is `delta/zoom`
  // world px -- otherwise dragging/resizing drifts away from the cursor when
  // zoomed in or out.
  let zoom = $derived($canvasView.zoom);

  let mode: "move" | "resize" | null = null;
  let px = 0, py = 0; // pointer origin
  let ox = 0, oy = 0, ow = 0, oh = 0; // geom origin

  function begin(kind: "move" | "resize", e: PointerEvent) {
    if (e.button !== 0 || !g) return;
    mode = kind;
    px = e.clientX; py = e.clientY;
    ox = g.x; oy = g.y; ow = g.w; oh = g.h;
    bringToFront(id);
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
    e.stopPropagation();
  }
  function move(e: PointerEvent) {
    if (!mode) return;
    const dx = (e.clientX - px) / zoom, dy = (e.clientY - py) / zoom;
    if (mode === "move") {
      // No lower clamp: the canvas is unbounded, tiles can live anywhere.
      setGeom(id, { x: ox + dx, y: oy + dy });
    } else {
      setGeom(id, { w: Math.max(MIN_W, ow + dx), h: Math.max(MIN_H, oh + dy) });
    }
  }
  function end(e: PointerEvent) {
    mode = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch {}
  }
</script>

{#if g}
  <div
    class="tilewrap"
    style="left:{g.x}px; top:{g.y}px; width:{g.w}px; height:{g.h}px; z-index:{g.z};"
    onpointerdown={() => bringToFront(id)}
    role="group"
    aria-label="terminal {id}"
  >
    <div
      class="titlebar"
      onpointerdown={(e) => begin("move", e)}
      onpointermove={move}
      onpointerup={end}
      role="toolbar"
      tabindex="-1"
    >
      <span class="dot {origin === 'Hub' ? 'hub' : 'ext'}"></span>
      <span class="tt">{origin || "session"} #{id}</span>
      <button class="x" title="Detach (close view)" onpointerdown={(e) => e.stopPropagation()} onclick={() => closeTile(id)}>✕</button>
    </div>

    <div class="body"><Terminal sessionId={id} {scrollback} /></div>

    <div
      class="resize"
      onpointerdown={(e) => begin("resize", e)}
      onpointermove={move}
      onpointerup={end}
      role="separator"
      aria-label="resize"
      aria-orientation="horizontal"
    ></div>
  </div>
{/if}

<style>
  .tilewrap {
    position: absolute;
    display: flex;
    flex-direction: column;
    border: 1px solid #333;
    border-radius: 6px;
    background: #000;
    box-shadow: 0 6px 22px rgba(0, 0, 0, 0.45);
    overflow: hidden;
  }
  .tilewrap:focus-within { border-color: #2d6; }
  .titlebar {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 6px;
    background: #1a1a1a;
    border-bottom: 1px solid #2a2a2a;
    cursor: move;
    user-select: none;
    flex: none;
  }
  .dot { width: 8px; height: 8px; border-radius: 50%; flex: none; }
  .dot.hub { background: #2d6; }
  .dot.ext { background: #58f; }
  .tt { flex: 1; font-size: 12px; color: #ccc; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .x {
    flex: none; width: 20px; height: 18px; border: 0; border-radius: 3px;
    background: transparent; color: #aaa; cursor: pointer; font-size: 12px; line-height: 1;
  }
  .x:hover { background: #a33; color: #fff; }
  .body { flex: 1; min-height: 0; overflow: hidden; }
  .resize {
    position: absolute;
    right: 0; bottom: 0;
    width: 16px; height: 16px;
    cursor: nwse-resize;
    /* subtle corner grip */
    background:
      linear-gradient(135deg, transparent 0 50%, #555 50% 60%, transparent 60% 70%, #555 70% 80%, transparent 80%);
  }
</style>
