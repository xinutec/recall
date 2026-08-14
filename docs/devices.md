# recall — device ingest, identity & onboarding

How recorder devices (the USB mic and roaming phones) connect, are identified, and
report liveness. Companion to design.md §5.1a, which covers *fusing* multiple sources —
several co-located mics capturing the **same** speech; this covers getting their audio
in and knowing which device is which.

## Model: one shared port, devices announce themselves

Every phone connects to **one** TCP port, served by a single host agent:

```
recall ingest                 # the recall-ingest agent: one server on DEFAULT_INGEST_PORT (9999)
recall record --id usb        # the USB mic: sox -d, local, no port, no handshake
```

A phone runs the recall-mic app and opens a plain TCP connection to the host's ingest
port. It first sends a one-line **handshake** announcing itself —
`{"id": "pixel5", "rate": 48000, "channels": 1}\n` — then streams **raw s16le PCM**.
The server (`recall.stream_server`):

1. reads *exactly* the handshake line (byte by byte, so it never consumes any PCM),
2. **auto-registers** the source by the announced id (a filesystem-safe id = one source
   = one storage directory) — no host-side provisioning,
3. pumps the socket's PCM into an ffmpeg segmenter that writes the 60 s segment files —
   the same files the USB mic produces.

So a new phone needs **zero host-side setup**: it connects, announces itself, and the
backend learns about it on first connect — no plist, no per-device port; devices are
added freely.

## The audio path: Python pumps, the kernel buffers

The server reads the socket and writes the bytes to ffmpeg's stdin — one short pump
loop is the only Python in the audio path. That's safe because a phone is a **TCP**
source: the kernel's receive buffer holds incoming audio across any momentary pause
(GC, scheduling), so a stall can't drop samples — the bytes are still contiguous when
the pump catches up. ffmpeg does all the decoding/segmenting, reading a clean pipe.

The USB mic — a real-time local device with **no** such buffer — stays sox/ffmpeg-only
and is never pumped; gap-free local capture depends on keeping our code out of that path.

## Identity: the handshake id

The id is announced in the handshake (the port is just transport), so there is
**nothing to set on a phone but the host**:

- **Derived:** a phone makes a stable id from its model + a random suffix
  (`pixel-9-3f7a`), persisted, so two same-model phones differ (`Prefs.deviceId`).
- **Pre-set:** a device can carry a fixed id (`pixel5`/`pixel9`, the iPhone's
  `iphone11` via `Prefs.presetID`) so its recording history stays one source.

A friendlier name can be set later in the web UI; the underlying id is display-renamable
without moving the data.

## Liveness: the host owns the socket

The ingest server holds the connection, so *recording* liveness is **direct**. While a
device streams, the server refreshes a per-source marker file (`<source>/.alive`); the
`/api/sources` endpoint reads its freshness into a per-recorder active/idle status. The
USB mic is known directly (`capture_running()` and not paused); the source kind
(`tcp_pcm` vs `coreaudio`) picks the path. Uploaded recordings (meetings) are sources
too, but they're excluded — they aren't live devices. The mic apps' Devices panel
renders it (own device highlighted, "active / Ns ago" per recorder).

⚠ **The marker is refreshed only by audio above the silence floor**, so "active" means
*recording*, not connected — a phone streaming digital silence reads idle on purpose,
because nobody should speak trusting a dot the audio can't back
(`stream_server.handle_connection`).

## Aliveness: the app says so, hourly

This page used to say liveness was direct and there was **no phone-sent heartbeat**.
That was true of *recording* and wrong about *running*, and the gap was total rather
than partial (#837):

- A **quiet room and a dead app are the same reading**, by the design just above.
- A phone that vanishes without a FIN leaves the ingest socket half-open, so no
  `ingest_disconnect` is written either and the connection looks open for ever (#838).
- **While paused there is no signal at all** — the listener is closed and every stream
  dropped. Capture is routinely paused for days, which is exactly when an app dying
  goes unnoticed until somebody picks the phone up.

So each mic app POSTs `/api/devices/heartbeat` **once an hour, whether or not it is
streaming**, from the component that is meant to be permanently alive: iOS's held audio
session, Android's `StreamService` foreground service. It beats only while *started* — a
stopped app is not going to record, and a beat that arrived anyway would paint the one
state worth catching bright green.

| | |
|---|---|
| Where | the **control** host (Isis over WireGuard), not the recorder on the LAN — so a phone beats from away from home too, and "out of the house" stops looking like "dead" |
| Auth | **none**, deliberately. The mic app has never held a token, and a beat that could 401 would report a credential mistake as dead hardware (`webauth._DEVICE_EXEMPT`) |
| Carries | `startedAt` (a restart between beats is what "it goes down now and then" looks like from here), `streaming`, `charging`, app + version |
| Stored | one settings row, capped and evicted by age (`recall.mic_alive`) — last-known status, no history, no migration |
| Graded | not here. The Mac reads `/sync/devices/heartbeats` and `xinutec-infra/mac-mini/recall_mics.py` decides what is too long, beside the rest of the fleetwatch thresholds |

`streaming` and `charging` are carried but **never graded**: every honest app reports
`streaming: false` while the household is paused, and a carried phone is off charge all
day. They say what the app was doing when the beats stopped.

## Pause stops phone recording too

A global pause must stop *all* recording, not just the USB mic. While paused, the
ingest server **closes its listener** (so connecting phones are refused and back off,
showing "Recording paused") and **drops any active stream** — its handler finalises the
current segment on the dropped socket, so no audio is lost. On resume it reopens the
listener and the phones reconnect. (`recall.stream_server.serve`.)

One platform nuance: **Android closes its microphone** whenever it can't deliver
(connect-first, then open the mic), so a pause means the mic is off. **iOS keeps the
audio session captive while "on"** (it dies in the background otherwise) and discards
the PCM when disconnected — during a pause the iPhone's mic is technically hot, its
audio dropped in RAM. See ios/README.md ("Always-on").

## Notes

- **One agent, many devices.** `recall-ingest` serves every phone; there's no
  per-device plist or port, and the handshake distinguishes devices, so the old
  unique-port constraint is gone.
- **The USB mic is unchanged** — a separate local `recall record --id usb` agent
  (sox -d), not a TCP client, so it neither handshakes nor pumps.
