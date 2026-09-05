# recall on Isis (k3s) — deployment notes

Status: **LIVE since 2026-07-17.** The manifests no longer live here — they are in the
`pippijn` monorepo at `code/kubes/recall/k8s/`, rendered from the typed Dhall model
(`dhall/apps/recall.dhall`). This file is kept for the setup and rationale below
(secrets, WireGuard exposure, backup), not as a source of manifests.

They were duplicated in both places until 2026-07-26, and the copy here silently went
stale: it was missing the six web-SSO env vars and the nextcloud egress rule that the
live cluster had been running for nine days. A `kubectl apply` from this directory would
have deleted working single sign-on. That is why there is now exactly one copy.

⚠ **Deploy with `kubes/deploy.sh recall`**, or equivalently `recall/k8s/sync.sh`,
which is a three-line wrapper that `exec`s it. `scripts/apply.sh` was named here
until 2026-08-23 and was **deleted 2026-08-16** — `plan-run deploy` replaced it.
The cluster comes from the model (`dhall/clusters.json`: `recall: isis.xinutec.org`),
so it is never passed by hand.

Nothing auto-applies anywhere — the fleet does NOT run Flux; every app is deployed by
hand. Rationale and topology: `docs/isis-migration.md`.

## What runs here

The **fleet tier only**, two containers from one image since 2026-09-05: the
FastAPI api + web + sync ingest, and `recalld` — the Rust ingest plane of
[architecture.md](../../docs/architecture.md), wg hostPort 8001, gated by the
`INGEST_TOKENS` key in `recall-secret`. No ML — the Mac keeps capture, ASR,
diarization, and the LLM. So the image needs the non-ML
subset of recall's deps (fastapi, pydantic, httpx, sqlite, uvicorn) plus the built Angular
frontend, and runs `recall api`.

Manifests (in `pippijn:code/kubes/recall/k8s/`): `00-namespace`, `01-pvc` (the SQLite DB +
audio under `/data`), `02-deployment` (hardened: non-root uid 1000, dropped caps,
read-only rootfs + `/tmp` emptyDir, seccomp, probes, limits), `03-service` (ClusterIP),
`04-networkpolicy` (default-deny egress bar DNS + the nextcloud SSO exchange).

## Steps to actually deploy (each is the host-touching part)

1. **Image** — DONE. Built and pushed by CI (`.github/workflows/build.yml`, like every
   other app) on push to `main`: `xinutec/recall:latest` is on Docker Hub. Nobody builds
   it locally. It runs as uid 1000 via `python -m recall api`; the non-ML dep set is
   validated (`recall.api`/`recall.sync` import with only fastapi/uvicorn/pydantic/httpx/
   python-multipart).
2. **Encryption at rest — DEFERRED (future action item, 2026-07-11).** Isis is a single
   unencrypted ext4 disk (no spare partition), so encryption would be a LUKS file-container
   mounted at recall's storage path (nixos-config + activation). Deferred by decision to
   get the pipeline working first — so until this is done, recall's household/medical audio
   sits on **plaintext disk** on Isis. Revisit before treating the split as production-grade.
3. **Secret** — create `recall-secret` in the `recall` namespace with `SYNC_TOKEN` (the
   Mac presents it as a bearer token). Source it from agenix/Vaultwarden, never committed.
   Without it the sync routes don't register (the app stays a plain LAN web UI).

   **Web-UI SSO (optional, additive).** To gate the human web UI behind a Nextcloud
   sign-in, add three more keys to the same `recall-secret`: `NC_CLIENT_ID` +
   `NC_CLIENT_SECRET` (from an OAuth 2.0 client registered on **dash.xinutec.org →
   Settings → Security → OAuth 2.0 clients**, redirect URI
   `http://10.100.0.2:8000/auth/callback`), and `SESSION_SECRET` (a random cookie-signing
   key, e.g. `openssl rand -hex 32`). All three raise the gate; missing any of them leaves
   the UI open. `RECALL_ALLOWED_USERS` (plain env in the Deployment, default `pippijn`)
   restricts who may enter after a valid sign-in. The recording plane stays login-free:
   `/sync/*` keeps its bearer token, and the iOS mic app's capture endpoints
   (`/api/capture`, `/api/sources`, `/api/capture/pause|resume`) are exempt — a headless
   device can't do an interactive OAuth login. See `docs/isis-migration.md`.
4. **WireGuard exposure** — do NOT add an nginx Ingress. Expose the Service over WireGuard
   only: a MetalLB address from a `wg0`-only pool, or a NodePort firewalled to `wg0`. That
   is the real network gate; the public ingress is not one.
5. ~~**Move the manifests to `kubes/recall/k8s/`** and add a `sync.sh`~~ — **DONE.** They
   live at `pippijn:code/kubes/recall/k8s/` with `sync.sh` + `secret.sh` (the `sync.sh`
   is now a wrapper on `kubes/deploy.sh`, not its own copy of the procedure). The copies that
   used to sit here were deleted 2026-07-26 (see the status note at the top). The Mac
   worker pushes (`recall sync`) at the Isis WG address.
6. **Backup** — add a recall block to odin `backup-prepare.sh`: a consistent
   `sqlite3 .backup` of `/data/recall.sqlite` on the PVC host path + the audio dir (NOT
   the MariaDB-dump shape). Verify a restore before trusting it. Then recall rides the
   standard odin-restic + Mac restic-copy — the bespoke `recall-backup.sh` retires.

Until step 5, production is unchanged: the Mac still runs everything and the LAN web UI is
untouched (`recall.sync` is inert without the token).
