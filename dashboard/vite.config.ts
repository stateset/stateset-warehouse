import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
export default defineConfig({
  plugins: [react()],
  server: {
    strictPort: true,
    proxy: {
      "/auth": "http://127.0.0.1:8088",
      "/v1": "http://127.0.0.1:8088",
      "/health": "http://127.0.0.1:8088",
    },
  },
});
