import { createHash } from "node:crypto";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import type { Plugin } from "vite";

/** Only public build artifacts are cached. API data remains in the existing IndexedDB sync model. */
export function offlineShellPlugin(): Plugin {
  let dist = resolve("dist");
  return {
    name: "nibomo-offline-shell",
    apply: "build",
    configResolved(config) {
      dist = resolve(config.root, config.build.outDir);
    },
    async closeBundle() {
      const files = (await readdir(dist, { recursive: true })).filter(name =>
        ["index.html", "manifest.webmanifest", "logo.svg", "icon.svg", "favicon.ico"].includes(name) || /^icon-\d+\.png$/.test(name)
        || /^assets\/.*\.(?:js|css|json|wasm|woff2?|png|svg)$/.test(name),
      ).sort();
      const paths = files.map(name => `/${name}`);
      const digest = createHash("sha256").update(JSON.stringify(paths));
      for (const content of await Promise.all(files.map(name => readFile(resolve(dist, name))))) digest.update(content);
      const version = digest.digest("hex").slice(0, 16);
      await writeFile(resolve(dist, "sw.js"), `/* Generated public app-shell cache. Never caches auth, API, or arbitrary responses. */
const CACHE = "nibomo-shell-${version}";
const FILES = ${JSON.stringify(paths)};
const STATIC = new Set(FILES);
self.addEventListener("install", event => {
  event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(FILES.map(path => new Request(path, {credentials:"omit",cache:"reload"})))));
});
self.addEventListener("activate", event => {
  event.waitUntil(caches.keys().then(keys => Promise.all(keys.filter(key => key.startsWith("nibomo-shell-") && key !== CACHE).map(key => caches.delete(key)))).then(() => self.clients.claim()));
});
self.addEventListener("fetch", event => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin || request.headers.has("authorization")) return;
  if (["v1","api","login","enroll","logout","logout-local","token","authorize","register","userinfo",".well-known"].includes(url.pathname.split("/")[1])) return;
  // All navigations receive the same public SPA shell, never a personalized response.
  if (request.mode === "navigate") {
    event.respondWith(fetch(new Request("/index.html", {credentials:"omit",cache:"no-store"})).then(response => {
      if (!response.ok) throw new Error("Shell unavailable");
      return response;
    }).catch(() => caches.open(CACHE).then(cache => cache.match("/index.html"))));
    return;
  }
  if (url.search || !STATIC.has(url.pathname)) return;
  event.respondWith(caches.open(CACHE).then(async cache => (await cache.match(url.pathname)) || fetch(new Request(url.pathname, {credentials:"omit"}))));
});
`);
    },
  };
}
