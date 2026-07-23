// Task 5: shared API layer — typed Tauri command wrappers + a mock/real IPC
// switch so components (Terminal.svelte) never call `invoke`/`listen`
// directly. `VITE_MOCK` routes everything through `./mock`'s in-memory IPC
// for dev-smoke/Playwright runs where no real Tauri backend is attached.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";
import * as mock from "./mock";

const MOCK = !!import.meta.env.VITE_MOCK;

// Route through the mock IPC in Playwright/dev-smoke builds, else real Tauri.
const invoke: typeof tauriInvoke = MOCK ? (mock.invoke as any) : tauriInvoke;
// Explicit annotation (deviation from the brief's un-annotated snippet):
// without it, the `any` branch makes the whole const collapse to `any`,
// which silently drops the generic signature and breaks `listen<T>(...)`
// call sites in Terminal.svelte under strict `svelte-check`/tsc.
export const listen: typeof tauriListen = MOCK ? (mock.listen as any) : tauriListen;
export type { UnlistenFn };

export type OriginTag = "External" | "Hub";

export interface SessionInfo {
  id: number;
  origin: OriginTag;
  title: string;
  pid: number;
  started_unix: number;
  cols: number;
  rows: number;
}
export interface GhostRecord extends SessionInfo {
  sock: string;
  record_version: number;
}
export interface Buckets {
  healthy: SessionInfo[];
  ghost: GhostRecord[];
  orphan: SessionInfo[];
}

export const listSessions = () => invoke<SessionInfo[]>("list_sessions");
export const reconcile = () => invoke<Buckets>("reconcile_sessions");
export const attach = (id: number) => invoke<void>("attach", { id });
export const detach = (id: number) => invoke<void>("detach", { id });
export const kill = (id: number) => invoke<void>("kill", { id });
export const sendInput = (id: number, bytes: number[]) => invoke<void>("send_input", { id, bytes });
export const resize = (id: number, cols: number, rows: number) => invoke<void>("resize", { id, cols, rows });
export const claimSize = (id: number, cols: number, rows: number) => invoke<void>("claim_size", { id, cols, rows });
export const spawnSession = () => invoke<void>("spawn_session");
export const getBufferSize = () => invoke<number>("get_buffer_size");
export const setBufferSize = (size: number) => invoke<void>("set_buffer_size", { size });

// App-lifecycle: install/uninstall wrappers over the tested `hub` CLI, plus the
// first-run "declined" flag so a prior "Not now" is honored across launches.
export const hubIsInstalled = () => invoke<boolean>("hub_is_installed");
export const hubDoInstall = () => invoke<void>("hub_do_install");
export const hubDoUninstall = () => invoke<void>("hub_do_uninstall");
export const getSetupDeclined = () => invoke<boolean>("get_setup_declined");
export const setSetupDeclined = (declined: boolean) =>
  invoke<void>("set_setup_declined", { declined });

// FIX 3: test-only hook (mock builds only, never wired up against the real
// Tauri backend) so Playwright can assert `getBufferSize()` reflects a Save
// from *within the same running page* -- there's no other DOM-visible signal
// for "the mock's set_buffer_size actually persisted the value", since
// Settings.svelte only calls get_buffer_size once, on mount.
if (MOCK && typeof window !== "undefined") {
  (window as any).__hubApi = { getBufferSize, setBufferSize };
}
