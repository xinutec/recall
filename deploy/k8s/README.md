# recall on Isis (k3s) — deployment (STAGED, not applied)

Status: **proposal, 2026-07-11.** These manifests are staged here — NOT in the
Flux-watched `kubes` repo — so nothing auto-applies. Applying them (and the host changes
below) is the deliberate, fragile-host step. Rationale and topology: `docs/isis-migration.md`.

## What runs here

The **fleet tier only**: FastAPI api + web + the sync ingest, in one light container. No
ML — the Mac keeps capture, ASR, diarization, and the LLM. So the image needs the non-ML
subset of recall's deps (fastapi, pydantic, httpx, sqlite, uvicorn) plus the built Angular
frontend, and runs `recall api`.

Manifests: `00-namespace`, `01-pvc` (the SQLite DB + audio under `/data`), `02-deployment`
(hardened: non-root uid 1000, dropped caps, read-only rootfs + `/tmp` emptyDir, seccomp,
probes, limits), `03-service` (ClusterIP).

## Steps to actually deploy (each is the host-touching part)

1. **Image** — the Dockerfile exists at the repo root (staged, not built — no container
   builder on the dev Mac). Its non-ML dep set is validated (`recall.api`/`recall.sync`
   import with only fastapi/uvicorn/pydantic/httpx/python-multipart). Build and push
   `xinutec/recall:latest` from a host with docker/podman (same registry convention as
   `xinutec/health-sync`); it runs as uid 1000 via `python -m recall api`.
2. **Encryption at rest** — Isis's disk is unencrypted today. Encrypt the k3s storage path
   (`/var/lib/rancher/k3s/storage`, LUKS) or mount an encrypted volume there BEFORE the
   audio PVC binds. Household/medical audio must not sit on plaintext disk. (nixos-config.)
3. **Secret** — create `recall-secret` in the `recall` namespace with `SYNC_TOKEN` (the
   Mac presents it as a bearer token). Source it from agenix/Vaultwarden, never committed.
   Without it the sync routes don't register (the app stays a plain LAN web UI).
4. **WireGuard exposure** — do NOT add an nginx Ingress. Expose the Service over WireGuard
   only: a MetalLB address from a `wg0`-only pool, or a NodePort firewalled to `wg0`. That
   is the real network gate; the public ingress is not one.
5. **Move the manifests to `kubes/recall/k8s/`** so Flux applies them, and cut the Mac
   worker over to push (`recall.sync.SyncClient`) at the Isis WG address.
6. **Backup** — add a recall block to odin `backup-prepare.sh`: a consistent
   `sqlite3 .backup` of `/data/recall.sqlite` on the PVC host path + the audio dir (NOT
   the MariaDB-dump shape). Verify a restore before trusting it. Then recall rides the
   standard odin-restic + Mac restic-copy — the bespoke `recall-backup.sh` retires.

Until step 5, production is unchanged: the Mac still runs everything and the LAN web UI is
untouched (`recall.sync` is inert without the token).
