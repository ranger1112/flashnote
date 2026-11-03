import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    // 不用 Tauri 默认的 1420：同一台机器上并行开发另一个 Tauri 应用时两边会抢同一个端口。
    port: 1421,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "chrome105",
    // 不指定 minify：Vite 8 默认用 Oxc 压缩。写成 "esbuild" 会要求额外安装 esbuild，
    // 而 Vite 8 只把它声明为可选 peer 依赖，缺了就会构建失败。
  },
});
