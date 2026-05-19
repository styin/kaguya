import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { supervisorPlugin } from "./server/plugin";
import pkg from "./package.json" with { type: "json" };

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  plugins: [react(), supervisorPlugin()],
  server: {
    port: 3000,
    proxy: {
      "/ws": {
        target: "ws://127.0.0.1:8080",
        ws: true,
      },
      "/health": {
        target: "http://127.0.0.1:8080",
      },
    },
  },
});
