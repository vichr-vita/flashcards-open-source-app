import { build } from "esbuild";
await build({ entryPoints: ["browser/localPasskey.ts"], outfile: "dist/local/passkey-browser.js", bundle: true, minify: true, format: "esm", platform: "browser", target: "es2022" });
