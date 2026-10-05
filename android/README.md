# recall-mic (Android)

A mains-powered phone as an always-on microphone for recall. A `microphone`
foreground service (`StreamService`) captures 48 kHz mono s16le, `UNPROCESSED`
where supported, else `MIC`, and sends it two ways:

- **Live stream:** TCP to the recorder host (the Mac, on the home LAN), port
  9999, a handshake line (`Handshake.kt`; parsed by `audiod/src/wire.rs`) then
  raw PCM. It connects before opening the mic, so away from home nothing records.
- **Segments:** `SegmentWriter` writes one-minute FLAC files beside the stream;
  `SegmentUpload` PUTs them to `https://recall.xinutec.org/ingest/v1/segments/…`
  A delivery counts only when the receipt's sha-256 matches; delivered files
  are evicted oldest first past a size ceiling (`SegmentStore`).

The control host (`https://recall.xinutec.org` by default; a bare host means
`http://<host>:8000`, see `ApiBase`) serves the capture pause, the device list,
the heartbeat and uploads. `BootReceiver` restarts the stream after a reboot;
`ResumeWarning` notifies 2 h before a bounded pause ends.

## Meeting mode

"Record a meeting" (`MeetingService`) stops the stream and writes one Ogg/Opus
file, since a truncated Ogg still decodes. Upload moves the file to
`meetings/outbox/`; `MeetingUpload` (WorkManager) posts it to `/api/sessions` and
files it under `uploaded/` or `unverified/` by whether recall's length matches.
Nothing is deleted except by the Delete button. Needs Android 10+.

## Build, test, deploy

```sh
cd android
nix develop ..#android -c ktlint "app/src/**/*.kt" "web/src/**/*.kt"
nix develop ..#android -c ./gradlew :app:testDebugUnitTest :app:assembleDebug :web:assembleDebug
nix develop ..#android -c ./deploy.sh    # install -r and relaunch on every phone
```

`deploy.sh`'s `PHONES` list holds each phone's LAN and VPN address. Phones use
adb port 5555 (`adb tcpip 5555`), which a reboot undoes. A new phone needs the
recorder host set in Settings, and:

```sh
adb shell pm grant org.recall.mic android.permission.RECORD_AUDIO
adb shell pm grant org.recall.mic android.permission.POST_NOTIFICATIONS
```
