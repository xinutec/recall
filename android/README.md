# recall-mic (Android)

A spare Android phone, mains-powered in a room, as an always-on microphone for
[recall](../). The app captures the mic as raw **48 kHz mono s16le PCM** — the
exact format recall's capture pipeline already speaks — and streams it over a
plain TCP socket to the recall host, reconnecting on any drop and restarting
after a reboot.

It's a **foreground service** with the `microphone` type, which is the supported
way to hold the mic open indefinitely on Android (a plain background process gets
killed by Doze/the OOM killer). The phone is the TCP **client**; recall listens.

## Architecture

```
 phone mic ──AudioRecord──▶ StreamService ──TCP s16le PCM──▶ recall host (ingest server)
                           (foreground svc)                  └─▶ segmenter ─▶ store ─▶ ASR…
```

- `StreamService` — captures + streams + reconnects (the whole job). Its
  foreground notification reflects the household pause state ("Recording paused" vs
  "Waiting for recall host"), read from the same API the screen uses.
- `BootReceiver` — restarts the stream after a reboot if it was enabled.
- `ResumeWarning` / `ResumeWarningReceiver` — a **2h-before-auto-resume heads-up**.
  A bounded pause records a resume-by time; `ResumeWarning` schedules an exact
  `AlarmManager` alarm for `resumeAt − 2h` (re-armed off the same capture-state polls
  the banner uses, so extending the pause moves it at once), and the receiver posts a
  notification in time to extend the pause before the mic comes back on. The
  pause-vs-resume decision is the pure, unit-tested `planResumeWarning`
  (`ResumeWarningPlan.kt`); a pause shorter than the lead gets no warning (the banner
  countdown is the only heads-up there).
- `MainActivity` — set the host, Start/Stop, and a **household-pause banner**:
  when capture is paused it shows the resume time with snooze/resume, mirroring the
  web app. The pause state is one shared value (`MicState.capture`) that both the
  screen and the notification render, polled from the recall web API via
  `CaptureApi` (host `:8000`) — reachable only on the home LAN, same as streaming.
- **Home-only recording:** the service connects to the recall host *before* opening
  the mic, so it records only when that host is reachable. The host is a private
  home-LAN address, reachable only on the home network — so off it (mobile data,
  another Wi-Fi) the connect fails and the mic never opens; nothing is captured
  unless it can actually be delivered to recall. (This is used instead of matching
  the Wi-Fi SSID, which Android 14+ deliberately won't expose to a background
  service — it's location-gated and redacted. No location permission is needed.)
- Capture source is `UNPROCESSED` (no AGC/noise-suppression) where supported,
  falling back to `MIC` — the rawest signal is best for downstream speaker-ID
  and separation.

The recall side ingests this as a TCP-PCM source kind (see the main repo): the
ingest server (`recall.stream_server`) listens on the one shared port, reads each
phone's handshake + raw PCM, and pumps it into the same ffmpeg segmenter the USB
mic uses. Nothing downstream of capture changes.

## Build

The toolchain is provided by the repo flake's `android` dev shell (JDK 17 +
Android SDK; the Gradle wrapper pins Gradle itself):

```sh
cd android
nix develop ..#android --command ./gradlew assembleDebug
# → app/build/outputs/apk/debug/app-debug.apk
```

The APK is signed with the auto-generated debug key — fine for sideloading, which
is the only distribution path (no Play Store).

## Install onto the phone (over WiFi)

One-time, on the phone: enable **Developer options** (tap Build number 7×), turn
on **Wireless debugging**, and pair. Then from the Mac:

```sh
nix-shell -p android-tools --run 'adb pair <ip>:<pair-port>'      # enter the code
nix-shell -p android-tools --run 'adb connect <ip>:<debug-port>'
nix-shell -p android-tools --run 'adb install -r app/build/outputs/apk/debug/app-debug.apk'
```

Mic permission can be granted without touching the phone:

```sh
adb shell pm grant org.recall.mic android.permission.RECORD_AUDIO
adb shell pm grant org.recall.mic android.permission.POST_NOTIFICATIONS
```

Then open the app, enter the recall host IP, and press Start. Keep the phone on
mains power; disable battery optimisation for the app for best longevity.

## Deploying updates

Once a phone is set up, push a new build to all phones in one command:

```sh
cd android
nix develop ..#android --command ./deploy.sh
```

It builds the APK, then for each phone in `deploy.sh`'s `PHONES` list: `install -r`
(keeps the app's config) and relaunches the activity (a reinstall stops the
foreground service, so this restarts streaming — no manual taps).

For this to be friction-free, each phone is pinned:

- **Stable IP** — a DHCP reservation on the router (Mac `192.168.1.81`, Pixel 9
  `.253`, Pixel 5 `.242`). These do drift in practice (the Pixel 9 moved off `.133`),
  so `deploy.sh`'s `PHONES` list is the source of truth — fix it there when it moves.
- **Fixed adb port** — `adb tcpip 5555` (set once while connected over wireless
  debugging) gives a stable port instead of wireless debugging's rotating one.
  This resets on phone reboot; after a reboot, reconnect once (re-enable wireless
  debugging, `adb connect <ip>:<rotating-port>`, `adb tcpip 5555`) to restore it.

Add a phone by appending `<ip>:5555  # comment` to the `PHONES` array.

## Config

| setting | default | meaning |
|---------|---------|---------|
| host    | —       | recall host IP/hostname (must be set) |

The **host is the only setting**. The ingest port is fixed at **9999** (one shared
port for all phones — `StreamService.INGEST_PORT`) and the device id is derived
automatically on first run, so there's nothing else to enter.
