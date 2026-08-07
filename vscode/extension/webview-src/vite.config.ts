import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "path";

export default defineConfig({
  plugins: [react()],
  base: "./",
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
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
        trace: resolve(__dirname, "trace.html"),
      },
      output: {
        // Fixed names (no content hash): the extension host references these
        // paths directly when building the webview's HTML shell, rather than
        // parsing Vite's own generated trace.html (the standard approach for
        // VS Code webviews, which need `asWebviewUri`-rewritten script/link
        // tags that a raw Vite build wouldn't produce anyway).
        entryFileNames: "[name].js",
        assetFileNames: "[name].[ext]",
      },
    },
  },
});
