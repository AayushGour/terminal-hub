<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
  import { listen, attach, detach, sendInput, claimSize, type UnlistenFn } from "./api";
  import { closeTile } from "./store";
  import Spinner from "./Spinner.svelte";

  let { sessionId, scrollback = 10000 }: { sessionId: number; scrollback?: number } = $props();

  let el: HTMLDivElement;
  let term: Terminal;
  let fit: FitAddon;
  let unlisten: UnlistenFn[] = [];
  let resizeTimer: number | undefined;
  let lastCols = 0;
  let lastRows = 0;
  let ro: ResizeObserver;
  // FIX 1: flips to false the moment this session's connection ends
  // (hub://closed) or errors (hub://error) -- a killed/exited session must
  // not keep being treated as an active tile (no more input, no more
  // claim_size calls racing a connection that's already gone).
  let live = $state(true);
  // issue #4: show a "connecting…" loader until the first frame (replay or
  // output) actually arrives, so an attaching tile isn't just a blank black box.
  let connecting = $state(true);
  let attachError = $state("");

  function claimNow() {
    if (!live) return;
    fit.fit();
    const dims = fit.proposeDimensions();
    if (!dims) return;
    // "only resize if dims changed" (spec §7) to avoid reflow thrash.
    if (dims.cols === lastCols && dims.rows === lastRows) return;
    lastCols = dims.cols;
    lastRows = dims.rows;
    claimSize(sessionId, dims.cols, dims.rows);
  }
  function claimSoon() {
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(claimNow, 50); // ~50ms debounce (spec §7)
  }

  onMount(async () => {
    term = new Terminal({ scrollback, convertEol: false, cursorBlink: true, fontSize: 13 });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();

    // Keystrokes -> daemon as a Data frame (INPUT). (Contract: input is Frame::Data.)
    term.onData((data: string) => {
      if (!live) return; // session already closed/errored; nothing to send to.
      sendInput(sessionId, Array.from(new TextEncoder().encode(data)));
    });

    // Focus-follows-size: gaining focus on this tile claims pty sizing.
    term.textarea?.addEventListener("focus", claimNow);

    // Register REPLAY + OUTPUT listeners BEFORE attach so no frame is missed.
    unlisten.push(
      await listen<{ id: number; bytes: number[] }>("hub://replay", (e) => {
        if (e.payload.id !== sessionId) return;
        connecting = false; // first frame in -> stream is live
        term.write(new Uint8Array(e.payload.bytes)); // REPLAY snapshot first
      }),
    );
    unlisten.push(
      await listen<{ id: number; bytes: number[] }>("hub://output", (e) => {
        if (e.payload.id !== sessionId) return;
        connecting = false;
        term.write(new Uint8Array(e.payload.bytes)); // then live stream, in order
      }),
    );

    // A session that ends (killed / shell exited -> hub://closed) or errors
    // (hub://error) must not leave a dead tile hanging around: remove it from
    // the grid immediately. closeTile() drops it from openTiles, which unmounts
    // this component -> onDestroy detaches cleanly. (The list row updates on the
    // next reconcile.)
    unlisten.push(
      await listen<{ id: number; exitCode?: number | null }>("hub://closed", (e) => {
        if (e.payload.id !== sessionId) return;
        live = false;
        connecting = false;
        closeTile(sessionId);
      }),
    );
    unlisten.push(
      await listen<{ id: number; message: string }>("hub://error", (e) => {
        if (e.payload.id !== sessionId) return;
        live = false;
        connecting = false;
        closeTile(sessionId);
      }),
    );

    // Drag-resize of the wrapping tile drives claim_size (debounced).
    ro = new ResizeObserver(() => claimSoon());
    ro.observe(el);

    // Attach LAST — this triggers the relay's Replay + SIGWINCH nudge + live stream.
    try {
      await attach(sessionId);
    } catch (e) {
      connecting = false;
      attachError = String(e);
      live = false;
    }
  });

  onDestroy(async () => {
    if (resizeTimer) clearTimeout(resizeTimer);
    term.textarea?.removeEventListener("focus", claimNow);
    ro?.disconnect();
    unlisten.forEach((u) => u());
    // Detach = stop viewing; the session keeps running (distinct from kill).
    try { await detach(sessionId); } catch {}
    term?.dispose();
  });
</script>

<div class="tileroot">
  <div class="tile" bind:this={el} tabindex="0"></div>
  {#if connecting}
    <div class="overlay"><Spinner size={20} /><span>connecting…</span></div>
  {:else if attachError}
    <div class="overlay err"><span>couldn't attach</span><small>{attachError}</small></div>
  {/if}
</div>

<style>
  .tileroot { position: relative; width: 100%; height: 100%; background: #000; }
  .tile { width: 100%; height: 100%; background: #000; }
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    background: rgba(0, 0, 0, 0.55);
    color: #aaa;
    font-size: 12px;
    pointer-events: none;
  }
  .overlay.err { color: #f88; }
  .overlay small { opacity: 0.7; max-width: 90%; text-align: center; word-break: break-word; }
</style>
