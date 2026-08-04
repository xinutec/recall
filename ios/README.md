# recall-mic — iOS

An iPhone port of the Android `recall-mic` app: captures the mic as 48 kHz mono
signed-16-bit PCM and streams it over TCP to the recall ingester, using the exact same
wire protocol (see [`../docs/devices.md`](../docs/devices.md)). The recorder
auto-registers the device on first connect — **no backend changes needed**.

## Parity with the Android app

| Aspect | Behaviour |
|---|---|
| Handshake | `{"id":"<deviceId>","rate":48000,"channels":1}\n` then raw s16le PCM |
| Audio | 48 kHz, mono, Int16; `.measurement` mode (no AGC/processing ≈ Android `UNPROCESSED`) |
| Port | TCP `9999`, `noDelay` on |
| Capture | **held continuously** while "on" (see *Always-on* below) — PCM is forwarded only while connected, dropped otherwise. This *diverges* from Android's connect-then-open-mic; iOS needs the session held to survive backgrounding. |
| Reconnect | 5 s connect timeout, 2 s retry, infinite — same as Android |
| Pause vs unreachable | resolved via `GET http://<host>:8000/api/capture` (the API is the authority) |
| Device id | `<sanitised-model>-<8hex>` derived once, **unless a fixed pre-set id is given** via `Prefs.presetID` (this build ships `"iphone11"`, like the Pixels' `pixel9`/`pixel5`). Persisted in UserDefaults. |
| Pause banner | shows household state with **Pause** / **Still away (24h)** / **Resume now** (POST `/api/capture/pause`,`/resume`), exactly as Android |
| Devices panel | fleet liveness from `/api/sources`, self highlighted, "active / Ns ago" labels |
| Polling | capture every 5 s, sources every 1.5 s (Android cadence) — and, like Android, only while the UI is visible (`scenePhase`); a backgrounded app polls nothing |
| Background | `UIBackgroundModes: audio` keeps it capturing with the screen off |
| Meter | shared dBFS math + the same colour tiers as Android's `meterTier` (green ≤60 %, orange ≤85 %, red above) — `Levels.tier`, tested against the Android boundaries |
| Recovery | interruptions (`.ended` restarts regardless of `.shouldResume`), route changes and media-services resets restart the engine; a **silence watchdog** kicks a mic that stops delivering buffers (>5 s) and zeroes the meter so a stall is visible, never a frozen "Streaming" (`Watchdog`) |
| Send errors | a failed TCP send cancels the connection so the loop reconnects immediately (no pumping into a dead pipe) |
| Always-on | the audio session is held the *whole* time it's on (capturing even while paused/disconnected), so iOS keeps the app alive in the background indefinitely — only a reboot or force-quit stops it. This is what makes it enable/disable-able over the network: pause/resume capture on the recorder and the still-alive app follows. |

### Platform limits (can't be matched on iOS)
These two Android behaviours have **no iOS API equivalent** — everything else is at parity:
- **Boot auto-start.** iOS cannot relaunch an app on reboot (Android's `BootReceiver`).
  Closest equivalent, implemented: the app **auto-resumes** streaming the next time it's
  opened, if it was left enabled.
- **Persistent status notification.** Android runs a foreground service with an ongoing
  "Streaming to…/Paused" notification. iOS has no ongoing notification for a backgrounded
  app; while recording you get the system **orange microphone indicator** instead. The
  Android wake-lock is likewise unneeded — the active audio session keeps the app alive.

## Build & install

iOS apps can only be built with **full Xcode** (the iOS SDK). The Command Line Tools on
this Mac are not enough. Install Xcode from the Mac App Store first.

1. **Generate the project** (from this `ios/` directory):
   ```sh
   nix-shell -p xcodegen --run 'xcodegen generate'
   open RecallMic.xcodeproj
   ```
   (Or, without XcodeGen: create a new iOS App in Xcode and add the files in `Sources/`
   plus the Info.plist keys listed in `project.yml`.)

2. **Signing:** select the `RecallMic` target → *Signing & Capabilities* → set your
   Team (the same identity you sideload with). Bundle id is `org.recall.mic`.

3. **Run to the iPhone:** pick the device (USB, or wireless once paired) and press Run.
   For a longer-lived install than a 7-day free cert, use a paid developer account or an
   auto-refresh sideloader (AltStore/SideStore).

### Redeploy from the CLI (no GUI)

Once signing is set up (team `83SSMZ4T7X` is baked into `project.yml`), rebuild and push
to the connected iPhone in one go — handy after a code change:

```sh
cd ios
nix-shell -p xcodegen --run 'xcodegen generate'      # only if project.yml changed
DEV=$(xcrun devicectl list devices | awk '/iPhone/{print $4; exit}')   # CoreDevice id
xcodebuild -project RecallMic.xcodeproj -scheme RecallMic -configuration Debug \
  -destination 'platform=iOS,id=<UDID>' -derivedDataPath build -allowProvisioningUpdates build
xcrun devicectl device install app --device "$DEV" \
  build/Build/Products/Debug-iphoneos/RecallMic.app
```

`<UDID>` comes from `idevice_id -l` (USB) or Xcode; the CoreDevice id from
`xcrun devicectl list devices`. Note `xcrun` only finds the toolchain **outside** the Nix
shell — the gate works around that (`env -u DEVELOPER_DIR -u SDKROOT`).

## Tests

Unit tests (`Tests/` — Levels/meter-tier parity with Android, DeviceID, the
watchdog decision, and the paused-banner text) run on the iOS Simulator, from the
**repo root** (the script lives there, not in `ios/`):

```sh
./scripts/ios-test.sh
```

One-time setup (already done on this Mac): `xcodebuild -downloadPlatform iOS`,
then `xcrun simctl create recall-test "iPhone 16" com.apple.CoreSimulator.SimRuntime.iOS-26-5`.
The gate lint-checks `Sources/` + `Tests/` on every run; the tests themselves
are on-demand (simulator boot is slow).

## First run on the phone

1. Grant **Microphone** and **Local Network** when prompted (both are required; the
   local-network prompt appears on the first connection attempt).
2. Set **Recorder host** to the Mac's LAN IP — currently **`192.168.1.81`** (`mac-mini`).
3. Press **Start**. The status dot goes green and the level meter moves when you speak.

This device registers as source **`iphone11`**, shown as **"iPhone 11"** (display name set
in the store; the model code `iPhone12,1` *is* the iPhone 11, so the name is corrected).
No DHCP reservation was added — unlike the Pixels it rides a dynamic lease, which is fine
because identity comes from the handshake id (`Prefs.presetID`), not the IP.

## Layout

```
ios/
├── project.yml            # XcodeGen project (target, Info.plist, background-audio)
├── Sources/
│   ├── RecallMicApp.swift # @main, owns state + stream client, auto-resume
│   ├── ContentView.swift  # host field, Start/Stop, status card, level meter, pause banner
│   ├── StreamClient.swift # NWConnection TCP: handshake → PCM pump → reconnect loop
│   ├── AudioCapture.swift # AVAudioEngine → 48 kHz mono Int16 via AVAudioConverter
│   ├── CaptureApi.swift   # polls /api/capture for pause state
│   ├── MicState.swift     # observable shared state
│   ├── Prefs.swift        # UserDefaults: host, port, enabled, device id
│   ├── DeviceID.swift     # stable source-id derivation (model + hex suffix)
│   ├── Levels.swift       # pure dBFS level-meter math (port of Levels.kt)
│   ├── Banner.swift       # paused-banner text, byte-identical to web/Android
│   └── Watchdog.swift     # pure decision: when a silent stream must be cycled
├── Tests/                 # Banner, DeviceID, Levels, Watchdog (the pure units)
├── Info.plist
└── README.md
```
