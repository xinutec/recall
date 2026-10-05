# recall's fleet image (Isis k3s): `recalld`, serving the API, the web app and the
# device ingest. No ML and no interpreter: the Mac keeps capture and the models, so the
# dependencies are the Rust lockfile and the `apt` line below.
#
# Builds the Angular app, builds recalld, then assembles; runs as uid 1000, matching
# the Deployment's runAsUser and fsGroup. Built by .github/workflows/build.yml.

# --- frontend build ---
FROM node:24-slim AS frontend
WORKDIR /build/frontend
# pnpm-workspace.yaml has the install-script allowlist; without it esbuild and the
# ui-harness do not unpack.
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
# git: the ui-harness is a git dependency, and node:slim has none.
#
# pnpm unpinned: --frozen-lockfile is what holds the install to the lock.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g pnpm \
    && pnpm install --frozen-lockfile
COPY frontend/ ./
RUN pnpm run build

# --- recalld build ---
# Trixie, unlike the rest of the fleet's bookworm: Debian ships libonnxruntime only
# from trixie, and the speech detector needs Debian's build, since ort's prebuilt
# one needs AVX2, which isis (Ivy Bridge) lacks (#1629).
FROM rust:1.98-slim-trixie AS recalld
WORKDIR /build
# Every workspace member, though only recalld is built: cargo loads the whole graph
# first. The list is also in Cargo.toml and flake.nix;
# scripts/check_workspace_members.py keeps them equal. Layers are cached by buildx's
# registry cache.
COPY Cargo.toml Cargo.lock ./
COPY audiocore/ audiocore/
COPY audiod/ audiod/
COPY doctor/ doctor/
COPY recalld/ recalld/
COPY runner/ runner/
COPY transcript/ transcript/
COPY cli/ cli/
COPY experimental/ experimental/
RUN cargo build --release --locked -p recalld

# --- runtime ---
# The same Debian as the build stage, whose glibc recalld links against.
FROM debian:trixie-slim
# The tools recalld runs; a missing one fails at request time, not at boot. `flac`
# decodes older archive segments. libonnxruntime1.21 is loaded by the speech detector
# (ORT_DYLIB_PATH below); Debian's build targets baseline x86-64 and runs on the
# fleet's Ivy Bridge CPUs, where ort's prebuilt one crashed with SIGILL.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg sox flac libonnxruntime1.21 \
    && rm -rf /var/lib/apt/lists/*
# deep-filter denoises playback clips on request (audio.rs, `enhance=true`; #1522).
# The static musl release uses pure-Rust inference, so needs no AVX2.
ADD --checksum=sha256:70775e251eee44c0f2451a1e833326cf8bcbbe304d3e7cd12851e6fce72ef7da \
    --chmod=755 \
    https://github.com/Rikorose/DeepFilterNet/releases/download/v0.5.6/deep-filter-0.5.6-x86_64-unknown-linux-musl \
    /usr/local/bin/deep-filter
# The versioned soname, so an apt upgrade cannot change the ABI ort expects (21).
ENV ORT_DYLIB_PATH=/usr/lib/x86_64-linux-gnu/libonnxruntime.so.1.21

RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin recall
WORKDIR /app
COPY --from=frontend /build/frontend/dist /app/frontend/dist
COPY --from=recalld /build/target/release/recalld /usr/local/bin/recalld
RUN mkdir -p /app/logs && chown -R 1000:1000 /app
USER 1000
EXPOSE 8000
# The command the Deployment passes (kubes/dhall/apps/recall.dhall), so `docker run`
# runs the same program: one port for the browser, ingest and sync.
CMD ["recalld", "--root", "/data", \
     "--bind", "0.0.0.0:8000", \
     "--frontend", "/app/frontend/dist/recall-web/browser"]
