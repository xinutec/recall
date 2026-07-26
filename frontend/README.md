# recall web

The recall front-end: an Angular 22 app (zoneless, signals, standalone) with
Angular Material 22. It talks to the FastAPI backend (`../src/recall/api.py`) over
`/api/*` and, in production, is served by that same backend on one origin.

Run everything from the repo's Nix devshell so the right Node is on PATH:

```sh
nix develop          # provides nodejs_24 (Angular 22 needs Node >= 24.15)
```

## Develop (hot reload)

```sh
cd frontend && npx ng serve
```

`proxy.conf.json` forwards `/api` to the backend on `http://localhost:8000`, so
start the API too (`../scripts/recall.sh api --out /Volumes/Backup/recall`). The
dev server binds `0.0.0.0`, so it's reachable from the phone over the LAN.

## Build (what the backend serves)

```sh
../scripts/recall-build-frontend.sh
```

Not a bare `ng build`: it runs `npm run build` (so the `prebuild` version stamp
fires), builds into a staging dir, checks `index.html` + the main bundle + every
`public/` asset landed, retries a known libuv/kqueue abort, and only then swaps into
`dist/recall-web`. Output is `dist/recall-web/browser/`, served with an SPA fallback.
In production the Dockerfile does this build — the image is what Isis serves.

## Test

```sh
npm test                                   # Vitest (via the pretest version stamp)
npx playwright test                        # e2e specs in e2e/
```

## Layout

- `src/app/features/` — one component per route: timeline (`''`), search, ask,
  review, train, labels, cleanup, sessions, session (`sessions/:id`), compare,
  compare-run (`compare/:id`) — plus the non-route `clip-trimmer` and `waveform`
- `src/app/shared/` — reusable pieces (transcript card, confirm dialog)
- `src/app/recall-api.ts` — typed client for the backend mutations
- `src/app/models.ts`, `format.ts` — API types and presentation helpers
