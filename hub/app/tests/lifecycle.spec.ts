import { test, expect } from "@playwright/test";

// App-lifecycle smokes (mocked IPC). The mock defaults to installed=true so a
// plain "/" load shows NO consent modal (keeps the other specs unaffected);
// `?notInstalled=1` opts into the first-run state, `?declined=1` seeds a prior
// "Not now".

test("first-run consent dialog appears when hub is not installed", async ({ page }) => {
  await page.goto("/?notInstalled=1");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("heading", { name: "Set up hub?" })).toBeVisible();
  await expect(dialog.getByText(/reversible line to/)).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Enable" })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Not now" })).toBeVisible();
});

test("no consent dialog when hub is already installed", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test('"Not now" dismisses the dialog and it does not reappear this session', async ({ page }) => {
  await page.goto("/?notInstalled=1");
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.getByRole("button", { name: "Not now" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test('"Enable" installs hub, closes the dialog, and shows a confirmation', async ({ page }) => {
  await page.goto("/?notInstalled=1");
  await page.getByRole("dialog").getByRole("button", { name: "Enable" }).click();
  // hub_do_install ran, the modal closed, and the success confirmation shows.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("status")).toContainText("hub enabled");
});

test("Settings shows Uninstall when hub is installed", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Uninstall hub & remove app" })).toBeVisible();
});

test("Settings offers Install when not installed but consent was declined", async ({ page }) => {
  // declined=1 suppresses the modal, so the sidebar Settings affordance is what
  // lets a user who clicked "Not now" enable capture later.
  await page.goto("/?notInstalled=1&declined=1");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Install / Enable capture" })).toBeVisible();
});

test("Uninstall from Settings confirms, then reverts to the Install affordance", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Uninstall hub & remove app" }).click();
  // Uses the in-app confirm (window.confirm no-ops in the webview).
  await page.getByRole("alertdialog").getByRole("button", { name: "Uninstall & remove" }).click();
  // Mock uninstall flips state (no real self-delete/quit) → Install offered.
  await expect(page.getByRole("button", { name: "Install / Enable capture" })).toBeVisible();
});
