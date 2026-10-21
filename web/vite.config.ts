import { defineConfig } from "vite"
import { tanstackStart } from "@tanstack/react-start/plugin/vite"
import viteReact from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"

// The lager server serves the built files under /-/ and answers every
// other browser page with the shell (docs/decisions/0014-web-interface.md).
// During development, Vite forwards the API and Git requests to a running
// server (LAGER_URL, default http://127.0.0.1:8080).
const server = process.env.LAGER_URL ?? "http://127.0.0.1:8080"

export default defineConfig({
  // Built files go to /-/assets/, which no repository name can take.
  build: { assetsDir: "-/assets" },
  resolve: { tsconfigPaths: true },
  server: {
    proxy: {
      "/-/api": { target: server, ws: true, changeOrigin: false },
      "^/.+/-/raw/": { target: server, changeOrigin: false },
    },
  },
  plugins: [
    tailwindcss(),
    tanstackStart({
      spa: { enabled: true },
    }),
    viteReact(),
  ],
})
