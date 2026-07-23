// Deterministic in-memory IPC used only when VITE_MOCK is set.
// send_input(id, bytes) echoes the same bytes back as a hub://output event,
// so typing into an xterm tile visibly echoes without a real backend.
type Handler = (e: { payload: any }) => void;
const handlers: Record<string, Handler[]> = {};

function emit(event: string, payload: any) {
  (handlers[event] ?? []).forEach((h) => h({ payload }));
}

// Task 9: mutable session table (was a static single-session literal) so
// `spawn_session` has somewhere to add a fake new session, and subsequent
// `list_sessions`/`reconcile_sessions` polls (as driven by
// `Toolbar.svelte`'s post-spawn `SessionList.refresh()`) pick it up -- this
// is what lets the "New session" button flow be exercised end-to-end under
// VITE_MOCK/Playwright without a real daemon.
const sessions: any[] = [{ id: 1, origin: "Hub", title: "mock", pid: 0, started_unix: 0, cols: 80, rows: 24 }];
let nextId = 2;

// FIX 3: was a hardcoded 10000 (get) + no-op (set), leaving the Settings
// Save path (set_buffer_size -> get_buffer_size) completely untested under
// the mock. Now stateful: set records the value, get returns it.
let bufferSize = 10000;

// App-lifecycle mock state. `installed` DEFAULTS TRUE so the first-run consent
// modal does NOT appear on a plain "/" load — every pre-existing Playwright
// spec navigates to "/" and would otherwise be blocked by the overlay. The
// new lifecycle smoke opts into the not-installed state with `?notInstalled=1`.
// hub_do_uninstall here only flips state (it neither self-deletes nor quits —
// there's no real bundle under the mock).
let installed = true;
let declined = false;
if (typeof window !== "undefined") {
  const params = new URLSearchParams(window.location.search);
  if (params.has("notInstalled")) installed = false;
  if (params.has("declined")) declined = true;
}

// Task 11: a leftover Hub-spawned relay (live socket, no record) and a dead
// record (ghost) are present from the very first `reconcile_sessions` call,
// so the startup smoke can assert BOTH that they render under their bucket
// AND that Kill/Clean-up on them actually removes them -- not just that the
// bucket headers exist (which would pass even with empty buckets).
const orphans: any[] = [{ id: 50, origin: "Hub", title: "orphan-relay", pid: 0, started_unix: 0, cols: 80, rows: 24 }];
const ghosts: any[] = [
  { id: 51, origin: "Hub", title: "ghost-relay", pid: 0, started_unix: 0, cols: 80, rows: 24, sock: "/tmp/mock-ghost.sock", record_version: 1 },
];

export async function invoke<T>(cmd: string, args?: any): Promise<T> {
  switch (cmd) {
    case "list_sessions":
      return [...sessions] as unknown as T;
    case "reconcile_sessions":
      return { healthy: [...sessions], ghost: [...ghosts], orphan: [...orphans] } as unknown as T;
    case "attach":
      // Simulate REPLAY on attach.
      setTimeout(() => emit("hub://replay", { id: args.id, bytes: Array.from(new TextEncoder().encode("$ ")) }), 5);
      return undefined as unknown as T;
    case "send_input":
      // Echo keystrokes back as live output.
      setTimeout(() => emit("hub://output", { id: args.id, bytes: args.bytes }), 1);
      return undefined as unknown as T;
    case "get_buffer_size":
      return bufferSize as unknown as T;
    case "detach":
      return undefined as unknown as T;
    case "kill": {
      // A kill can target any bucket -- healthy, orphan (leftover relay), or
      // ghost (dead-socket record) -- so remove from whichever list has it
      // (mirrors the real `ConnManager::kill`, which handles both attached
      // and not-attached ids via the same command).
      const target = args?.id;
      // Simulate real backend kill lag: `kill()` returns immediately but the
      // shell dies a beat later, so the session lingers ~300ms before dropping
      // from the next reconcile. This is what makes the sidebar Kill button's
      // in-button spinner (which stays up until the session is actually gone)
      // observable rather than an imperceptible flash.
      setTimeout(() => {
        for (const list of [sessions, orphans, ghosts]) {
          const idx = list.findIndex((s) => s.id === target);
          if (idx !== -1) {
            list.splice(idx, 1);
            break;
          }
        }
      }, 300);
      // Mirrors daemon.rs `viewer_actor`, which emits hub://closed for this id
      // once the relay dies -- so a Terminal tile still open on this id reacts.
      setTimeout(() => emit("hub://closed", { id: target, exitCode: null }), 250);
      return undefined as unknown as T;
    }
    case "spawn_session": {
      // Mirrors the real backend's async registration: the fake session
      // shows up a beat later, matching Toolbar's setTimeout(onNew, 400).
      const id = nextId++;
      setTimeout(() => {
        sessions.push({ id, origin: "Hub", title: "hub-relay", pid: 0, started_unix: 0, cols: 80, rows: 24 });
      }, 100);
      return undefined as unknown as T;
    }
    case "set_buffer_size":
      // FIX 3: stateful, so the Settings Save path (set then get) is
      // actually exercised under the mock instead of being a silent no-op.
      bufferSize = args?.size ?? bufferSize;
      return undefined as unknown as T;
    case "hub_is_installed":
      return installed as unknown as T;
    case "hub_do_install":
      installed = true;
      return undefined as unknown as T;
    case "hub_do_uninstall":
      // Real backend reverts, self-deletes the .app, and quits; the mock just
      // flips state so the Settings flow is exercisable headlessly.
      installed = false;
      return undefined as unknown as T;
    case "get_setup_declined":
      return declined as unknown as T;
    case "set_setup_declined":
      declined = !!args?.declined;
      return undefined as unknown as T;
    default:
      return undefined as unknown as T;
  }
}

export async function listen(event: string, handler: Handler) {
  (handlers[event] ??= []).push(handler);
  return () => {
    handlers[event] = (handlers[event] ?? []).filter((h) => h !== handler);
  };
}

// Task 11: Simulate the backend signalling a completed daemon connection
// shortly after load, so App.svelte's startup path (mount-refresh + refresh-
// on-connect) runs end-to-end under the mock/Playwright.
if (typeof window !== "undefined") {
  setTimeout(() => {
    (handlers["hub://connected"] ?? []).forEach((h) => h({ payload: null }));
  }, 20);
}
