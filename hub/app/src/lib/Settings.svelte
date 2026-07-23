<script lang="ts">
  // Task 10: scrollback buffer-size setting. Loads the persisted preference
  // on mount (`get_buffer_size`), and on Save persists it (`set_buffer_size`)
  // and notifies the parent via `onChange` so `App.svelte` can thread the
  // new value into `Grid` -> `Terminal` for newly opened tiles. Per the
  // interface contract this can only affect xterm's client-side scrollback
  // for tiles opened *after* the change -- a running relay's vt buffer is a
  // fixed-size ring set at spawn and can't be resized retroactively -- so the
  // hint text below spells that caveat out alongside the RAM tradeoff.
  import { onMount } from "svelte";
  import {
    getBufferSize,
    setBufferSize,
    hubDoInstall,
    hubDoUninstall,
  } from "./api";
  // Shared install state (issue #6): the SAME store the first-run popup reads,
  // so installing from either place keeps both in sync. Fail-safe default is
  // `null`/`false` (not installed) -- we never show the self-deleting Uninstall
  // button unless the check positively resolved to installed.
  import { installed, refreshInstalled } from "./store";
  import { confirmAction } from "./dialog";
  import Spinner from "./Spinner.svelte";

  let { onChange }: { onChange: (n: number) => void } = $props();
  let size = $state(10000);
  let saved = $state(false);
  let busy = $state(false);
  let lifecycleError = $state("");

  onMount(async () => {
    size = await getBufferSize();
    onChange(size);
    await refreshInstalled();
  });

  async function save() {
    await setBufferSize(size);
    onChange(size);
    saved = true;
    setTimeout(() => (saved = false), 1500);
  }

  async function installHub() {
    busy = true;
    lifecycleError = "";
    try {
      await hubDoInstall();
      await refreshInstalled();
    } catch (e) {
      lifecycleError = String(e);
    } finally {
      busy = false;
    }
  }

  async function uninstallHub() {
    // Work runs inside the confirm dialog so its button shows a spinner while
    // the (slow) revert + self-delete happens.
    await confirmAction(
      "This reverts the ~/.zshrc change, stops the service, deletes ~/.hub, and moves hub.app to the Trash.",
      {
        title: "Uninstall hub",
        confirmLabel: "Uninstall & remove",
        danger: true,
        action: async () => {
          lifecycleError = "";
          try {
            // On the real backend this quits and self-deletes the app, so
            // control may not return here; under the mock it just flips state.
            await hubDoUninstall();
            await refreshInstalled();
          } catch (e) {
            lifecycleError = String(e);
          }
        },
      },
    );
  }
</script>

<div class="settings">
  <label>
    Scrollback buffer (lines)
    <input type="number" min="1000" step="1000" bind:value={size} />
  </label>
  <button onclick={save}>Save</button>
  {#if saved}<span class="ok">saved ✓</span>{/if}
  <p class="hint">
    Memory ≈ <strong>buffer × line width × live session count</strong>. The 10k default
    is a few MB per open terminal; raising it multiplies across every open tile, so a large
    buffer with many tiles open at once can use hundreds of MB. New value applies to newly
    opened tiles only — it can't resize the scrollback of tiles already open, or a running
    relay's fixed-size buffer.
  </p>

  <div class="lifecycle">
    <h5>hub capture</h5>
    {#if $installed === true}
      <button class="danger" onclick={uninstallHub} disabled={busy}>
        {#if busy}<Spinner size={13} /> Uninstalling…{:else}Uninstall hub & remove app{/if}
      </button>
      <p class="hint">
        Reverts the guarded <code>~/.zshrc</code> line, stops the background service,
        deletes <code>~/.hub</code>, and moves this app to the Trash.
      </p>
    {:else}
      <button class="enable" onclick={installHub} disabled={busy}>
        {#if busy}<Spinner size={13} /> Enabling…{:else}Install / Enable capture{/if}
      </button>
      <p class="hint">
        hub isn't set up. Enabling adds a guarded, reversible line to
        <code>~/.zshrc</code> and starts the hub background service.
      </p>
    {/if}
    {#if lifecycleError}<p class="err">{lifecycleError}</p>{/if}
  </div>
</div>

<style>
  .settings { padding: 8px; border-top: 1px solid #333; font-size: 13px; }
  .settings input { width: 90px; margin-left: 6px; }
  .hint { opacity: 0.7; font-size: 11px; line-height: 1.4; }
  .ok { color: #2d6; margin-left: 8px; }
  .lifecycle { margin-top: 12px; padding-top: 10px; border-top: 1px solid #333; }
  .lifecycle h5 { margin: 0 0 6px; font-size: 12px; text-transform: uppercase; opacity: 0.6; letter-spacing: 0.04em; }
  .lifecycle button { border: 0; border-radius: 4px; padding: 6px 10px; cursor: pointer; font-weight: 600; }
  .lifecycle button:disabled { opacity: 0.6; cursor: default; }
  .lifecycle .danger { background: #a33; color: #fff; }
  .lifecycle .enable { background: #2d6; color: #012; }
  .lifecycle code { background: #000; padding: 1px 3px; border-radius: 3px; }
  .err { color: #f77; font-size: 11px; }
</style>
