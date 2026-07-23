<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { reconcile, kill } from "./api";
  import {
    healthy, ghost, orphan,
    openTiles, openTile, closeTile, closeTiles,
    listBusy, busyIds, setBusy,
  } from "./store";
  import { confirmAction } from "./dialog";
  import Spinner from "./Spinner.svelte";

  export async function refresh() {
    listBusy.set(true);
    try {
      const b = await reconcile();
      healthy.set(b.healthy);
      ghost.set(b.ghost);
      orphan.set(b.orphan);
      // Prune any open tile whose session is no longer alive: a session that
      // dropped to a ghost (dead socket) or vanished entirely must not leave a
      // tile hanging. Still-live sessions = healthy ∪ orphan (orphan = live, no
      // record -- still viewable), so close every open id outside that set.
      const alive = new Set<number>([...b.healthy, ...b.orphan].map((s) => s.id));
      closeTiles(get(openTiles).filter((id) => !alive.has(id)));
    } finally {
      listBusy.set(false);
    }
  }

  // Open/close a tile = attach/detach. ONE consistent toggle (issue #2): the
  // row's button and clicking the name do the same thing, and the row shows
  // whether it's currently attached. Detach just removes the tile; Terminal's
  // onDestroy detaches at the daemon (the session keeps running).
  function toggle(id: number) {
    if ($openTiles.includes(id)) closeTile(id);
    else openTile(id);
  }

  // Kill = terminate the shell for all viewers. Uses the in-app confirm
  // (window.confirm is a no-op in this webview -- see dialog.ts), shows a
  // per-row spinner while in flight, then closes any tile + refreshes.
  // Poll reconcile until session `id` is no longer LIVE (left healthy ∪ orphan),
  // so the row's Kill spinner stays up for the whole real kill -- `kill()` only
  // sends the frame and returns immediately; the shell dies a beat later on the
  // backend. Bounded (~5s) so a stuck kill can't spin forever.
  async function pollUntilDead(id: number, tries = 25, ms = 200) {
    for (let i = 0; i < tries; i++) {
      const b = await reconcile();
      healthy.set(b.healthy);
      ghost.set(b.ghost);
      orphan.set(b.orphan);
      const alive = new Set<number>([...b.healthy, ...b.orphan].map((s) => s.id));
      closeTiles(get(openTiles).filter((x) => !alive.has(x)));
      // Keep the spinner up until the id is gone from EVERY bucket -- a live
      // kill (healthy/orphan) that briefly leaves a ghost record, and a ghost
      // "Clean up" (already not-alive), both only finish once the record is
      // gone too.
      const present =
        b.healthy.some((s) => s.id === id) ||
        b.orphan.some((s) => s.id === id) ||
        b.ghost.some((g) => g.id === id);
      if (!present) return;
      await new Promise((r) => setTimeout(r, ms));
    }
  }

  async function onKill(id: number) {
    // Confirm, then CLOSE the dialog immediately (non-blocking) -- the progress
    // shows as an in-button spinner on the sidebar Kill button, which stays up
    // until the session is actually dead (pollUntilDead), not just until the
    // kill frame is sent.
    const ok = await confirmAction(
      `End session #${id}? This kills the shell for every viewer and can't be undone.`,
      { title: "Kill session", confirmLabel: "Kill", danger: true },
    );
    if (!ok) return;
    setBusy(id, true);
    try {
      await kill(id);
      await pollUntilDead(id);
    } catch (e) {
      console.error("kill failed", e);
    } finally {
      setBusy(id, false);
    }
  }

  onMount(refresh);
</script>

<aside class="panel">
  <header>
    <strong>Sessions</strong>
    <button class="refresh" onclick={refresh} disabled={$listBusy} title="Refresh">
      {#if $listBusy}<Spinner size={12} />{:else}↻{/if}
    </button>
  </header>

  <section>
    <h4>Healthy</h4>
    {#if $healthy.length === 0}<p class="empty">No live sessions.</p>{/if}
    {#each $healthy as s (s.id)}
      {@const isOpen = $openTiles.includes(s.id)}
      {@const busy = $busyIds.has(s.id)}
      <div class="row" class:attached={isOpen}>
        <span class="badge {s.origin === 'Hub' ? 'hub' : 'ext'}">{s.origin}</span>
        <button class="title" onclick={() => toggle(s.id)} title="{isOpen ? 'Detach' : 'Open'} {s.title} #{s.id}">
          {s.title} #{s.id}
        </button>
        <button class="act" onclick={() => toggle(s.id)} disabled={busy}>
          {isOpen ? "Detach" : "Open"}
        </button>
        <button class="act kill" onclick={() => onKill(s.id)} disabled={busy} title="Kill session">
          {#if busy}<Spinner size={12} />{:else}Kill{/if}
        </button>
      </div>
    {/each}
  </section>

  <section>
    <h4>Orphan <small>(live, no record)</small></h4>
    {#if $orphan.length === 0}<p class="empty">None.</p>{/if}
    {#each $orphan as s (s.id)}
      {@const isOpen = $openTiles.includes(s.id)}
      {@const busy = $busyIds.has(s.id)}
      <div class="row orphan" class:attached={isOpen}>
        <span class="badge {s.origin === 'Hub' ? 'hub' : 'ext'}">{s.origin}</span>
        <button class="title" onclick={() => toggle(s.id)}>{s.title} #{s.id}</button>
        <button class="act kill" onclick={() => onKill(s.id)} disabled={busy}>
          {#if busy}<Spinner size={12} />{:else}Kill{/if}
        </button>
      </div>
    {/each}
  </section>

  <section>
    <h4>Ghost <small>(record, dead socket)</small></h4>
    {#if $ghost.length === 0}<p class="empty">None.</p>{/if}
    {#each $ghost as g (g.id)}
      {@const busy = $busyIds.has(g.id)}
      <div class="row ghost">
        <span class="badge {g.origin === 'Hub' ? 'hub' : 'ext'}">{g.origin}</span>
        <span class="title dim">{g.title} #{g.id}</span>
        <button class="act kill" onclick={() => onKill(g.id)} disabled={busy}>
          {#if busy}<Spinner size={12} />{:else}Clean up{/if}
        </button>
      </div>
    {/each}
  </section>
</aside>

<style>
  .panel { width: 100%; padding: 8px; overflow-y: auto; box-sizing: border-box; }
  header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 4px; }
  .refresh {
    background: #333; color: #eee; border: 0; border-radius: 4px;
    width: 26px; height: 22px; cursor: pointer; display: inline-flex;
    align-items: center; justify-content: center;
  }
  .refresh:disabled { cursor: default; }
  h4 { margin: 10px 0 4px; font-size: 13px; }
  h4 small { font-weight: normal; opacity: 0.6; font-size: 11px; }
  .empty { font-size: 11px; opacity: 0.45; margin: 2px 0 0; }

  /* issue #1: everything shrinks / truncates so nothing overflows a 280px rail. */
  .row {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 4px 4px;
    border-radius: 5px;
    font-size: 13px;
    min-width: 0;
  }
  .row.attached { background: #14301d; box-shadow: inset 2px 0 0 #2d6; }
  .badge { flex: none; font-size: 10px; padding: 1px 5px; border-radius: 3px; }
  .badge.hub { background: #2d6; color: #012; }
  .badge.ext { background: #58f; color: #fff; }

  /* title takes the slack but is allowed to shrink to nothing (min-width:0)
     and ellipsize, so the action buttons always stay on-row. */
  .title {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
    background: none;
    border: 0;
    color: inherit;
    cursor: pointer;
    padding: 0;
    font: inherit;
  }
  .title.dim { cursor: default; opacity: 0.6; }

  .act {
    flex: none;
    background: #444;
    color: #eee;
    border: 0;
    border-radius: 4px;
    padding: 3px 8px;
    font-size: 12px;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 34px;
    min-height: 22px;
  }
  .act:disabled { opacity: 0.55; cursor: default; }
  .act.kill { background: #a33; color: #fff; }
  .row.ghost { opacity: 0.7; }
</style>
