import { test, expect } from "@playwright/test";

test("session list shows an origin badge and open/kill controls", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("Sessions")).toBeVisible();
  // Task 11: the mock now also seeds an orphan + a ghost (both origin Hub)
  // so the startup buckets aren't empty; assert on the Healthy row's own
  // badge rather than an exact array match across every badge on the page.
  await expect(page.locator(".badge").first()).toContainText("Hub");
  // A not-yet-attached healthy session shows "Open" (issue #2: the row's
  // button reflects attach state -- it becomes "Detach" once opened).
  const healthyRow = page.locator(".row", { hasText: "mock #1" });
  await expect(healthyRow.getByRole("button", { name: "Open" })).toBeVisible();
  await expect(healthyRow.getByRole("button", { name: "Kill" })).toBeVisible();
});

test("opening a session flips the row to attached/Detach, closing flips it back", async ({ page }) => {
  await page.goto("/");
  const healthyRow = page.locator(".row", { hasText: "mock #1" });
  // Open -> tile mounts, row shows attached state + a Detach button.
  await healthyRow.getByRole("button", { name: "Open" }).click();
  await expect(healthyRow).toHaveClass(/attached/);
  await expect(healthyRow.getByRole("button", { name: "Detach" })).toBeVisible();
  // Detach -> tile unmounts, row reverts to Open.
  await healthyRow.getByRole("button", { name: "Detach" }).click();
  await expect(healthyRow).not.toHaveClass(/attached/);
  await expect(healthyRow.getByRole("button", { name: "Open" })).toBeVisible();
});

test("Kill shows an in-button loader on the sidebar button while it's in progress", async ({ page }) => {
  await page.goto("/");
  const row = page.locator(".row", { hasText: "mock #1" });
  await row.getByRole("button", { name: "Kill" }).click();
  // Confirm; the dialog closes immediately (non-blocking).
  await page.getByRole("alertdialog").getByRole("button", { name: "Kill" }).click();
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  // The sidebar Kill button now shows an in-button spinner while the shell dies.
  await expect(row.locator(".act.kill .spinner")).toBeVisible();
  // Once the session is actually gone, the row (and its spinner) disappear.
  await expect(page.locator(".row", { hasText: "mock #1" })).toHaveCount(0);
});
