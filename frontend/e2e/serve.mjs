// Minimal static server for the production bundle, used by the Playwright layout
// harness. Serves dist/recall-web/browser with SPA fallback to index.html and the
// correct content types. The API is mocked per-test via page.route; this only
// needs to serve the static app. No deps; Node stdlib only.
//
// Serving the BUILT bundle (not `ng serve`) keeps the harness hermetic and dodges
// the macOS kqueue.c:279 abort that spawning the Angular CLI dev server trips.
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

// Which built bundle to serve. Defaults to the normal `ng build` output; verify.sh
// points it at its scratch build (RECALL_E2E_DIST) so the harness reuses that one
// build instead of producing a second.
const FRONTEND = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const ROOT = process.env.RECALL_E2E_DIST
  ? join(FRONTEND, process.env.RECALL_E2E_DIST)
  : join(FRONTEND, "dist", "recall-web", "browser");
const PORT = Number(process.argv[2] ?? 4293);

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".webmanifest": "application/manifest+json; charset=utf-8",
  ".ico": "image/x-icon",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
};

async function fileFor(urlPath) {
  const clean = normalize(decodeURIComponent(urlPath.split("?")[0])).replace(/^(\.\.[/\\])+/, "");
  const candidate = join(ROOT, clean);
  try {
    if ((await stat(candidate)).isFile()) return candidate;
  } catch {
    /* fall through to SPA fallback */
  }
  return join(ROOT, "index.html");
}

createServer(async (req, res) => {
  const path = (req.url ?? "/").split("?")[0];
  // API is mocked in-test via page.route; anything reaching here answers empty.
  if (path.startsWith("/api/")) {
    res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
    res.end("[]");
    return;
  }
  const file = await fileFor(req.url ?? "/");
  try {
    const body = await readFile(file);
    res.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
}).listen(PORT, () => console.log(`serving ${ROOT} on http://localhost:${PORT}`));
