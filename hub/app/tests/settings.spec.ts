import { test, expect } from "@playwright/test";

test("buffer-size setting shows the value and RAM tradeoff hint", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator('input[type="number"]')).toHaveValue("10000");
  await expect(page.getByText(/buffer × line width × live session count/)).toBeVisible();
});

// FIX 3: the mock's set_buffer_size used to be a no-op (get_buffer_size
// always hardcoded 10000), so clicking Save was never actually verified to
// persist anything. Now the mock is stateful; assert the Save path both
// shows its own "saved" confirmation AND that getBufferSize() -- the exact
// API Settings.svelte calls on mount -- reflects the new value afterward.
test("saving a new buffer size persists it (getBufferSize reflects the Save)", async ({ page }) => {
  await page.goto("/");
  const input = page.locator('input[type="number"]');
  await input.fill("25000");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("saved ✓")).toBeVisible();

  const persisted = await page.evaluate(() => (window as any).__hubApi.getBufferSize());
  expect(persisted).toBe(25000);
});
