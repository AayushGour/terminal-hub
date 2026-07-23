<script lang="ts">
  // Singleton confirm dialog. Mounted once (App.svelte). Renders whatever is
  // pushed onto `pendingConfirm` and resolves the awaiting promise on choice.
  // Replaces window.confirm(), which no-ops in this webview (see dialog.ts).
  // When the pending item carries an async `action`, the confirm button shows a
  // spinner and the dialog stays open until it finishes.
  import { pendingConfirm, acceptConfirm, cancelConfirm } from "./dialog";
  import Spinner from "./Spinner.svelte";

  function onKey(e: KeyboardEvent) {
    if (!$pendingConfirm) return;
    if (e.key === "Escape") cancelConfirm();
    if (e.key === "Enter") acceptConfirm();
  }
</script>

<svelte:window on:keydown={onKey} />

{#if $pendingConfirm}
  <div class="backdrop" onclick={cancelConfirm} role="presentation">
    <div
      class="box"
      role="alertdialog"
      aria-modal="true"
      aria-label={$pendingConfirm.title}
      onclick={(e) => e.stopPropagation()}
    >
      <h3>{$pendingConfirm.title}</h3>
      <p>{$pendingConfirm.body}</p>
      <div class="actions">
        <button class="ghost" onclick={cancelConfirm} disabled={$pendingConfirm.busy}>Cancel</button>
        <button
          class:danger={$pendingConfirm.danger}
          class:primary={!$pendingConfirm.danger}
          onclick={acceptConfirm}
          disabled={$pendingConfirm.busy}
        >
          {#if $pendingConfirm.busy}<Spinner size={13} /> Working…{:else}{$pendingConfirm.confirmLabel}{/if}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }
  .box {
    background: #1b1b1b;
    border: 1px solid #444;
    border-radius: 8px;
    padding: 18px 20px;
    max-width: 420px;
    box-shadow: 0 8px 30px rgba(0, 0, 0, 0.5);
  }
  h3 { margin: 0 0 8px; font-size: 16px; }
  p { margin: 0; font-size: 13px; line-height: 1.5; opacity: 0.9; }
  .actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 16px; }
  .actions button {
    padding: 7px 14px;
    border: 0;
    border-radius: 4px;
    cursor: pointer;
    font-weight: 600;
    font-size: 13px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .actions button:disabled { opacity: 0.7; cursor: default; }
  .ghost { background: #333; color: #eee; }
  .primary { background: #2d6; color: #012; }
  .danger { background: #a33; color: #fff; }
</style>
