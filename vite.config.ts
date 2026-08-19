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
    // Element Plus 整包体积超过 Rollup 默认 500 kB，桌面端可接受
    chunkSizeWarningLimit: 1024,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return;
          if (id.includes("@xterm")) return "xterm";
          if (id.includes("element-plus") || id.includes("@element-plus")) return "element-plus";
        },
      },
    },
  },
});
