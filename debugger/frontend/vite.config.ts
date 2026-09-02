/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "path";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
  },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
    // Transform the splash screen's module graph (main.tsx -> App.tsx ->
    // ThemeContext.tsx) as soon as the dev server starts, rather than
    // on-demand from the first browser request. `cargo tauri dev` shows the
    // window as soon as it gets any response from the dev server (the
    // static index.html), so without this, the cold esbuild/transform cost
    // for that first real module request lands *after* the window is
    // already visible — a blank white window for a few seconds before
    // "Initializing…" appears (issue #405; doesn't affect production
    // builds, which serve pre-built static assets).
    warmup: {
      clientFiles: ["./src/main.tsx", "./src/App.tsx", "./src/ThemeContext.tsx"],
    },
  },
  css: {
    preprocessorOptions: {
      scss: {
        api: "modern-compiler",
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        "terminal-detached": resolve(__dirname, "terminal-detached.html"),
        "display-detached": resolve(__dirname, "display-detached.html"),
        "led-matrix-detached": resolve(__dirname, "led-matrix-detached.html"),
      },
    },
  },
});
