// A reliable in-app confirm(), replacing window.confirm() -- which is a NO-OP
// in the wry/WKWebView the app runs in (it returns false without ever showing a
// dialog, so any `if (!confirm(...)) return;` silently aborts). That's why the
// Kill / Uninstall buttons "did nothing" while Install (no confirm) worked.
//
// Two shapes:
//  - `await confirmAction(body, opts)` -> boolean (did the user confirm?).
//  - pass `opts.action` (async) to run the work INSIDE the dialog: the confirm
//    button shows a spinner and the dialog stays open until the action settles,
//    then closes. Use this for slow ops (kill, uninstall) so the button the
//    user just clicked shows progress, instead of closing instantly and hiding
//    the wait.
import { writable, get } from "svelte/store";

export interface PendingConfirm {
  title: string;
  body: string;
  confirmLabel: string;
  danger: boolean;
  busy: boolean;
  action?: () => Promise<void> | void;
  resolve: (ok: boolean) => void;
}

export const pendingConfirm = writable<PendingConfirm | null>(null);

export function confirmAction(
  body: string,
  opts: {
    title?: string;
    confirmLabel?: string;
    danger?: boolean;
    action?: () => Promise<void> | void;
  } = {},
): Promise<boolean> {
  return new Promise((resolve) => {
    pendingConfirm.set({
      title: opts.title ?? "Are you sure?",
      body,
      confirmLabel: opts.confirmLabel ?? "Confirm",
      danger: opts.danger ?? false,
      busy: false,
      action: opts.action,
      resolve,
    });
  });
}

// User cancelled: clear + resolve(false). No-op while an action is running.
export function cancelConfirm() {
  const p = get(pendingConfirm);
  if (!p || p.busy) return;
  pendingConfirm.set(null);
  p.resolve(false);
}

// User confirmed. If there's an async action, flip `busy` (button spinner) and
// keep the dialog open until it settles, THEN close + resolve(true). Otherwise
// close immediately. Guarded so a double-click can't run the action twice.
export async function acceptConfirm() {
  const p = get(pendingConfirm);
  if (!p || p.busy) return;
  if (p.action) {
    pendingConfirm.update((x) => (x ? { ...x, busy: true } : x));
    try {
      await p.action();
    } catch (e) {
      console.error("confirm action failed", e);
    }
  }
  pendingConfirm.set(null);
  p.resolve(true);
}
