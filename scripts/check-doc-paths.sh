#!/usr/bin/env bash
# Every repo path a doc cites in backticks must exist.
#
# ⚠ **This is the commonest way documentation rots here, and it rots SILENTLY.**
# A doc that names a deleted file reads exactly like one that names a live file,
# so the claim around it keeps its authority long after the evidence is gone.
# Found twice by hand: a permanent Python floor citing a test module that had
# been deleted, two lines above its own warning that a deleted module standing
# in a floor is the worst place for a claim to rot; and seven paths left behind
# when the test suites were consolidated into `integration/` subdirectories.
#
# ⚠ It only checks paths that LOOK like repo paths — a leading directory this
# repo actually has. A prose backtick like `speaker_label` is not a path and
# must not be treated as one, or the check becomes noise and gets ignored.
set -euo pipefail

cd "$(dirname "$0")/.."

roots='scripts|src|recalld|audiod|audiocore|runner|cli|doctor|frontend|deploy|tests|docs|android|ios'
missing=0

while read -r path; do
    [ -z "$path" ] && continue
    # A path with a line/anchor suffix still names a file.
    file="${path%%#*}"
    file="${file%%:*}"
    if [ ! -e "$file" ]; then
        printf 'missing: %s\n' "$file"
        grep -rn "$path" docs/*.md README.md 2>/dev/null | head -2 | sed 's/^/    cited: /'
        missing=$((missing + 1))
    fi
done < <(grep -ohE "\`($roots)/[A-Za-z0-9_./-]+\`" docs/*.md README.md 2>/dev/null |
         tr -d '`' | sort -u)

if [ "$missing" -gt 0 ]; then
    printf '\n%s doc path(s) name something that does not exist\n' "$missing"
    exit 1
fi
printf 'every repo path cited in the docs exists\n'
