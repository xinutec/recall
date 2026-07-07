#!/usr/bin/env bash
#
# The single source of truth for "is this change good?".
#
# Run it three ways, identically — so local-green and CI-green can never diverge:
#   - by hand:     nix develop -c scripts/verify.sh
#   - pre-commit:    scripts/githooks/pre-commit calls it (see scripts/setup-hooks.sh)
#   - CI:          run the same line once a remote exists
#
# Each step owns a distinct error class; they run cheapest-first so a fast failure
# (formatting, a type error) doesn't wait behind the slow frontend build.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() { printf '\n\033[1m=== %s ===\033[0m\n' "$*"; }

step "check-pii (no personal terms in tracked files)"
# The denylist lives in the encrypted data root, never in the repo. See the script.
scripts/check-pii.sh

step "ruff check (lint)"
ruff check

step "ruff format --check (formatting)"
ruff format --check

step "swift-format lint --strict (ios formatting)"
# The Swift counterpart of `ruff format --check`, for the iOS app. swift-format ships
# with Xcode, not Nix — and Nix retargets DEVELOPER_DIR to its own SDK, so clear it to
# resolve the real toolchain. Skipped (loudly) where Xcode is absent, e.g. a future CI.
swift_format="$(env -u DEVELOPER_DIR /usr/bin/xcrun --find swift-format 2>/dev/null || true)"
if [ -n "$swift_format" ]; then
  "$swift_format" lint --strict --recursive --configuration ios/.swift-format ios/Sources ios/Tests
else
  echo "  swift-format not found (no Xcode) — Swift formatting NOT checked"
fi

step "mypy --strict (types, real third-party types from .venv)"
mypy

step "venv matches requirements.lock (the ML runtime is reconstructible)"
# The venv holds the runtime the agents actually run on; requirements.lock is its
# committed pin set. Drift either way (ad-hoc install, stale lock) fails here.
# After an intentional upgrade, regenerate the lock (see its header) and commit.
diff <(uv pip freeze --python .venv/bin/python) <(grep -v "^#" requirements.lock) \
  || { echo "venv and requirements.lock disagree (see diff above)" >&2; exit 1; }

step "dev-lint (custom static-analysis rules)"
# Strict (no baseline): the bare-dict-route debt was cleared via TypedDicts, so
# any new violation fails. Re-introduce --baseline when a new rule lands with debt.
# Pinning ?rev= to HEAD builds dev-lint's COMMITTED state — current, but never
# the dirty worktree, so in-flight edits in that repo can't break this one's gate.
dev_lint_rev=$(git -C "$HOME/Code/dev-lint" rev-parse HEAD)
nix run "git+file://$HOME/Code/dev-lint?rev=$dev_lint_rev" -- .

step "contract: frontend models.ts is generated from the backend API shapes"
# Fails if models.ts has drifted from src/recall/schemas.py (responses) or
# src/recall/api_models.py (request bodies). Regenerate with
# `.venv/bin/python scripts/gen_models.py --write`. Makes the cross-boundary
# contract a build error, not a convention. (.venv python: it imports pydantic.)
.venv/bin/python scripts/gen_models.py --check

step "capture-agent import surface (devshell python, no ML deps)"
# recall-capture/-ingest run `python -m recall` on the DEVSHELL interpreter, which
# has no ML deps — every ML import reachable from the CLI must stay lazy. pytest
# can't catch a new top-level ML import (it runs on the fully-stocked .venv), but
# it would crash-loop the capture agent, the one process that must never die.
# Import the CLI on the exact interpreter the agents use.
python -c "import recall.cli"

step "pytest (backend, via the uv .venv that holds the ML deps)"
# Plain `pytest` is the nix interpreter and can't import fastapi/numpy/pyannote;
# those live in the uv-managed .venv (the same interpreter mypy resolves from).
.venv/bin/python -m pytest

step "frontend: eslint (type-aware)"
( cd frontend && npm run lint )

step "frontend: build (Angular strict templates)"
# Two deliberate differences from a plain `npm run build`:
#  - a scratch --output-path, so verify can never clobber the served bundle in
#    dist/recall-web (deploying is recall-build-frontend.sh's job, not verify's);
#  - success judged by the artifact, not the exit code: a headless build on this Mac
#    can abort in the CLI's teardown (kqueue.c:279) AFTER the bundle is fully written.
#    A real compile error produces no usable bundle, so nothing is masked.
verify_build=frontend/dist/.verify-build
rm -rf "$verify_build"
( cd frontend && npm run build -- --output-path=dist/.verify-build ) || true
verify_main=$(grep -oE 'main-[A-Za-z0-9]+\.js' "$verify_build/browser/index.html" 2>/dev/null | head -1 || true)
if [[ -z "$verify_main" || ! -s "$verify_build/browser/$verify_main" ]]; then
  echo "frontend build produced no usable bundle — see errors above" >&2
  exit 1
fi

step "frontend: layout harness (playwright, phone-width e2e @ @xinutec/ui-harness)"
# Reuse the scratch build above (no second `ng build`): serve.mjs serves it and the
# specs mock every /api call. Re-sync public/ static assets first — the kqueue
# teardown abort can truncate the verbatim public/** copy (a dropped Material Icons
# woff2 would fail the icon-font check with ligature text). serve.mjs/playwright are
# plain node, so this run doesn't trip the ng-cli teardown crash.
cp -R frontend/public/. "$verify_build/browser/"
( cd frontend && RECALL_E2E_DIST=dist/.verify-build/browser npm run e2e )
rm -rf "$verify_build"

step "frontend: unit tests (vitest, jsdom)"
( cd frontend && npm test -- --watch=false )

printf '\n\033[1;32mALL GREEN\033[0m — verified\n'
