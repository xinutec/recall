# recall — device ingest, identity & liveness

How recorders (the USB mic, the phones, and always-on Linux hosts) get their audio
in, are told apart, and show that they are alive. What happens to several mics
hearing the same speech is in [architecture.md](architecture.md).

## Model: one shared port, devices announce themselves

Every networked recorder connects to one TCP port, served by one agent:

```
audiod ingest --root <archive>   # recall-ingest: one server on DEFAULT_INGEST_PORT (9999)
audiod capture --id usb          # the USB mic: local, no port, no handshake
audiod capture + audiod upload   # a Linux host's own mic (geb): store-and-forward
```

A phone runs the recall-mic app and opens a plain TCP connection to the ingest
port. It sends a one-line handshake —
`{"id": "pixel5", "rate": 48000, "channels": 1, "epoch": 1756900000.25}\n` — then
streams raw s16le PCM. `epoch` (optional) is the phone's wall clock, in unix
seconds, for the first PCM byte. The server measures that byte's arrival and
renames each closed segment from arrival time to capture time, so the mics' times
line up. Without a usable epoch (absent, or more than 10 minutes out) segments
keep their arrival time; the shift is only ever backwards.

The server (`audiod`):

1. reads exactly the handshake line, byte by byte, so it consumes no PCM;
2. registers the source by its announced id (one id, one storage directory) in the
   capture log, with no host-side setup;
3. pumps the PCM into an ffmpeg segmenter that writes the same 60 s segment files
   the USB mic produces.

So a new recorder needs nothing on the host: it connects, announces itself, and
exists from then on.

## Three kinds of client

| | how the audio reaches the segmenter |
|---|---|
| **USB mic** (`audiod capture`) | locally: sox into ffmpeg, no socket. No code of ours is in this path, because a real-time device has no buffer to absorb a stall |
| **Phones** (the mic apps) | TCP to the ingest port, handshake, raw PCM; and alongside it, closed capture-stamped segments of their own, delivered with verified receipts. The phones roam, sleep and need a person to restart them, which is what most of the apps is for |
| **Linux hosts** (geb: `audiod capture` + `audiod upload`) | store-and-forward only: ffmpeg reads ALSA into audiod's segmenter, closed segments go to recalld with verified receipts, and `audiod pause-mirror` honours the household pause |

A Linux mic is the cheapest recorder to add: no roaming, no battery, no app
lifecycle. Its uploader reads only closed files, so a network stall costs
delivery latency, never audio.

⚠ **Do not judge reachability by ping.** From geb the control plane answered a
real POST in 0.22 s while ICMP to the same address lost every packet. Check with
the request itself.

⚠ **A Linux host's buffer is a cushion, not a store.** The server renames a
connection's segments by one offset measured at its first byte
(`audiod::rebase::connection_offset`), so a replayed backlog would be stamped
right at its head and ever more wrong towards its tail. Clients discard while
disconnected; keeping audio across a disconnect needs a protocol that times each
segment.

## The audio path: a pump and the kernel's buffer

`audiod ingest` reads the socket and writes the bytes to ffmpeg's stdin. That is
safe because a phone is a TCP source: the kernel's receive buffer holds audio
across any momentary pause, so a stall drops nothing. The USB mic has no such
buffer, which is why our code stays out of its path.

## Identity: the handshake id

The id comes in the handshake (the port is only transport), so the only thing to
set on a recorder is the host:

- **Derived:** a phone builds a stable id from its model and a random suffix
  (`pixel-9-3f7a`), persisted, so two phones of one model differ
  (`Prefs.deviceId`).
- **Preset:** a device can carry a fixed id (`pixel5`, `pixel9`, `oneplus6t`, the
  iPhone's `iphone11` via `Prefs.presetID`), so its history stays one source.

A friendlier display name can be set in the web UI without moving any data.

## Liveness: two questions, two answers

`/api/sources` answers two different questions per recorder, and the mic apps show
three states from the pair: audible, recording but quiet, off.

| field | question | evidence |
|---|---|---|
| `active` | is my voice being captured audibly? | a marker file (`<source>/.alive`) refreshed only by audio above the silence floor, so a silent room reads inactive on purpose: nobody should speak trusting a dot the audio cannot back |
| `recording` | is this recorder running? | the marker, or the newest delivered segment |

The marker is refreshed by the server holding a stream, or for the USB mic by the
capture watchdog. ⚠ **A store-and-forward recorder refreshes no marker**: it never
opens a stream, so it proves itself by delivering (`/ingest/v1/liveness`). The
windows differ because the evidence does: under a second per chunk for a stream,
up to five minutes for a delivery, which must close and wait out the upload timer
(`recalld::sources`).

Rules the two proofs need:

- ⚠ **A delivery counts by its capture time, never its arrival.** A backlog
  draining hours late arrives now and proves nothing about now.
- ⚠ **A recently stale marker outranks a delivery.** A phone streams as its main
  path, so its marker going quiet is the deliberate stop, newer than any segment
  captured just before it. Without this, a phone stopped by hand stayed green for
  the whole delivery window. How stale tells the cases apart: inside the delivery
  window means just stopped, beyond it means a recorder that does not stream.
- ⚠ **A pause discards delivered evidence for every kind of recorder**, or audio
  captured in the seconds before a pause would keep a dot green through it.
- Uploaded recordings (meetings) are sources too, but not recorders, and are
  left out.

⚠ **Do not stop a recorder on the strength of this dot.** A false "off" once led
to a recorder being stopped and two minutes of audio lost. Where the panel and the
ingest plane disagree, the ingest plane holds the evidence.

## Aliveness: the heartbeat

Recording liveness cannot tell a quiet room from a dead app, says nothing at all
during a pause (the listener is closed), and a phone that vanishes without a FIN
leaves its socket looking open for ever. So every recorder also says it is alive:

- the mic apps POST `/api/devices/heartbeat` once an hour while started, whether
  or not they are streaming, from the part meant to stay alive (iOS's held audio
  session, Android's `StreamService`). A stopped app does not beat: a beat would
  paint the one state worth catching green.
- audiod's store-and-forward recorders (geb) beat every minute
  (`audiod::capture_run::BEAT_EVERY`).

| | |
|---|---|
| Where | the control host (Isis over WireGuard) first, so a phone away from home still beats; if that fails, the recorder host on the LAN, where `audiod beat-relay` forwards it |
| Auth | none, on purpose: a beat that could 401 would report a credential mistake as dead hardware (`recalld::webauth::DEVICE_EXEMPT`) |
| Carries | `startedAt` (a restart between beats), `streaming`, `charging`, `micOk`, app and version |
| Retries | a failed beat retries after a minute, doubling up to the hourly cadence |
| Stored | one settings row (`recalld::devices`): last-known status only. At most 16 devices, aged out after 30 days, removable with an authenticated `DELETE /api/devices/heartbeat/{device}` |
| Graded | not here: the Mac reads `/sync/devices/heartbeats`, and `xinutec-infra/mac-mini/recall_mics.py` decides what is too old, taking either a fresh beat or delivered audio as proof |

`streaming` and `charging` are carried but never graded: every honest app reports
`streaming: false` during a pause, a carried phone is off charge all day, and a
phone switched off on purpose is not a fault. `micOk` is graded: the app is
running but the audio engine will not open.

| Recorder | Where `micOk` comes from | What it can miss |
| --- | --- | --- |
| Android (`MicState.micOkAfter`) | every open attempt; a failure that never reached the microphone leaves it unchanged | — |
| iOS (`RecallMicApp`) | the result of `client.start()`, once | a mic that dies after a good start still reads true |
| audiod (geb) | the last producer start | — |

### The LAN fallback

A phone at home with its tunnel off records correctly but cannot reach Isis to
beat. So the Mac answers the same request on the LAN: `audiod beat-relay`, its own
small server on port 8000 (the fleet's port, so the apps need one URL shape),
separate from the capture agents because a pause closes the ingest listener. It
stores nothing: it filters the body to an allowlist, stamps `viaLan` itself, and
forwards to Isis, the only place a beat lives.

## Pause stops every recorder

While paused, the ingest server closes its listener (phones are refused, back off
and show "Recording paused") and drops any active stream, finalising the current
segment so nothing is lost. On resume it reopens and the phones reconnect
(`audiod::server::serve`).

**Android closes its microphone** whenever it cannot deliver (it connects first,
then opens the mic), so a pause means the mic is off. **iOS keeps its audio
session while on**, because it is killed in the background otherwise, and discards
the PCM while disconnected: during a pause the iPhone's mic is technically live and
its audio dropped in memory. See ios/README.md ("Always-on").

## Updating a phone's app

An install stops the recorder on both platforms, and only Android can be
restarted remotely.

- **Android:** `adb install -r app-debug.apk`, then `adb shell monkey -p
  org.recall.mic -c android.intent.category.LAUNCHER 1` restarts the foreground
  service. The phones answer adb on their VPN addresses (`adb connect
  10.100.0.N:5555`), not the LAN.
- **iOS:** `xcodebuild … -destination 'platform=iOS,id=<udid>' install` needs the
  phone unlocked and awake; a locked one fails with "The developer disk image could
  not be mounted on this device", which looks like a toolchain problem and is not.
  ⚠ **The app must then be opened by hand**: there is no remote equivalent of the
  Android intent, so an iPhone left alone after an install has stopped recording.

While the household is recording, update one device at a time and confirm it is
streaming again (a fresh segment under its source directory) before the next, so
a live mic is always up.
