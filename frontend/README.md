# recall web

Angular 22 (zoneless, signals, standalone) with Angular Material, served by
recalld (`--frontend`) on the same origin as `/api`. Run from `nix develop`.

```sh
pnpm start              # dev server; proxy.conf.json sends /api to localhost:8000
pnpm run lint
pnpm exec ng test --watch=false
pnpm run e2e            # Playwright, e2e/
../scripts/recall-build-frontend.sh    # the image's build, into dist/recall-web/browser
```

## Layout

- `src/app/features/`: one component per route (`app.routes.ts`): timeline,
  search, check, labels, sessions, session
- `src/app/shared/`: the line sheet, player, confirm dialog
- `src/app/generated/`: API types from recalld's structs (`scripts/gen-types.sh`)
- `recall-api.ts`: the client for mutations; `models.ts`, `format.ts`: types and
  display helpers
