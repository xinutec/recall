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
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# --- runtime ---
FROM python:3.12-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg \
    && rm -rf /var/lib/apt/lists/*
# Non-ML runtime deps only (see deploy/k8s/README.md), pinned to the app's floors. A
# dedicated fleet lockfile would make this reproducible — a follow-up.
RUN pip install --no-cache-dir \
    "fastapi>=0.136" "uvicorn>=0.49" "pydantic>=2.13" "httpx>=0.28" \
    "python-multipart>=0.0.32"
# uid 1000 matches the Deployment's runAsUser + fsGroup.
RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin recall
WORKDIR /app
COPY src/ /app/src/
COPY --from=frontend /build/frontend/dist /app/frontend/dist
# _REPO in recall.api is three parents up from src/recall/api.py, i.e. /app — so the
# frontend resolves at /app/frontend/dist/recall-web/browser and PYTHONPATH is /app/src.
RUN mkdir -p /app/logs && chown -R 1000:1000 /app
ENV PYTHONPATH=/app/src
USER 1000
EXPOSE 8000
# --out binds the data root; `recall api` overwrites RECALL_OUT from it, so pass the flag
# (a bare RECALL_OUT env would be ignored). The k8s Deployment mounts the PVC at /data.
CMD ["python", "-m", "recall", "api", "--out", "/data", "--host", "0.0.0.0", "--port", "8000"]
