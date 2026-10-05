#!/usr/bin/env bash
# Every repo path a doc cites in backticks must exist.
#
# A doc naming a deleted file reads like one naming a live file.
#
# Only backticks starting with one of this repo's top directories count as
# paths; `speaker_label` is not one.
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
