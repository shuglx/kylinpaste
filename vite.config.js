import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));

// 目标机 webkit2gtk 为 2.28.1(约 Safari 13 时代引擎),
// 构建目标锁定 es2020/safari13 保证兼容性
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // 显式指定唯一入口,避免 dev 扫描器爬取 ref/QuickClipboard 的多页面源码
  build: {
    target: ['es2020', 'safari13'],
    outDir: 'dist',
    rollupOptions: {
      input: resolve(root, 'index.html'),
    },
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
