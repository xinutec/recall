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
../scripts/recall-build-frontend.sh        # = npx ng build
```

Output goes to `dist/recall-web/browser/`, which `recall api` serves with an SPA
fallback. The running service picks up a new build on its next request — no
restart needed.

## Test

```sh
npx ng test --watch=false                  # Vitest
```

## Layout

- `src/app/features/` — one component per route (timeline, home, search, review, train, record, labels, sessions, session)
- `src/app/shared/` — reusable pieces (transcript card)
- `src/app/recall-api.ts` — typed client for the backend mutations
- `src/app/audio/wav-recorder.ts` — lossless AudioWorklet PCM → WAV recorder
- `src/app/models.ts`, `format.ts` — API types and presentation helpers
