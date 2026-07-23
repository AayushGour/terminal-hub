<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    listen,
    type UnlistenFn,
    hubDoInstall,
    getSetupDeclined,
    setSetupDeclined,
  } from "./lib/api";
  import Toolbar from "./lib/Toolbar.svelte";
  import SessionList from "./lib/SessionList.svelte";
  import Grid from "./lib/Grid.svelte";
  import Settings from "./lib/Settings.svelte";
  import ConfirmDialog from "./lib/ConfirmDialog.svelte";
  import { installed, refreshInstalled } from "./lib/store";

  let sessionList: SessionList;

  // If hub becomes installed via EITHER the popup or Settings, dismiss the
  // popup -- one shared source of truth so the two can't disagree (issue #6).
  $effect(() => {
    if ($installed === true) showConsent = false;
  });

  // First-run consent (app-lifecycle). On startup, if hub isn't installed and
  // the user hasn't previously declined, show a one-time modal. "Enable" runs
  // the install (rc snippet + daemon + binaries) via the tested `hub` CLI;
  // "Not now" persists a declined flag so we don't nag on every launch.
  let showConsent = $state(false);
  let consentBusy = $state(false);
  let consentError = $state("");
  let installedOk = $state(false);

  async function maybePromptConsent() {
    try {
      if (await refreshInstalled()) return; // shared state; already set up
      const declined = await getSetupDeclined();
      if (declined) return;
      showConsent = true;
    } catch {
      // If the check itself fails, don't block the app with a modal.
    }
  }

  async function enableHub() {
    consentBusy = true;
    consentError = "";
    try {
      await hubDoInstall();
      await refreshInstalled(); // flips shared state -> $effect closes the popup
      installedOk = true;
      setTimeout(() => (installedOk = false), 2500);
      try { await sessionList?.refresh(); } catch {}
    } catch (e) {
      consentError = String(e);
    } finally {
      consentBusy = false;
    }
  }

  async function declineHub() {
    showConsent = false;
    try { await setSetupDeclined(true); } catch {}
  }
  // Task 10: current scrollback preference, loaded by Settings on mount and
  // updated on Save; threaded down into Grid -> Terminal so only *newly*
  // opened tiles pick up a changed value (see Settings.svelte's hint).
  let scrollback = $state(10000);

  // Task 11: show every live shell on startup with NO manual action --
  // including orphaned Hub relays (live socket, no record) and ghosts
  // (record, dead socket) -- so leftovers can be killed (spec §9/§18.7/§19).
  let unlistenConnected: UnlistenFn | undefined;
  let refreshTimer: number | undefined;

  onMount(async () => {
    // First-run consent check (non-blocking wrt the reconcile below).
    maybePromptConsent();
    // Reconcile immediately (may report "not connected" if the daemon/socket
    // isn't reachable yet)...
    try { await sessionList?.refresh(); } catch {}
    // ...again the moment the backend confirms a completed daemon connection
    // (fires under the mock; the real backend's per-tile connection manager
    // has no single app-wide connect to complete, so this is a no-op there
    // today, kept for forward-compat with the event contract)...
    unlistenConnected = await listen("hub://connected", () => sessionList?.refresh());
    // ...and periodically thereafter: `reconcile_sessions` opens its own
    // short-lived connection per call (cheap, idempotent), and polling is the
    // only way to notice a daemon that starts AFTER the app, or orphans/
    // ghosts that appear later, so the list stays current without clicks.
    refreshTimer = window.setInterval(() => sessionList?.refresh(), 5000);
  });

  onDestroy(() => {
    unlistenConnected?.();
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<main class="layout">
  <div class="side">
    <Toolbar onNew={() => sessionList?.refresh()} />
    <SessionList bind:this={sessionList} />
    <Settings onChange={(n) => (scrollback = n)} />
  </div>
  <Grid {scrollback} />
</main>

<!-- Reliable in-app confirm (window.confirm no-ops in this webview). -->
<ConfirmDialog />

{#if installedOk}
  <div class="toast" role="status">hub enabled ✓ — open a new terminal to start capturing.</div>
{/if}

{#if showConsent}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-labelledby="consent-title">
      <h2 id="consent-title">Set up hub?</h2>
      <p>
        hub captures the terminals you open so you can manage them here. This adds a
        guarded, reversible line to <code>~/.zshrc</code> and starts the hub background
        service. You can undo it anytime from Settings → Uninstall.
      </p>
      {#if consentError}
        <p class="err">Setup failed: {consentError}</p>
      {/if}
      <div class="actions">
        <button class="ghost" onclick={declineHub} disabled={consentBusy}>Not now</button>
        <button class="primary" onclick={enableHub} disabled={consentBusy}>
          {consentBusy ? "Enabling…" : "Enable"}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  :global(body) { margin: 0; background: #111; color: #eee; font-family: system-ui, sans-serif; }
  .layout { display: flex; height: 100vh; }
  .side { display: flex; flex-direction: column; width: 280px; border-right: 1px solid #333; overflow-y: auto; }
  main :global(.panel) { width: 100%; border-right: 0; flex: 1; }
  main :global(.grid) { flex: 1; }

  .modal-backdrop {
    position: fixed; inset: 0; background: rgba(0, 0, 0, 0.6);
    display: flex; align-items: center; justify-content: center; z-index: 1000;
  }
  .modal {
    background: #1b1b1b; border: 1px solid #444; border-radius: 8px;
    padding: 20px 22px; max-width: 460px; box-shadow: 0 8px 30px rgba(0, 0, 0, 0.5);
  }
  .modal h2 { margin: 0 0 10px; font-size: 18px; }
  .modal p { font-size: 13px; line-height: 1.5; opacity: 0.9; }
  .modal code { background: #000; padding: 1px 4px; border-radius: 3px; }
  .modal .err { color: #f77; }
  .actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 16px; }
  .actions button { padding: 7px 14px; border: 0; border-radius: 4px; cursor: pointer; font-weight: 600; }
  .actions button:disabled { opacity: 0.6; cursor: default; }
  .actions .ghost { background: #333; color: #eee; }
  .actions .primary { background: #2d6; color: #012; }
  .toast {
    position: fixed; bottom: 18px; left: 50%; transform: translateX(-50%);
    background: #113; color: #cfe; border: 1px solid #2d6; border-radius: 6px;
    padding: 8px 14px; font-size: 13px; z-index: 1001;
  }
</style>
