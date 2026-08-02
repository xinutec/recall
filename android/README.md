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

## The second mode: recording a meeting

"Record a meeting" in the drawer opens a recorder for one appointment or meeting — the job
a third-party mp3 recorder used to do. It writes **one Ogg/Opus file** (48 kHz mono,
56 kbps, same `UNPROCESSED`→`MIC` preference), which can then be played back on the phone
and either deleted or uploaded to `/api/sessions` as a session, transcribed and diarized
like any other.

**Nothing uploads by itself.** A finished recording sits in the list on that screen with
Play, Upload and Delete until you choose; the phone holds the only copy until you do.

- `MeetingService` — the recording, as a mic-type foreground service. Starting it stops
  the stream; stopping it starts the stream again if it was enabled. Both modes live in
  one process so that rule can be enforced rather than discovered.
- `MeetingQueue` — the files, in `getExternalFilesDir(Music)/meetings` (app-private but
  visible over USB). Each recording carries a sidecar with its title and true start,
  written before the first audio frame. Pressing Upload *moves* it into `meetings/outbox/`
  — approval is a fact on disk, so a reboot can't lose it and the uploader can't send
  anything you didn't approve.
- `MeetingLibrary` / `MeetingPlayer` — what the list shows (length, size, when), and
  listening back before deciding. Playback is released when the screen closes and never
  changes the device volume.
- `MeetingUpload` — a `WorkManager` job that drains the outbox whenever the host is
  reachable, and keeps retrying while it isn't. Meetings happen where recall is
  unreachable, so an approved recording is still a file first and an upload second;
  nothing is deleted from the phone until the host has it.

Ogg, not m4a, because a truncated Ogg still decodes to its last complete page — a flat
battery costs the tail, not the appointment. (Android has no MP3 encoder at all; Opus is
what it encodes well.) Needs Android 10+; older phones can still stream.

Uploads — from here and from the share sheet — go to the **control host** (Isis), which
is where the API lives; the recorder host serves only the PCM stream.

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

Both hosts live in **Settings**, behind the drawer — they are set once per phone and then
never again, so the daily screen shows status and Start/Stop instead of a form.

| setting      | default      | meaning |
|--------------|--------------|---------|
| recorder host| —            | where the PCM stream goes (must be set) |
| control host | `10.100.0.2` | the recall web API: pause, device list, uploads |

The ingest port is fixed at **9999** (one shared port for all phones —
`StreamService.INGEST_PORT`) and the device id is derived automatically on first run, so
there's nothing else to enter.
