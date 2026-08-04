#!/usr/bin/env bash
# Fail if any tracked file contains a term from the private denylist. The denylist
# (real names, addresses, recorded-utterance fragments) lives OUTSIDE the repo, in
# the encrypted data root — committing it here would itself be the violation it
# guards against. One term per line; matched case-insensitively as a fixed string.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DENYLIST=/Volumes/Backup/recall/pii-denylist.txt

# A missing denylist FAILS. It used to `exit 0` with a warning, added on
# 2026-07-09 to "make validation gates resilient to unmounted volumes" — but this
# is the check that stops real names and addresses reaching tracked files, and
# those files get pushed. Resilient here meant: when the external volume happens
# not to be mounted, the one gate that guards personal data silently passes.
# An unmounted volume is a normal condition on this machine, so that was not a
# rare corner. Being unable to commit until it is mounted is the cheaper failure.
if [[ ! -r "$DENYLIST" ]]; then
    echo "check-pii: cannot read $DENYLIST" >&2
    echo "check-pii: the PII gate cannot run, so this is a FAILURE, not a skip." >&2
    echo "check-pii: mount the Backup volume and re-run, or commit with --no-verify" >&2
    echo "check-pii: only if you are certain no personal terms are in the change." >&2
    exit 1
fi

# -F fixed strings, -i case-insensitive, -w whole words (a short name must not match
# inside a base64 hash), -I skip binaries. xargs may split the file list into several
# grep runs with mixed exit codes, so judge by collected output, not exit status.
matches=$(git ls-files -z | xargs -0 grep -FIinw -f "$DENYLIST" -- 2>/dev/null || true)
if [[ -n "$matches" ]]; then
    printf '%s\n' "$matches" >&2
    echo "check-pii: personal terms found in tracked files (denylist: $DENYLIST)" >&2
    exit 1
fi
echo "check-pii: no denylisted terms in tracked files"
