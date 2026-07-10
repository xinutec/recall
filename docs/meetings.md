# Discrete meeting recordings

recall's live capture is continuous, but it can also ingest **discrete, one-off
recordings** — e.g. a meeting or appointment recorded on a phone and dropped into
a cloud folder — transcribe them with diarization, and hand them off as clean
transcripts. This is the `ingest_meetings.py` → `transcribe` → `transcript` path.

The steps below are generic. A *consumer* of this pipeline (a project that pulls
the resulting transcripts into its own documents) keeps its own private mapping of
which recording became which document, and its own source links — see that
project's notes, not this file.

## Filename convention

Each recording is named with its **local start time**:

```
YYYY_MM_DD_HH_MM_SS_N.mp3      e.g. 2026_07_10_09_54_00_1.mp3
```

`ingest_meetings.py` parses that stamp, probes the duration, folds short
add-on segments into the recording they continue, and registers **one source per
session** with id `meeting-YYYYMMDD-HHMM` (labelled in local time — how you think
of it, "the 09:54 one"). Times are stored in UTC internally, as everywhere in recall.

## 1. Get the files onto disk

Drop the `*.mp3` files into the incoming directory (default
`/Volumes/Backup/recall-meetings/incoming/`).

If the source is a **link-shared** cloud folder (anyone-with-link), each file can
be fetched with a plain `curl` on its per-file download endpoint — no auth needed.
For Google Drive that is:

```sh
curl -sL "https://drive.usercontent.google.com/download?id=<FILE_ID>&export=download&confirm=t" \
  -o "/Volumes/Backup/recall-meetings/incoming/<YYYY_MM_DD_HH_MM_SS_N>.mp3"
```

Get the folder's file list + ids however is convenient (the Drive API / an MCP
Drive tool / the web UI). Verify each download is real audio, not an HTML error
page:

```sh
file /Volumes/Backup/recall-meetings/incoming/*.mp3   # expect "MPEG ADTS, layer III"
```

*(If the folder is **not** link-shared, you need an authenticated download —
routing a large binary through anything token-limited is a bad idea; download it
in a real logged-in browser session instead.)*

## 2. Ingest

```sh
nix develop --command .venv/bin/python scripts/ingest_meetings.py [ROOT] [INCOMING_DIR]
# defaults: ROOT=/Volumes/Backup/recall, INCOMING_DIR=/Volumes/Backup/recall-meetings/incoming
```

Registers one source per session and copies the audio under `ROOT/<session-id>/`.
To ingest **only new** recordings without re-touching already-registered ones,
point it at a temp dir holding just the new files:

```sh
mkdir -p /Volumes/Backup/recall-meetings/incoming-new
cp .../2026_07_10_09_54_00_1.mp3 /Volumes/Backup/recall-meetings/incoming-new/
nix develop --command .venv/bin/python scripts/ingest_meetings.py \
  /Volumes/Backup/recall /Volumes/Backup/recall-meetings/incoming-new
```

Session ids are derived from the timestamp, so re-ingesting the same file is a
no-op collision on an existing id — but isolating new files keeps the run obvious.
Confirm what's registered: `ls -d /Volumes/Backup/recall/meeting-*`.

## 3. Transcribe (with diarization)

```sh
./scripts/recall.sh transcribe --id <session-id> --diarize --out /Volumes/Backup/recall
```

- Uses mlx-whisper (`whisper-large-v3-turbo` by default; `--model` to override) plus
  pyannote for per-turn diarization.
- **Diarization is gated on `HF_TOKEN`** — `recall.sh` auto-sources it from `.env`.
  See [running.md](running.md) ("One-time: HuggingFace") and speaker
  [enrolment](running.md) if you want named speakers instead of `SPEAKER_00/01`.
- A `libtorchcodec` load warning at startup is **benign** — pyannote falls back and
  still runs.
- Long recordings take a while and are worth backgrounding; poll the process, don't
  block on a fixed sleep.

## 4. Pull the transcript

```sh
./scripts/recall.sh transcript <session-id> --json --out /Volumes/Backup/recall
```

Returns `{session, date, speakers, turns:[{start, speaker, text}]}`. **`--out` must
point at the data root** or you'll get "no transcript for session" even after a
successful transcribe.

## 5. Clean & attribute

The raw diarized output is a **rough dump**, not a finished transcript:

- turns are often **duplicated** (a continuous ASR pass alongside the diarized split),
- speaker labels get **mis-assigned**, and
- on longer recordings the diarization can **collapse in the second half** into one
  run-on block.

So a publishable transcript is **hand-cleaned**: de-duplicate turns, re-attribute
speakers from the *content*, and fix ASR mishears (names, drug names, numbers). Keep
a short "source & accuracy" note listing the corrections applied — never present a
raw dump as verbatim fact. The audio remains the source of truth.

For transcripts that get corrected *inside* recall's review UI, the corrected export
is deterministic and can be re-pulled cleanly (that's what the
`<!-- recall:transcript <id> -->` block mechanism in downstream projects consumes).

## Gotchas

- `transcribe --id` needs the source **ingested first** (step 2) — it transcribes a
  known source, it does not import a file.
- `--out` on both `transcribe` and `transcript` must be the **data root**, not the
  incoming dir.
- Heavy diarization competes with GPU-bound training — see
  [running.md](running.md) about pausing capture / not stacking GPU jobs.
