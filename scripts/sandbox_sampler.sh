#!/usr/bin/env bash
# Sample the NIX sandbox arm of the workspace test suite (#1480).
#
# `nix build --rebuild` and `--check` return on a cache hit without running any
# tests. So each iteration changes a tracked marker, which changes the
# derivation hash, and a run with no `test result:` line is refused.
#
# Works in a scratch clone: the marker must be `git add`ed for the flake to see
# it, which would mix an experiment into the real checkout.
#
#   scripts/sandbox_sampler.sh [iterations]     default 10
#
# Expected non-zero statuses (`grep -c` counting zero, a failing build) are
# caught with `if` or `|| true` so `set -e` does not end the loop.
set -euo pipefail

iterations="${1:-10}"
repo="$(cd "$(dirname "$0")/.." && pwd)"
scratch="${SANDBOX_SAMPLER_DIR:-$HOME/.cache/recall-sandbox-sampler}"
log="$scratch/sampler.log"

rm -rf "$scratch"
mkdir -p "$scratch"
git clone --quiet --no-hardlinks "$repo" "$scratch/recall"
cd "$scratch/recall"

# Unique per run too: an "iteration 3" marker would hit the previous run's cache.
run="$(date -u +%Y%m%dT%H%M%S)-$$"
pass=0 fail=0 noop=0
for i in $(seq 1 "$iterations"); do
    # In the crate everything depends on, so the whole workspace is re-tested;
    # the flake did not reproduce with webauth alone (0/40).
    printf '\n// sandbox sampler %s iteration %s\n' "$run" "$i" >> audiocore/src/lib.rs
    git add audiocore/src/lib.rs

    out="$scratch/run-$i.log"
    started=$SECONDS
    if nix build --no-warn-dirty --no-link -L .#sandbox-tests > "$out" 2>&1; then
        status=0
    else
        status=$?
    fi
    elapsed=$((SECONDS - started))

    # A build that ran no tests is neither a pass nor a failure.
    ran=$(grep -c 'test result:' "$out" || true)
    if [ "$ran" -eq 0 ]; then
        noop=$((noop + 1))
        verdict="NO TESTS RAN — not counted"
    elif [ "$status" -eq 0 ]; then
        pass=$((pass + 1))
        verdict="pass"
    else
        fail=$((fail + 1))
        verdict="FAIL"
        cp "$out" "$scratch/FAILURE-$i.log"
    fi
    printf '%s  iteration %2s/%s  %s  (%s suites, %ss)\n' \
        "$(date -u +%H:%M:%S)" "$i" "$iterations" "$verdict" "$ran" "$elapsed" | tee -a "$log"
done

printf '\nsandbox arm: %s pass, %s FAIL, %s no-op over %s iterations\n' \
    "$pass" "$fail" "$noop" "$iterations" | tee -a "$log"
if [ "$noop" -gt 0 ]; then
    printf '⚠ %s iteration(s) ran no tests — that many are NOT evidence\n' "$noop" | tee -a "$log"
fi
if [ "$fail" -gt 0 ]; then
    printf 'failing logs: %s/FAILURE-*.log\n' "$scratch" | tee -a "$log"
fi
