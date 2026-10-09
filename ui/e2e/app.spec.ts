import { expect, test, type Page } from "@playwright/test";

const shots = process.env.E2E_SHOTS;

async function shot(page: Page, name: string) {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Deep cleanup" })).toBeVisible();
});

test("overview: scan, drill down, delete with in-use warning", async ({ page }) => {
  const sidebar = page.getByRole("complementary");
  await sidebar.getByRole("button", { name: "Overview" }).click();
  await page.getByRole("button", { name: "Scan", exact: true }).click();

  const grid = page.getByRole("grid", { name: "Folder contents" });
  await expect(grid.getByRole("row").first()).toContainText(".cache");
  await shot(page, "overview-home");

  await grid.getByRole("button", { name: ".cache", exact: true }).click();
  await expect(page.getByRole("navigation", { name: "Breadcrumbs" })).toContainText(".cache");
  await grid.getByRole("checkbox", { name: "Select uv" }).check();
  await expect(page.getByText("1 selected")).toBeVisible();

  await page.getByRole("button", { name: "Delete permanently" }).click();
  const dialog = page.getByRole("dialog", { name: "Delete permanently?" });
  await expect(dialog.getByRole("alert")).toContainText("uvx (4242, mapped)");
  await shot(page, "overview-confirm");
  await dialog.getByRole("button", { name: "Delete permanently" }).click();

  await expect(page.getByRole("status")).toContainText("Deleted 1 item");
  await expect(grid.getByRole("checkbox", { name: "Select uv" })).toHaveCount(0);
});

test("overview: keyboard only", async ({ page }) => {
  await page.getByRole("complementary").getByRole("button", { name: "Overview" }).click();
  await page.getByLabel("Folder to scan").press("Enter");
  const grid = page.getByRole("grid", { name: "Folder contents" });
  await expect(grid.getByRole("row").first()).toHaveAttribute("aria-selected", "true");
  await grid.focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter"); // open Downloads
  await expect(grid.getByRole("row").first()).toContainText("ubuntu-24.04.iso");
  await page.keyboard.press("Space");
  await page.keyboard.press("Delete");
  await expect(page.getByRole("dialog")).toContainText("ubuntu-24.04.iso");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.keyboard.press("Backspace");
  await expect(grid.getByRole("row").nth(1)).toHaveAttribute("aria-selected", "true");
});

test("overview: whole disk explains hidden usage", async ({ page }) => {
  await page.getByRole("complementary").getByRole("button", { name: "Overview" }).click();
  await page.locator(".root-chips").getByRole("button", { name: "/", exact: true }).click();
  const card = page.getByRole("region", { name: "Disk usage" });
  await expect(card).toContainText("Docker data (/var/lib/docker)");
  await expect(page.getByRole("grid").getByRole("checkbox", { name: "Select usr" })).toBeDisabled();
  await shot(page, "overview-root");
});

test("docker: remove stale images and prune old cache", async ({ page }) => {
  await page.getByRole("complementary").getByRole("button", { name: "Docker" }).click();
  await expect(page.getByRole("table", { name: "Docker images" })).toContainText("tritonserver");
  await shot(page, "docker");

  await page.getByRole("button", { name: /Select unused > 14d/ }).click();
  await page.getByRole("button", { name: "Remove selected images" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Remove" }).click();
  await expect(page.getByRole("status")).toContainText("Removed");
  await expect(page.getByRole("table")).not.toContainText("tritonserver");

  const cache = page.getByRole("region", { name: "Build cache" });
  await cache.getByRole("button", { name: /Prune cache/ }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Prune" }).click();
  await expect(page.getByRole("status")).toContainText("Pruned build cache");
});
