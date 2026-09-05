# recall's fleet image (Isis k3s): api + web + the sync ingest. NO ML — the Mac keeps
# capture, ASR, diarization, and the LLM — so this is a light FastAPI + SQLite + static
# frontend. Multi-stage: build the Angular app, then a slim python runtime with only the
# non-ML deps (empirically fastapi/uvicorn/pydantic/httpx/python-multipart) plus ffmpeg
# for the playback slicing the api does. Runs as non-root uid 1000, matching
# deploy/k8s/02-deployment.yaml.
#
# NOT YET BUILT — there's no container builder on the dev Mac; this is a staged artifact.
# Build + push `xinutec/recall:latest` from a host with docker/podman (see deploy/k8s/README).

# --- frontend build ---
FROM node:24-slim AS frontend
WORKDIR /build/frontend
# pnpm-workspace.yaml belongs in this layer, not with the sources: it carries the
# install-script allowlist, and without it neither esbuild nor the ui-harness
# unpacks — the build then fails on dependencies that look installed.
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
# git: the shared layout harness is a git dependency (github:xinutec/ui-harness),
# so the install clones it — node:slim ships no git.
#
# pnpm is taken unpinned. The host gets its copy from the flake, and pinning a
# second version here would be two numbers held level by hand; the lockfile is
# what has to match, and --frozen-lockfile fails rather than drift.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g pnpm \
    && pnpm install --frozen-lockfile
COPY frontend/ ./
RUN pnpm run build

# --- recalld build ---
# The Rust system-of-record daemon (docs/architecture.md, stage A). Built here so
# the one fleet image carries both tiers: the Python api container and the recalld
# ingest container run from the same image with different commands — one artifact
# to version, push and roll.
FROM rust:1-slim-trixie AS recalld
WORKDIR /build
# The whole Rust workspace (stage D1): cargo needs every member's manifest and
# sources to load the graph, but `-p recalld` compiles only recalld and its
# audiocore dependency — audiod rides along as text. Layer caching comes from
# buildx's registry cache rather than a dummy-source dance, which a workspace
# would make three times as fiddly for a build measured in low minutes.
COPY Cargo.toml Cargo.lock ./
COPY audiocore/ audiocore/
COPY audiod/ audiod/
COPY recalld/ recalld/
RUN cargo build --release --locked -p recalld

# --- runtime ---
# -trixie pinned explicitly: the recalld stage links against this release's glibc,
# so the two FROMs must name the same Debian rather than drift apart on a float.
FROM python:3.12-slim-trixie
# The app shells out to these; a missing one is a 500 at request time, not a boot error,
# so it hides until someone presses play. `sox` was: the image had ffmpeg only, and every
# audio request on the fleet died with FileNotFoundError deep in loudness normalisation
# while the transcripts served perfectly. `flac` decodes the older archive segments.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg sox flac \
    && rm -rf /var/lib/apt/lists/*
# Non-ML runtime deps only (see deploy/k8s/README.md), pinned to the app's floors. A
# dedicated fleet lockfile would make this reproducible — a follow-up.
#
# numpy is here and is NOT a concession on "no ML". It is arithmetic over audio: the
# envelope (RMS per 0.1s bucket), the spectrum (log-band shapes) and the per-mic
# threshold calibration. The cleanup review — the page where the household's dead air is
# actually deleted — runs on those, and the archive it deletes from now lives HERE. The
# feature has to work where the audio is. What stays on the Mac is the ML proper:
# mlx-whisper, pyannote, mlx-lm — none of which this image has, or can run.
RUN pip install --no-cache-dir \
    "fastapi>=0.136" "uvicorn>=0.49" "pydantic>=2.13" "httpx>=0.28" \
    "python-multipart>=0.0.32" "numpy>=2.1"
# uid 1000 matches the Deployment's runAsUser + fsGroup.
RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin recall
WORKDIR /app
COPY src/ /app/src/
COPY --from=frontend /build/frontend/dist /app/frontend/dist
COPY --from=recalld /build/target/release/recalld /usr/local/bin/recalld
# _REPO in recall.api is three parents up from src/recall/api.py, i.e. /app — so the
# frontend resolves at /app/frontend/dist/recall-web/browser and PYTHONPATH is /app/src.
RUN mkdir -p /app/logs && chown -R 1000:1000 /app
ENV PYTHONPATH=/app/src
USER 1000
EXPOSE 8000
# --out binds the data root; `recall api` overwrites RECALL_OUT from it, so pass the flag
# (a bare RECALL_OUT env would be ignored). The k8s Deployment mounts the PVC at /data.
CMD ["python", "-m", "recall", "api", "--out", "/data", "--host", "0.0.0.0", "--port", "8000"]
