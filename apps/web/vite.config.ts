import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

/**
 * 注意：这里**刻意不用任何 Node API**（不用 `fileURLToPath`、不用 `process.env`）。
 * 一旦用了就得引 `@types/node`，而这个文件只跑在构建期 ——
 * 为它多一个依赖不划算。`import.meta.url` 是标准，两边都认。
 */
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      // shadcn 的约定就是 @/…；tsconfig.json 里也声明了一份，两边要一致
      '@': new URL('./src', import.meta.url).pathname,
    },
  },
  server: {
    // 开发时把 /api 代理到 Rust 服务，免得前端还要处理 CORS。
    // 要指到别处就改这一行（或以后接 env）。
    proxy: {
      '/api': { target: 'http://127.0.0.1:8080', changeOrigin: true },
    },
  },
});
