import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// This server never mounts the authenticated app or points at a product backend.
export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  publicDir: fileURLToPath(new URL("../../public", import.meta.url)),
  plugins: [react()],
  resolve: {
    alias: [{
      find: /^(?:\.{1,2}\/)+appData$/,
      replacement: fileURLToPath(new URL("./appData.ts", import.meta.url)),
    }],
  },
  server: {
    host: "127.0.0.1",
    port: 4318,
    strictPort: true,
    fs: { allow: [fileURLToPath(new URL("../../../../", import.meta.url))] },
  },
});
