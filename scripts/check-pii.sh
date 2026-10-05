#!/usr/bin/env bash
# Fail if any tracked file contains a term from the private denylist, which lives
# in the encrypted data root, not here. One term per line, matched as a
# case-insensitive fixed string.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DENYLIST=/Volumes/Backup/recall/pii-denylist.txt

# A missing denylist fails: the volume is often unmounted, and passing then would
# let personal data be pushed.
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
