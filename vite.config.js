import { defineConfig } from 'vite';
import { fileURLToPath, URL } from 'node:url';

// dist/ 会提交进 git：Mac 侧没有 node，Tauri 编译期直接嵌入这份预构建产物
export default defineConfig({
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    outDir: 'dist',
    target: 'es2021',
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        // v0.9 M2：独立进度子窗口（FFS 同款）——第二入口
        progress: fileURLToPath(new URL('./progress.html', import.meta.url)),
      },
    },
  },
});
