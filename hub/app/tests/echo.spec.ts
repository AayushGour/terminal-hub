import { test, expect } from "@playwright/test";

test("typing into the terminal echoes back", async ({ page }) => {
  await page.goto("/");
  // Task 7: sessions no longer auto-open a tile; click the session to open it.
  await page.getByText("mock #1").click();
  await page.locator(".xterm").first().waitFor();
  // Focus xterm's hidden helper textarea, then type.
  await page.locator(".xterm-helper-textarea").first().click();
  await page.keyboard.type("hello");
  await expect(page.locator(".xterm-rows")).toContainText("hello");
});
