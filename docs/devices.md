# Device ingest, identity and liveness

How recorders (the USB mic, the phones, Linux hosts) get audio in, are told
apart, and show they are alive. Several mics hearing the same speech:
[architecture.md](architecture.md).

## Three kinds of recorder

```
audiod ingest --root <archive>   # recall-ingest: one server on DEFAULT_INGEST_PORT (9999)
audiod capture --id usb          # the USB mic: local, no port, no handshake
audiod capture + audiod upload   # a Linux host's own mic (geb): store-and-forward
```

| | how audio reaches the segmenter |
|---|---|
| USB mic (`audiod capture`) | sox into ffmpeg, no socket and none of our code: a real-time device has no buffer to absorb a stall |
| Phones | TCP to the ingest port, plus their own capture-stamped segments delivered with verified receipts |
| Linux hosts (geb) | store-and-forward only: ffmpeg reads ALSA into audiod's segmenter, closed segments go to recalld with verified receipts; `audiod pause-mirror` honours the pause |

The ingest server pumps socket bytes into ffmpeg's stdin; the kernel's receive
buffer absorbs a momentary stall.

## The phone stream

One handshake line, then raw s16le PCM:
`{"id": "pixel5", "rate": 48000, "channels": 1, "epoch": 1756900000.25}\n`.
`epoch` (optional) is the phone's clock at the first PCM byte; the server
measures that byte's arrival and renames the connection's closed segments from
arrival to capture time. An absent epoch, or one more than 10 minutes out,
keeps arrival time; the shift is only ever backwards.

`audiod` reads exactly the handshake line, registers the source by its id (one
id, one directory) in the capture log, and writes the same 60 s segments as the
USB mic. A new recorder needs no setup on the host.

The offset is measured once per connection
(`audiod::rebase::connection_offset`), so a replayed backlog would be stamped
ever more wrongly towards its tail: clients discard audio while disconnected.

Judge reachability by a real request, not ping: from geb the control plane
answered a POST in 0.22 s while ICMP lost every packet.

## Identity

The id comes from the handshake, so the only setting on a recorder is the host.
A phone derives `<model>-<suffix>` once (`Prefs.deviceId`), or carries a preset
(`pixel5`, `pixel9`, `oneplus6t`, iOS `Prefs.presetID` = `iphone11`) so its
history stays one source. Display names are set in the web UI.

## Liveness

`/api/sources` gives two fields per recorder; the apps show audible, recording
but quiet, or off.

| field | question | evidence |
|---|---|---|
| `active` | is speech being captured audibly? | `<source>/.alive`, refreshed only by audio above the silence floor |
| `recording` | is the recorder running? | the marker, or the newest delivered segment |

The marker is refreshed by the server holding a stream, or for the USB mic by
the capture watchdog. A store-and-forward recorder never streams, so it proves
itself by delivering (`/ingest/v1/liveness`): within five minutes, against under
a second per chunk for a stream (`recalld::sources`). Rules:

- A delivery counts by capture time, never arrival.
- A recently stale marker outranks a delivery: a phone's marker going quiet is a
  deliberate stop, newer than segments captured before it. Stale within the
  delivery window means just stopped; beyond it, a recorder that does not stream.
- A pause discards delivered evidence for every recorder.
- Uploaded recordings (meetings) are sources but not recorders, and are left out.

Do not stop a recorder on the strength of this dot: a false "off" once cost two
minutes of audio. Where the panel and the ingest plane disagree, trust ingest.

## The heartbeat

Liveness cannot tell a quiet room from a dead app, says nothing during a pause,
and a phone gone without a FIN leaves its socket looking open. So:

- the mic apps POST `/api/devices/heartbeat` hourly while started, streaming or
  not (a stopped app does not beat);
- audiod's store-and-forward recorders beat every minute
  (`audiod::capture_run::BEAT_EVERY`).

| | |
|---|---|
| Where | the control host first; on failure, the recorder host on the LAN, where `audiod beat-relay` (port 8000) filters the body, stamps `viaLan` and forwards it to Isis |
| Auth | none: a beat that could 401 would report a credential mistake as dead hardware (`recalld::webauth::DEVICE_EXEMPT`) |
| Carries | `startedAt`, `streaming`, `charging`, `micOk`, `droppedBytes`, app, version |
| Retries | after a minute, doubling up to the hourly cadence |
| Stored | last status only, in one settings row (`recalld::devices`); at most 16 devices, aged out after 30 days, `DELETE /api/devices/heartbeat/{device}` |
| Graded | on the Mac: `xinutec-infra/mac-mini/recall_mics.py` reads `/sync/devices/heartbeats` and takes a fresh beat or delivered audio as proof |

`micOk` and `droppedBytes` are graded; `streaming` is false in every pause and a
carried phone is off charge all day.

`droppedBytes` is the audio the app's spool discarded since the previous beat
that landed, because the sender fell behind. On iOS one spool feeds both the
stream and the phone's copy, so it is lost; on Android it is the stream's spool,
and the phone's FLAC copy, with a spool of its own, may still hold it.

| Recorder | `micOk` from | misses |
| --- | --- | --- |
| Android (`MicState.micOkAfter`) | every open attempt | |
| iOS (`RecallMicApp`) | `client.start()`, once | a mic that dies after a good start |
| audiod (geb) | the last producer start | |

## Pause

The ingest server closes its listener and drops active streams, finalising the
current segment; phones are refused and show "Recording paused"
(`audiod::server::serve`). Android's mic is then closed, since it opens only
after connecting. iOS holds its audio session (or it is killed in the
background) and drops the PCM in memory.

## Updating a phone

An install stops the recorder.

- Android: `android/deploy.sh` installs and relaunches every phone.
- iOS: the install needs the phone unlocked; a locked one fails with "The
  developer disk image could not be mounted", which is not a toolchain fault.
  The app must then be opened by hand.

While recording, update one device at a time and wait for a fresh segment
before the next.
