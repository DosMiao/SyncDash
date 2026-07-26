import { defineConfig } from 'vite';

// dist/ 会提交进 git：Mac 侧没有 node，Tauri 编译期直接嵌入这份预构建产物
export default defineConfig({
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { outDir: 'dist', target: 'es2021' },
});
