#!/usr/bin/env bash
# Generate the frontend's API types from recalld's structs via ts-rs.
#
#   nix develop --command scripts/gen-types.sh            # regenerate + install
#   nix develop --command scripts/gen-types.sh --check    # report drift, write nothing
#
# The mechanics — generate into scratch, install only on success, compare by
# content — are dev-lint#gen-types, shared with the other repositories. The
# export tests are named export_bindings_*, so the filter runs generation only.
set -euo pipefail
cd "$(dirname "$0")/.."
exec nix run "git+file:../dev-lint?ref=HEAD#gen-types" -- "$@" \
  --out frontend/src/app/generated \
  -- cargo test -p recalld export_bindings
