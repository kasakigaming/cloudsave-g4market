import { defineConfig } from "vite";
import prefixer from "postcss-prefix-selector";

/// Mỗi file theme chỉ áp dụng dưới đúng `<html data-theme="…">` của nó, nên
/// hai bộ CSS cùng nằm trong bundle mà không đè lên nhau. `:root` / `html` /
/// `body` trong file theme được gắn vào chính phần tử html đó.
function scoped(theme: string, file: RegExp) {
  const prefix = `html[data-theme="${theme}"]`;
  return prefixer({
    prefix,
    includeFiles: [file],
    transform(_prefix: string, selector: string) {
      if (selector === ":root" || selector === "html") return prefix;
      if (selector.startsWith(":root")) return prefix + selector.slice(5);
      if (selector.startsWith("html")) return prefix + selector.slice(4);
      return `${prefix} ${selector}`;
    },
  });
}

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    // Tauri trỏ cứng vào cổng này; nếu bị chiếm thì phải báo lỗi chứ không
    // được lặng lẽ nhảy sang cổng khác.
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  css: {
    postcss: {
      plugins: [scoped("glass", /themes[\/]glass\.css$/), scoped("flat", /themes[\/]flat\.css$/)],
    },
  },
  build: {
    target: "chrome105",
    minify: process.env.TAURI_ENV_DEBUG ? false : "esbuild",
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
