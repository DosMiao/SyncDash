import { defineConfig } from 'vite';
import { fileURLToPath, URL } from 'node:url';

// dist/ is committed to git: the Mac side has no node, so Tauri embeds this prebuilt artifact at compile time
export default defineConfig({
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    outDir: 'dist',
    target: 'es2021',
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        // v0.9 M2: standalone progress sub-window (same as FFS) — the second entry point
        progress: fileURLToPath(new URL('./progress.html', import.meta.url)),
      },
    },
  },
});
