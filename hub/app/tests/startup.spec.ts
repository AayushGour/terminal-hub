import { test, expect } from "@playwright/test";

test("startup lists live shells without any manual action", async ({ page }) => {
  await page.goto("/");
  // The mock's hub://connected auto-emit triggers reconcile; the Healthy bucket fills.
  await expect(page.locator("h4", { hasText: "Healthy" })).toBeVisible();
  await expect(page.locator(".row .title").first()).toContainText("mock");
  // Orphan bucket header is present so the user can kill leftovers.
  await expect(page.locator("h4", { hasText: "Orphan" })).toBeVisible();
});

test("startup surfaces an orphaned Hub relay and a ghost record for cleanup", async ({ page }) => {
  await page.goto("/");
  // Task 11: the mock seeds an orphan (live, no record) and a ghost (record,
  // dead socket) from the very first reconcile so BOTH buckets render a real
  // row -- not just an empty header -- confirming orphans/ghosts are visible
  // on startup with no clicks.
  await expect(page.locator(".row.orphan .title")).toContainText("orphan-relay");
  await expect(page.locator("h4", { hasText: "Ghost" })).toBeVisible();
  await expect(page.locator(".row.ghost .title")).toContainText("ghost-relay");
});

test("killing a startup-visible orphan removes it from the list", async ({ page }) => {
  await page.goto("/");
  const orphanRow = page.locator(".row.orphan", { hasText: "orphan-relay" });
  await expect(orphanRow).toBeVisible();
  await orphanRow.getByRole("button", { name: "Kill" }).click();
  // onKill uses the in-app confirm (window.confirm no-ops in the webview).
  await page.getByRole("alertdialog").getByRole("button", { name: "Kill" }).click();
  await expect(page.locator(".row.orphan", { hasText: "orphan-relay" })).toHaveCount(0);
});

test("cleaning up a startup-visible ghost removes it from the list", async ({ page }) => {
  await page.goto("/");
  const ghostRow = page.locator(".row.ghost", { hasText: "ghost-relay" });
  await expect(ghostRow).toBeVisible();
  await ghostRow.getByRole("button", { name: "Clean up" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Kill" }).click();
  await expect(page.locator(".row.ghost", { hasText: "ghost-relay" })).toHaveCount(0);
});
