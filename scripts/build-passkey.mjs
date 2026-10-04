import { build } from "esbuild";

// Authentication keeps its deployed browser code while the server runs in Rust.
await build({
  entryPoints: ["apps/auth/browser/localPasskey.ts"],
  outfile: "apps/web/dist/assets/local-passkey.js",
  bundle: true,
  minify: true,
  format: "esm",
  platform: "browser",
  target: "es2022",
});
