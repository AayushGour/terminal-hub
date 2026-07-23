import { test, expect } from "@playwright/test";

test("new session button is present and clickable", async ({ page }) => {
  await page.goto("/");
  const btn = page.getByRole("button", { name: "+ New session" });
  await expect(btn).toBeVisible();
  await btn.click(); // mock spawn_session is a no-op; must not throw
});

test("clicking new session adds a session to the list (mock spawn + refresh)", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("mock #1")).toBeVisible();
  await page.getByRole("button", { name: "+ New session" }).click();
  // Toolbar waits 400ms then calls SessionList.refresh(); the mock IPC
  // registers the fake spawned session ~100ms after spawn_session is
  // invoked, so it is present by the time refresh() polls reconcile_sessions.
  await expect(page.getByText("hub-relay #2")).toBeVisible();
});
