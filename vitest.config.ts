import path from "path";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Kept apart from vite.config.ts, which carries Tauri's dev-server settings and has no business
// running under the test runner.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    // Component tests drive the real components, Radix and all, so they need a DOM. The pure
    // modules do not care which environment they run in.
    environment: "jsdom",
    // The suite runs as the Windows app; a test about Linux overrides the user agent itself.
    environmentOptions: {
      jsdom: { userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) rivals-toolkit-test" },
    },
    setupFiles: ["./src/test/setup.ts"],
    restoreMocks: true,
  },
});
