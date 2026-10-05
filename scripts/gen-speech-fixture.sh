#!/usr/bin/env bash
# Generate tests/fixtures/speech/ — the committed real-speech golden fixture.
#
# Synthetic speech from macOS `say`, so no household audio enters the repo. The
# FLAC and reference transcripts are committed; score-asr transcribes them with
# the real ASR and fails when WER passes its threshold.
#
# Regenerate only deliberately: a changed `say` voice changes the audio and may
# move the baseline.
#   ./scripts/gen-speech-fixture.sh
set -euo pipefail

# shellcheck disable=SC1091 # the nix profile, absent where nix is not installed
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT=tests/fixtures/speech
mkdir -p "$OUT"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Invented dialogue in alternating voices. One fixture per language: Whisper
# detects one language per segment, so mixing would measure that known weakness
# instead. Keep these lines and reference-*.txt in step.
utter() { # utter <index> <voice> <text>
    say -v "$2" -o "$WORK/$1.aiff" "$3"
}
utter en-01 Daniel "Good morning, did you remember to water the plants on the balcony?"
utter en-02 Moira "Yes, I did that before breakfast, and I also fed the cat."
utter en-03 Daniel "Perfect. The plumber is coming on Thursday at half past nine."
utter en-04 Moira "Then I will move the boxes out of the hallway tomorrow evening."
utter en-05 Daniel "Could you add oranges and coffee to the shopping list please?"
utter en-06 Moira "Already done. We should also book the train tickets this weekend."
utter en-07 Daniel "Agreed. Let us have dinner at seven and watch the film afterwards."
utter nl-01 Xander "Vergeet niet dat we zondag bij de bakker brood moeten halen."
utter nl-02 Ellen "Goed idee, en daarna kunnen we koffie drinken in het park."
utter nl-03 Xander "De trein naar de stad vertrekt morgen om kwart over acht."

# One 48 kHz mono FLAC per language, the shape of a captured segment, with 0.8 s
# of silence between utterances.
stitch() { # stitch <lang>
    : >"$WORK/$1.pcm"
    for f in "$WORK/$1"-*.aiff; do
        nix develop --command ffmpeg -hide_banner -loglevel error \
            -i "$f" -f s16le -ac 1 -ar 48000 - >>"$WORK/$1.pcm"
        dd if=/dev/zero bs=76800 count=1 2>/dev/null >>"$WORK/$1.pcm" # 0.8s @ 48k s16
    done
    nix develop --command ffmpeg -hide_banner -loglevel error -y \
        -f s16le -ac 1 -ar 48000 -i "$WORK/$1.pcm" -c:a flac "$OUT/dialogue-$1.flac"
}
stitch en
stitch nl

cat >"$OUT/reference-en.txt" <<'REF'
Good morning, did you remember to water the plants on the balcony?
Yes, I did that before breakfast, and I also fed the cat.
Perfect. The plumber is coming on Thursday at half past nine.
Then I will move the boxes out of the hallway tomorrow evening.
Could you add oranges and coffee to the shopping list please?
Already done. We should also book the train tickets this weekend.
Agreed. Let us have dinner at seven and watch the film afterwards.
REF

cat >"$OUT/reference-nl.txt" <<'REF'
Vergeet niet dat we zondag bij de bakker brood moeten halen.
Goed idee, en daarna kunnen we koffie drinken in het park.
De trein naar de stad vertrekt morgen om kwart over acht.
REF

echo "gen-speech-fixture: wrote $OUT/dialogue-{en,nl}.flac + reference-{en,nl}.txt"
