import { defineConfig, devices } from "@playwright/test";

// E2E against the built UI (vite preview). Without Tauri, src/api.ts uses the in-browser
// mocks (src/mocks.ts), so these tests exercise the real UI end to end.
export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  retries: process.env.CI ? 1 : 0,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:5199",
    viewport: { width: 1280, height: 860 },
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 860 } } }],
  webServer: {
    // Production bundle via preview: no file watchers, same code that ships.
    command: "npx vite build && npx vite preview --host 127.0.0.1 --port 5199 --strictPort",
    url: "http://127.0.0.1:5199",
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
});
