# recall on Isis (k3s)

The manifests live in the `xinutec/pippijn` monorepo at `code/kubes/recall/k8s/`,
rendered from `dhall/apps/recall.dhall`; this repo keeps no copy (a stale one
here once lacked the SSO settings). Deploy with `kubes/deploy.sh recall`; nothing
auto-applies. The image is `xinutec/recall:latest`, built and smoke-tested by CI
(`.github/workflows/build.yml`) on push to `main`.

One container runs `recalld` on one port: the browser, the phones' ingest and
the Mac's sync all come through isis's front door as `recall.xinutec.org`,
which answers on the WireGuard address only. No ML; the Mac does ASR and
diarization. The PVC holds the SQLite databases and audio under `/data`.
Network policy denies egress except DNS and the Nextcloud sign-in.

## Secrets (`recall-secret`)

The env vars recalld reads (`grep -rn 'env::var' recalld/src`):

- `RECALL_SYNC_TOKEN`: the Mac's bearer token for `/sync/*`.
- `RECALLD_INGEST_TOKENS`: per-source tokens for segment delivery.
- `RECALL_DEVICE_TOKEN`: what a phone presents instead of a sign-in cookie.
- `RECALLD_READ_TOKEN`: gates the read side.
- Web sign-in, all or none: `NC_CLIENT_ID`, `NC_CLIENT_SECRET` (an OAuth client
  on dash.xinutec.org, redirect `https://recall.xinutec.org/auth/callback`) and
  `RECALL_SESSION_SECRET`. `RECALL_ALLOWED_USERS` limits who may enter. The
  device paths are exempt (`webauth::DEVICE_EXEMPT`).

## Facts

- Backed up nightly by odin: a `.backup` of the databases and an rsync of the
  audio (`xinutec-infra/backups.md`).
- Isis's disk is not encrypted; deferred on 2026-07-11.
