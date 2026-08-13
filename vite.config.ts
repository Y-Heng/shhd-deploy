import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri 开发约定：固定端口，报错不清屏
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "es2021",
    minify: "esbuild",
  },
});
