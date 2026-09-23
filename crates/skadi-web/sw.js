// Skadi app-shell service worker (SKADI-T-0333 / I-0048 offline player).
//
// Goal: the app OPENS with zero network after one online visit.
// - /api/*: never touched — the app handles offline itself (OPFS books).
// - navigations: network-first, and ONLY a successful (2xx) response updates
//   the cached shell — a transient 500/auth page must never become the
//   offline app (review pass 2, web finding 6).
// - hashed js/wasm/css: cache-first (content-hashed, so a hit is always
//   correct). Unhashed copies (icons, manifest): network-first so an updated
//   asset actually reaches installed clients (finding 7).
// - opaque cross-origin (fonts/covers): NOT runtime-cached — Chrome charges
//   ~7MB padded quota per opaque entry, stealing space from OPFS books
//   (finding 11).
//
// Bump CACHE on any shell change: activate() deletes every other cache name,
// so old hashed wasm/js don't accumulate forever against the device quota
// (finding 7).
const CACHE = "skadi-shell-v2";
const UNHASHED = ["/manifest.webmanifest", "/icon-192.png", "/icon-512.png"];

self.addEventListener("install", () => self.skipWaiting());

self.addEventListener("activate", (e) =>
  e.waitUntil(
    (async () => {
      const names = await caches.keys();
      await Promise.all(names.filter((n) => n !== CACHE).map((n) => caches.delete(n)));
      await self.clients.claim();
    })()
  )
);

self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  const sameOrigin = url.origin === self.location.origin;

  // The app owns API offline-ness (OPFS); never intercept.
  if (sameOrigin && url.pathname.startsWith("/api/")) return;

  // Navigations: network-first, cache only 2xx as the shell.
  if (req.mode === "navigate") {
    e.respondWith(
      fetch(req)
        .then((r) => {
          if (r.ok) {
            const copy = r.clone();
            caches.open(CACHE).then((c) => c.put("/", copy));
          }
          return r;
        })
        .catch(() => caches.match("/"))
    );
    return;
  }

  // Unhashed same-origin assets: network-first (updates reach clients),
  // fall back to cache offline.
  if (sameOrigin && UNHASHED.includes(url.pathname)) {
    e.respondWith(
      fetch(req)
        .then((r) => {
          if (r.ok) {
            const copy = r.clone();
            caches.open(CACHE).then((c) => c.put(req, copy));
          }
          return r;
        })
        .catch(() => caches.match(req))
    );
    return;
  }

  // Cross-origin (fonts/covers): straight to network, no runtime cache.
  if (!sameOrigin) return;

  // Same-origin hashed assets: cache-first with fill (only 2xx).
  e.respondWith(
    caches.match(req).then(
      (hit) =>
        hit ||
        fetch(req).then((r) => {
          if (r.ok) {
            const copy = r.clone();
            caches.open(CACHE).then((c) => c.put(req, copy));
          }
          return r;
        })
    )
  );
});
