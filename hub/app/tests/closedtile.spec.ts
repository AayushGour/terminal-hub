import { test, expect } from "@playwright/test";

// A killed/exited session's tile must be REMOVED from the grid, not left
// hanging as a dead/dimmed tile: daemon.rs emits hub://closed per session id,
// Terminal.svelte reacts by calling closeTile(id) -> the tile unmounts.
test("killing an open tile's session removes its tile from the grid", async ({ page }) => {
  await page.goto("/");
  // Open the mock session (Healthy, id 1) as a tile (click its name/title).
  await page.getByRole("button", { name: /mock #1/ }).click();
  const tile = page.locator(".tilewrap").first();
  await expect(tile.locator(".xterm")).toBeVisible();
  await expect(page.locator(".tilewrap")).toHaveCount(1);

  // Kill it from the session list, then confirm in the in-app dialog (the
  // mock's `kill` then emits hub://closed for this id, mirroring daemon.rs's
  // viewer_actor, which the still-open tile's Terminal.svelte listens for).
  await page.locator(".row", { hasText: "mock #1" }).getByRole("button", { name: "Kill" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Kill" }).click();

  // The tile is gone -- no dead tile lingers in the grid.
  await expect(page.locator(".tilewrap")).toHaveCount(0);
});
