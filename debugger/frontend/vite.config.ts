import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "path";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
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
        terminal: resolve(__dirname, "terminal.html"),
        trace: resolve(__dirname, "trace.html"),
        log: resolve(__dirname, "log.html"),
        // Phase 0 spike (issue #379) — throwaway, removed with the rest of
        // the spike code once the write-up lands.
        spike: resolve(__dirname, "spike.html"),
        stackDetached: resolve(__dirname, "stack-detached.html"),
      },
    },
  },
});
