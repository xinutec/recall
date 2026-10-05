#!/usr/bin/env bash
# Build the Angular app into frontend/dist/recall-web/browser, the path
# `recalld --frontend` serves in the image.
#
# Builds into a staging dir and swaps it in only when complete, since a crashed
# build can leave empty files. On this Mac a non-interactive build can abort in
# the CLI's teardown (libuv kqueue, `Abort trap: 6`), usually after the bundle is
# written: success is judged by the staged files, not the exit code, with
# RECALL_BUILD_ATTEMPTS retries for an incomplete one.
set -euo pipefail

# shellcheck disable=SC1091 # the nix profile, absent where nix is not installed
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

# Decline the Angular CLI's analytics prompt, which aborts a headless build with
# ExitPromptError. Unrelated to the kqueue abort.
export NG_CLI_ANALYTICS=false

FRONTEND="$(cd "$(dirname "$0")/.." && pwd)/frontend"
DIST="$FRONTEND/dist/recall-web"           # recalld serves …/browser
STAGE="$FRONTEND/dist/.recall-web-staging" # built here first, swapped in on success
ATTEMPTS="${RECALL_BUILD_ATTEMPTS:-6}"

cd "$FRONTEND"

# On success the mv below has already consumed it.
trap 'rm -rf "$STAGE"' EXIT

# Usable when index.html, the main bundle it names, and every file of public/ are
# present and non-empty. The abort once struck mid-copy of public/: a build
# shipped with an empty fonts/, and every icon showed as its ligature name.
staged_ok() {
    local idx="$STAGE/browser/index.html"
    [[ -s "$idx" ]] || return 1
    local main
    main=$(grep -oE 'main-[A-Za-z0-9]+\.js' "$idx" | head -1) || true
    [[ -n "$main" && -s "$STAGE/browser/$main" ]] || return 1

    # public/ is copied into the bundle root (angular.json assets). By name, not by
    # count: the bundle also holds the emitted JS and CSS.
    local missing=0 rel
    while IFS= read -r rel; do
        [[ -s "$STAGE/browser/$rel" ]] || {
            echo "recall-build-frontend: asset missing from the staged build: $rel" >&2
            missing=1
        }
    done < <(cd "$FRONTEND/public" && find . -type f | sed 's|^\./||')
    ((missing == 0))
}

built=""
for ((attempt = 1; attempt <= ATTEMPTS; attempt++)); do
    rm -rf "$STAGE"
    echo "recall-build-frontend: build attempt ${attempt}/${ATTEMPTS}..."
    # `run build`, not `ng build`, so the `prebuild` hook stamps build-info.ts.
    # `|| true`: staged_ok is the verdict, not the exit code.
    nix develop ..#default --command npm run build -- --output-path="$STAGE" "$@" || true
    if staged_ok; then
        built=1
        break
    fi
    echo "recall-build-frontend: attempt $attempt produced no usable bundle - retrying" >&2
done

if [[ -z "$built" ]]; then
    echo "recall-build-frontend: build failed after $ATTEMPTS attempts." >&2
    echo "  This Mac hits an intermittent libuv/kqueue abort on non-interactive spawn." >&2
    echo "  Just re-run this script, or run it in a real terminal (reliable there)." >&2
    echo "  The live bundle in $DIST is untouched." >&2
    exit 1
fi

# The old bundle is moved aside, not removed, so a failed swap can restore it.
OLD="$FRONTEND/dist/.recall-web-old"
rm -rf "$OLD"
if [[ -d "$DIST" ]]; then mv "$DIST" "$OLD"; fi
if ! mv "$STAGE" "$DIST"; then
    [[ -d "$OLD" ]] && mv "$OLD" "$DIST"
    echo "recall-build-frontend: swap failed — previous bundle restored" >&2
    exit 1
fi
rm -rf "$OLD"
echo "recall-build-frontend: deployed $(wc -c <"$DIST/browser/index.html") byte index.html from $DIST/browser (attempt $attempt)"
