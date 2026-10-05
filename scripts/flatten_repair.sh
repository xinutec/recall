#!/usr/bin/env bash
# Undo the diarized pass's flattening on N clips (#1663).
#
# A pass may split turns (keeping every word) or label them; a merge is
# neither. This hides the merged turns and restores the originals. It does not
# release the clips to the pass: do that afterwards, since releasing first let
# the pass re-decide against the broken state and left 7 duplicate turns.
#
# The merged turns are hidden before the originals are restored, or the restored
# ones would match the "not hidden" predicate and be hidden again.
#
#   scripts/flatten_repair.sh <count>      default 20
set -euo pipefail

count="${1:-20}"
host=root@isis
pvc=/var/lib/rancher/k3s/storage/pvc-0d2b964f-9ebf-4720-8e9f-b543ca3a0dbb_recall_recall-data-pvc
stamp=$(date -u +%Y%m%d)
reason="flatten-repair $stamp"

ssh "$host" "set -euo pipefail
pvc=$pvc
# A snapshot per day's run, to undo against the state it started from.
for db in recall ingest; do
  [ -f \"\$pvc/\$db-before-flatten-repair-$stamp.sqlite\" ] ||
    sqlite3 \"\$pvc/\$db.sqlite\" \".backup \$pvc/\$db-before-flatten-repair-$stamp.sqlite\"
done

sqlite3 \"\$pvc/recall.sqlite\" <<SQL
PRAGMA busy_timeout = 60000;
CREATE TEMP TABLE pick AS
SELECT a.id aid,
       replace(a.path, rtrim(a.path, replace(a.path,'/','')), '') fname,
       SUM(CASE WHEN t.hidden_reason LIKE 'diarized%' THEN 1 ELSE 0 END) hidden,
       SUM(CASE WHEN t.provenance LIKE 'diarized-aligned%' AND t.hidden_reason IS NULL
                     AND t.superseded_by IS NULL THEN 1 ELSE 0 END) written
FROM audio_segments a JOIN transcript_segments t ON t.audio_segment_id = a.id
WHERE a.source_id NOT LIKE 'meeting-%'
GROUP BY a.id HAVING hidden > 0 AND written > 0 AND written < hidden
ORDER BY (hidden - written) DESC LIMIT $count;

BEGIN IMMEDIATE;
-- 1. the merged replacements go first
UPDATE transcript_segments SET hidden_reason = '$reason'
 WHERE audio_segment_id IN (SELECT aid FROM pick)
   AND provenance LIKE 'diarized-aligned%' AND hidden_reason IS NULL
   AND superseded_by IS NULL;
SELECT 'merged turns hidden', changes();
-- 2. then the originals come back
UPDATE transcript_segments SET hidden_reason = NULL
 WHERE audio_segment_id IN (SELECT aid FROM pick)
   AND hidden_reason LIKE 'diarized%';
SELECT 'original turns restored', changes();
COMMIT;

SELECT 'clips repaired', COUNT(*) FROM pick;
SELECT group_concat(fname, char(10)) FROM pick;
SQL"
