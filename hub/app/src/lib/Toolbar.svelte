<script lang="ts">
  // Task 9: "New session" button. `spawn_session` launches a detached
  // Hub-origin `hub-relay` process (see commands.rs::spawn_session) which
  // registers with the daemon asynchronously -- there is no session id to
  // return synchronously, so we just fire the spawn and ask the caller
  // (App.svelte -> SessionList.refresh) to re-poll shortly after so the new
  // Healthy/Hub-origin session has time to show up in the registry.
  import { spawnSession } from "./api";
  let { onNew }: { onNew: () => void } = $props();

  async function newSession() {
    await spawnSession(); // launches a detached Hub-origin relay via the `hub` CLI
    // The relay registers asynchronously; poll the list shortly after.
    setTimeout(onNew, 400);
  }
</script>

<div class="toolbar">
  <button class="primary" onclick={newSession}>+ New session</button>
</div>

<style>
  .toolbar { padding: 8px; border-bottom: 1px solid #333; }
  .primary { background: #2d6; color: #012; font-weight: 600; padding: 6px 12px; border: 0; border-radius: 4px; cursor: pointer; }
</style>
