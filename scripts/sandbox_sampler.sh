#!/usr/bin/env bash
# Sample the NIX SANDBOX arm of the workspace test suite (#1480).
#
# ⚠ **`nix build --rebuild` and `--check` CANNOT do this.** Both return in under
# a second on a cache hit and run NO tests, so a loop over either reports a
# confident zero having done nothing — and one did: "0 sandbox failures in 8
# runs" was eight no-ops. What works is varying a TRACKED input so the
# derivation hash changes every iteration, plus an assertion that cargo really
# ran, which is why this counts `test result:` lines and refuses a run with none.
#
# ⚠ It works in a scratch CLONE. The marker has to be `git add`ed for the flake
# to see it (nix ignores untracked files), and doing that in the real checkout
# would mix an experiment into the tree somebody is committing from.
#
#   scripts/sandbox_sampler.sh [iterations]     default 10
# ⚠ `set -e` with a loop whose whole purpose is to SURVIVE a failing command:
# every status that is expected to be non-zero is captured in an `if` or with an
# explicit `|| true`, never left bare. In particular `grep -c` exits 1 when it
# counts ZERO, which is the ordinary case here and would otherwise end the run
# at the first clean build.
set -euo pipefail

iterations="${1:-10}"
repo="$(cd "$(dirname "$0")/.." && pwd)"
scratch="${SANDBOX_SAMPLER_DIR:-$HOME/.cache/recall-sandbox-sampler}"
log="$scratch/sampler.log"

rm -rf "$scratch"
mkdir -p "$scratch"
git clone --quiet --no-hardlinks "$repo" "$scratch/recall"
cd "$scratch/recall"

# ⚠ Unique per RUN, not just per iteration. A marker of "iteration 3" is the
# same text every run, so the second run's derivations hash to the first run's
# and nix serves the cache — a whole sampling that measures nothing. Caught by
# the assertion below on the first re-run, which is what it is for.
run="$(date -u +%Y%m%dT%H%M%S)-$$"
pass=0 fail=0 noop=0
for i in $(seq 1 "$iterations"); do
    # A comment in the crate everything depends on: the whole workspace is
    # rebuilt and re-tested, which is the condition the flake needs (it does
    # NOT reproduce with webauth alone — 0/40).
    printf '\n// sandbox sampler %s iteration %s\n' "$run" "$i" >> audiocore/src/lib.rs
    git add audiocore/src/lib.rs

    out="$scratch/run-$i.log"
    started=$SECONDS
    if nix build --no-warn-dirty --no-link -L .#agents > "$out" 2>&1; then
        status=0
    else
        status=$?
    fi
    elapsed=$((SECONDS - started))

    # ⚠ The did-it-actually-run assertion. A build that ran no tests says
    # nothing about a test flake, and must never be counted as a pass.
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
        # Keep the failing log under a name that says so; the rest are churn.
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
