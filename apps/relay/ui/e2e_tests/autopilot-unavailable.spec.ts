import { test, expect } from "@playwright/test";

// Relay does not offer Autopilot (upstream TensorZero's hosted service). Every
// /autopilot route must render the honest "not available" page and the
// sidebar must not link to it. See app/routes/autopilot/layout.tsx.

const UNAVAILABLE_HEADING = "Autopilot is not available in Relay";

for (const path of [
  "/autopilot",
  "/autopilot/sessions",
  "/autopilot/sessions/new",
  "/autopilot/sessions/0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
]) {
  test(`${path} renders the Autopilot unavailable page`, async ({ page }) => {
    // `/autopilot` redirects to `/autopilot/sessions`; goto follows it.
    const response = await page.goto(path);
    expect(response?.status()).toBe(501);
    await expect(page.getByText(UNAVAILABLE_HEADING)).toBeVisible();
    await expect(
      page.getByRole("link", { name: "Back to the overview" }),
    ).toHaveAttribute("href", /\/$/);
  });
}

test("the sidebar does not link to Autopilot", async ({ page }) => {
  const response = await page.goto("/");
  expect(response?.status()).toBe(200);
  await expect(page.locator('a[href$="/autopilot"]')).toHaveCount(0);
  await expect(page.locator('a[href$="/autopilot/sessions/new"]')).toHaveCount(
    0,
  );
});
