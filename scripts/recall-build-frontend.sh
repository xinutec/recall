#!/usr/bin/env bash
# Build the Angular app into frontend/dist/recall-web/browser, which the FastAPI
# `api` command serves. Run after any frontend change, then the service picks up
# the new bundle on its next request (no restart needed — files are read live).
#
# Two hazards this guards against:
#
#  1. A silently broken build. The Angular build can crash mid-write and leave
#     zero-byte index.html/JS that *look* deployed but serve a blank page. So we
#     build into a staging dir, verify index.html is non-empty, and only then swap
#     it into place — a bad attempt never replaces the last good bundle.
#
#  2. A libuv/kqueue abort (`Abort trap: 6`, kqueue.c:279) that hits this Mac when the
#     build is spawned non-interactively (no controlling TTY, e.g. from an agent/CI).
#     It fires in the CLI's teardown *after* the bundle is fully written, so the output
#     is valid despite the non-zero exit — we therefore judge success by the staged
#     artifact (staged_ok), not the exit code. The retry loop is a secondary net for a
#     genuinely-incomplete build; override its count with RECALL_BUILD_ATTEMPTS. In a
#     real terminal it exits cleanly first try; both guards are harmless there.
set -euo pipefail

# shellcheck disable=SC1091
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

# Decline the Angular CLI's first-run analytics-consent prompt so it never blocks a
# headless build: with a TTY but no one to answer, the prompt aborts with
# ExitPromptError (exit 127). (Note: this is NOT the intermittent kqueue abort above
# — that still occurs with this set; the retry loop is what handles that one.)
export NG_CLI_ANALYTICS=false

FRONTEND=/Users/pippijn/Code/recall/frontend
DIST="$FRONTEND/dist/recall-web"           # what `recall api` serves (…/browser)
STAGE="$FRONTEND/dist/.recall-web-staging" # built here first, swapped in on success
ATTEMPTS="${RECALL_BUILD_ATTEMPTS:-6}"

cd "$FRONTEND"

# Always clear the staging dir on exit (any path, including a hard crash under
# `set -e`). On success the mv below has already consumed it, so this is a no-op.
trap 'rm -rf "$STAGE"' EXIT

# A staged build is usable if index.html is non-empty, the main bundle it points at
# exists and is non-empty, AND every file in public/ actually arrived. We validate the
# *artifact*, not the build's exit code: the kqueue abort happens in the CLI's teardown
# *after* "bundle generation complete", so the process exits non-zero even though the
# output is usually fully written and valid.
#
# The assets check is not belt-and-braces. That same abort can kill the CLI *mid-copy*
# of public/**, and it did: a build shipped with an EMPTY fonts/ directory — the
# directory created, not one font in it. index.html was fine, the JS was fine, the
# check passed, and the app deployed to the phone with every icon rendered as ligature
# text ("delete", "graphic_eq") instead of a glyph. A half-copied build must never swap
# into place; comparing the file count is what makes "half" detectable at all.
staged_ok() {
    local idx="$STAGE/browser/index.html"
    [[ -s "$idx" ]] || return 1
    local main
    main=$(grep -oE 'main-[A-Za-z0-9]+\.js' "$idx" | head -1) || true
    [[ -n "$main" && -s "$STAGE/browser/$main" ]] || return 1

    # Every file under public/ is copied verbatim into the bundle root (angular.json
    # assets). Check each one arrived and is non-empty — by name, not by count: the
    # bundle also holds hashed JS/CSS the build emits, so a count would compare two
    # different populations and be wrong in both directions.
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
    # `npm run build` (not `npx ng build`) so the `prebuild` hook stamps build-info.ts
    # with the current sha. --output-path redirects into the staging dir so the served
    # DIST is untouched until an attempt is validated. `|| true` swallows the teardown
    # crash's non-zero exit so `set -e` doesn't abort — staged_ok is the real verdict.
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

# Validated non-empty: swap the staged bundle into the served path. The old bundle
# is moved aside (not rm'd) first, so a failure between the two renames can restore
# it — the served path must never be left empty.
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
