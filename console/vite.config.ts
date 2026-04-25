import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { supervisorPlugin } from "./server/plugin";

export default defineConfig({
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
