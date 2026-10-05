# recall-mic (iOS)

The iPhone counterpart of the Android app (`../android`); most pure units
(`Handshake`, `Levels`, `PcmSpool`, `Banner`, `ApiBase`) mirror the Kotlin ones
and their tests mirror the Kotlin tests.

- **Live stream:** `StreamClient`, TCP to the recorder host (the Mac on the home
  LAN) on 9999: a handshake line, then 48 kHz mono s16le. 5 s connect timeout,
  2 s retry.
- **Segments:** `SegmentWriter` writes one-minute WAVs beside the stream (Android
  writes FLAC, #1842); `SegmentUpload` PUTs them to
  `https://recall.xinutec.org/ingest/v1/segments/…`, not on expensive networks.
- **Control:** `CaptureApi` uses `/api/capture` (long poll), `/api/capture/pause`,
  `/api/capture/resume` and `/api/sources` on the control host; `Heartbeat` posts to
  `/api/devices/heartbeat`. Polling runs only while the UI is visible.
- **Capture:** `AudioCapture`, `.measurement` mode (no gain or noise
  processing). Unlike Android, the audio session is held the whole time the app
  is on, connected or not, because that is what keeps an iOS app alive in the
  background; PCM is dropped while disconnected. `Watchdog` restarts a mic that
  delivers nothing for 5 s.
- **Identity:** `Prefs.presetID` (`"iphone11"`); nil derives one (`DeviceID`).

iOS cannot start the app at boot; it resumes streaming when next opened if it
was left on.

## Build, test, install

Needs full Xcode. The project is generated from `project.yml` (team
`83SSMZ4T7X`, bundle `org.recall.mic`); `xcrun` works only outside the Nix
shell, or with `env -u DEVELOPER_DIR -u SDKROOT`.

```sh
./scripts/ios-test.sh        # from the repo root: unit tests on the simulator

cd ios
nix-shell -p xcodegen --run 'xcodegen generate'
DEV=$(xcrun devicectl list devices | awk '/iPhone/{print $4; exit}')
xcodebuild -project RecallMic.xcodeproj -scheme RecallMic -configuration Debug \
  -destination 'platform=iOS,id=<UDID>' -derivedDataPath build -allowProvisioningUpdates build
xcrun devicectl device install app --device "$DEV" \
  build/Build/Products/Debug-iphoneos/RecallMic.app
```

`<UDID>` from `idevice_id -l` or Xcode. On a new phone: allow Microphone and
Local Network, set the recorder host to the Mac's LAN address, press Start.
