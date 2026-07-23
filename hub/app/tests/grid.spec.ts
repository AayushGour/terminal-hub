import { test, expect } from "@playwright/test";

test("opening a session renders a floating terminal window (title bar + resize handle)", async ({ page }) => {
  await page.goto("/");
  await page.locator(".title").first().click();
  const tile = page.locator(".tilewrap").first();
  await expect(tile).toBeVisible();
  await expect(tile.locator(".xterm")).toBeVisible();
  await expect(tile.locator(".titlebar")).toBeVisible(); // drag to move
  await expect(tile.locator(".resize")).toBeAttached(); // drag to resize
  await expect(tile).toHaveCSS("position", "absolute"); // free-form positioning
});

test("dragging the title bar repositions the window", async ({ page }) => {
  await page.goto("/");
  await page.locator(".title").first().click();
  const tile = page.locator(".tilewrap").first();
  const before = (await tile.boundingBox())!;
  const bar = (await tile.locator(".titlebar").boundingBox())!;
  await page.mouse.move(bar.x + 40, bar.y + 12);
  await page.mouse.down();
  await page.mouse.move(bar.x + 160, bar.y + 92, { steps: 6 });
  await page.mouse.up();
  const after = (await tile.boundingBox())!;
  expect(after.x).toBeGreaterThan(before.x + 60);
  expect(after.y).toBeGreaterThan(before.y + 40);
});

test("dragging the corner resizes the window", async ({ page }) => {
  await page.goto("/");
  await page.locator(".title").first().click();
  const tile = page.locator(".tilewrap").first();
  const before = (await tile.boundingBox())!;
  const rz = (await tile.locator(".resize").boundingBox())!;
  await page.mouse.move(rz.x + 6, rz.y + 6);
  await page.mouse.down();
  await page.mouse.move(rz.x + 126, rz.y + 96, { steps: 6 });
  await page.mouse.up();
  const after = (await tile.boundingBox())!;
  expect(after.width).toBeGreaterThan(before.width + 60);
  expect(after.height).toBeGreaterThan(before.height + 40);
});

test("zoom controls scale the canvas", async ({ page }) => {
  await page.goto("/");
  await page.locator(".title").first().click();
  const tile = page.locator(".tilewrap").first();
  const before = (await tile.boundingBox())!;
  await page.locator("button[aria-label='zoom out']").click();
  await page.locator("button[aria-label='zoom out']").click();
  await expect(page.locator(".controls .pct")).not.toHaveText("100%");
  const after = (await tile.boundingBox())!;
  expect(after.width).toBeLessThan(before.width); // zoomed out -> smaller
});

test("dragging empty canvas pans the view (tiles move with it)", async ({ page }) => {
  await page.goto("/");
  await page.locator(".title").first().click();
  const tile = page.locator(".tilewrap").first();
  const before = (await tile.boundingBox())!;
  // start on empty canvas to the right of the tile
  const sx = before.x + before.width + 100, sy = before.y + before.height / 2;
  await page.mouse.move(sx, sy);
  await page.mouse.down();
  await page.mouse.move(sx - 160, sy + 60, { steps: 6 });
  await page.mouse.up();
  const after = (await tile.boundingBox())!;
  expect(after.x).toBeLessThan(before.x - 80); // panned left
  expect(after.y).toBeGreaterThan(before.y + 30); // panned down
});
