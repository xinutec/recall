#!/usr/bin/env bash
# Fail if any tracked file contains a term from the private denylist. The denylist
# (real names, addresses, recorded-utterance fragments) lives OUTSIDE the repo, in
# the encrypted data root — committing it here would itself be the violation it
# guards against. One term per line; matched case-insensitively as a fixed string.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DENYLIST=/Volumes/Backup/recall/pii-denylist.txt

if [[ ! -r "$DENYLIST" ]]; then
    echo "check-pii: $DENYLIST not readable (volume unmounted?) — cannot verify" >&2
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
