# recall's fleet image (Isis k3s): the browsing API, the web app and the device ingest,
# all served by `recalld`. NO ML — the Mac keeps capture, ASR, diarization and the LLM.
#
# ⚠ **No Python, and no interpreter.** Until 2026-09-12 this was a python:3.12 base
# carrying FastAPI, uvicorn and numpy so `recall.api` could serve the fleet tier; the
# port to recalld finished and the Deployment stopped naming Python months before the
# image did. What is left is a Debian base, one static-ish binary, and the media tools
# recalld shells out to — so the fleet dependency set is now the Rust lockfile and
# `apt` line below, nothing else.
#
# Multi-stage: build the Angular app, build recalld, then assemble. Runs as non-root
# uid 1000, matching the Deployment's runAsUser + fsGroup.
#
# Built and pushed by .github/workflows/build.yml — there's no container builder on the
# dev Mac.

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
# The Rust system-of-record daemon (docs/architecture.md, stage A) — the only
# program this image exists to run. One binary binds both planes, so the pod is
# one container and the image is one artifact to version, push and roll.
FROM rust:1-slim-trixie AS recalld
WORKDIR /build
# The whole Rust workspace (stage D1): cargo needs every member's manifest and
# sources to load the graph, but `-p recalld` compiles only recalld and its
# audiocore dependency — audiod rides along as text. Layer caching comes from
# buildx's registry cache rather than a dummy-source dance, which a workspace
# would make three times as fiddly for a build measured in low minutes.
# ⚠ EVERY workspace member must be copied, even ones this image never runs:
# cargo loads the whole graph before it compiles anything, so a missing crate
# fails with "failed to load manifest for workspace member". The member list
# lives in THREE places — Cargo.toml, flake.nix's fileset, and here — and the
# two build ones fail only in CI and the nix sandbox, never on a laptop.
COPY Cargo.toml Cargo.lock ./
# ⚠ EVERY workspace MEMBER, or cargo cannot even load the graph — it fails with
# a bare "No such file or directory" naming nothing. Only recalld is built here,
# which makes it tempting to copy only what it needs; adding a crate to
# Cargo.toml and not to this list broke four consecutive image builds on
# 2026-09-08 before anyone looked, because the commit gate does not build this
# image. flake.nix carries the same list and the same warning.
COPY audiocore/ audiocore/
COPY audiod/ audiod/
COPY doctor/ doctor/
COPY recalld/ recalld/
COPY runner/ runner/
RUN cargo build --release --locked -p recalld

# --- runtime ---
# -trixie pinned explicitly: the recalld stage links against this release's glibc,
# so the two FROMs must name the same Debian rather than drift apart on a float.
FROM debian:trixie-slim
# The app shells out to these; a missing one is a 500 at request time, not a boot error,
# so it hides until someone presses play. `sox` was: the image had ffmpeg only, and every
# audio request on the fleet died with FileNotFoundError deep in loudness normalisation
# while the transcripts served perfectly. `flac` decodes the older archive segments.
# libonnxruntime1.21: recalld's VAD (stage D4) DLOPENS this rather than bundling
# a runtime. ort's prebuilt binaries require AVX2 and the fleet's servers are Ivy
# Bridge (2012) — isis crash-looped with SIGILL on them. Debian's build targets
# baseline x86-64 and runs there (verified by executing silero through it on
# amun's identical CPU). ORT_DYLIB_PATH below names it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg sox flac libonnxruntime1.21 \
    && rm -rf /var/lib/apt/lists/*
# deep-filter denoises playback clips on demand (audio.rs, `enhance=true`) — the
# #1522 listen-test winner. The release binary is static musl with tract
# inference (pure Rust): no AVX2, which matters because the fleet is Ivy Bridge
# and ort's prebuilt binaries already SIGILLed here once (above). Verified on
# isis 2026-09-10: runs, 10 s of speech in 3.5 s, 57 MB peak.
ADD --checksum=sha256:70775e251eee44c0f2451a1e833326cf8bcbbe304d3e7cd12851e6fce72ef7da \
    --chmod=755 \
    https://github.com/Rikorose/DeepFilterNet/releases/download/v0.5.6/deep-filter-0.5.6-x86_64-unknown-linux-musl \
    /usr/local/bin/deep-filter
# uid 1000 matches the Deployment's runAsUser + fsGroup.
# Pinned to the versioned soname on purpose: an unversioned symlink would let an
# apt upgrade swap the ABI under a running image, and ort asks for API 21.
ENV ORT_DYLIB_PATH=/usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.21

RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin recall
WORKDIR /app
COPY --from=frontend /build/frontend/dist /app/frontend/dist
COPY --from=recalld /build/target/release/recalld /usr/local/bin/recalld
RUN mkdir -p /app/logs && chown -R 1000:1000 /app
USER 1000
EXPOSE 8000 8001
# The Deployment passes its own command (kubes/dhall/apps/recall.dhall) — this is the
# shape it passes, kept here so `docker run` on the image is the same program the fleet
# runs rather than a bare shell. `--root` binds the PVC mount; both ports are bound in
# one process (the browsing plane and the device ingest plane).
CMD ["recalld", "--root", "/data", \
     "--bind", "0.0.0.0:8000", "--bind", "0.0.0.0:8001", \
     "--frontend", "/app/frontend/dist/recall-web/browser"]
