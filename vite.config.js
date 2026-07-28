import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath, URL } from 'node:url';

// dist/ is committed to git: the Mac side has no node, so Tauri embeds this prebuilt artifact at compile time
export default defineConfig({
  clearScreen: false,
  // Pinned to plugin-react 4: 5+ requires Vite 8, and moving the build off Vite 6 is not part of a UI change
  plugins: [react()],
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
      // No content hash in filenames: dist/ is committed, and a hash would turn every
      // frontend edit into an add/delete pair plus an index.html churn. Cache busting buys
      // nothing here — Tauri serves these from the binary over tauri://, not over HTTP.
      //
      // Without an explicit name the shared chunk is named after whichever module Rollup picks as
      // its representative. That was `core/zoom.ts` — 35 lines — so React, react-dom, lucide and
      // all of core/ shipped as `assets/zoom.js` (220 KB), and the whole stylesheet as
      // `assets/zoom.css`. Both windows load it, so it is `shared`.
      output: {
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name].[ext]',
        manualChunks(id) {
          if (id.includes('node_modules')) return 'vendor';
          // Everything both windows share: core/ and the stylesheet. The emitted CSS attaches to
          // whichever chunk pulls it in, so naming this one is what stops the whole stylesheet
          // shipping as `zoom.css`.
          if (id.includes('/typescript/core/') || id.endsWith('styles.css')) return 'shared';
          return null;
        },
      },
    },
  },
});
