# recall web viewer (Android)

The recall web UI (the Angular SPA the recall API serves at `http://<host>:8000`)
presented as a native-feeling app: a single full-screen **WebView**, no address
bar, no tabs, a home-screen icon. It avoids browser chrome while showing the UI
exactly as designed (the system WebView is Chromium, so it renders like Chrome).

It is a **separate app** from `recall-mic` (`../app`) — different application id
(`org.recall.web` vs `org.recall.mic`), its own icon and launcher entry. The two
just share this Gradle project (toolchain, wrapper, version catalog). Installing
or updating one never touches the other.

## What it does

- Loads `http://10.100.0.2:8000/` (Isis's WireGuard address, the fleet system of
  record) — the host is **hardcoded** (`MainActivity.RECALL_URL`); this app is
  single-purpose. Being on the VPN, it works whether home or away, and pause/resume
  on this UI drives the capture intent the Mac mirrors.
- JavaScript + DOM storage on (Angular), media playback without a gesture (so the
  UI's audio clips play on tap), all navigation kept in-app, Back walks the SPA's
  history.
- Insets the WebView from the system bars by padding a wrapper, and paints the
  strips behind the bars with the page's own surface colour (read on load, so it
  tracks the Material light/dark theme). The WebView no longer underlaps the bars,
  so the page's own `env(safe-area-inset-*)` collapse to 0 and add nothing on top.

It needs only `INTERNET` and cleartext (HTTP over the WireGuard tunnel to a private
address) — no other permissions.

## Build & install

Same toolchain as `recall-mic` (the repo flake's `android` dev shell):

```sh
cd android
nix develop ..#android --command ./gradlew :web:assembleDebug
# → web/build/outputs/apk/debug/web-debug.apk
```

Install onto a phone over WiFi (Pixel 9 is at `192.168.1.133:5555`, same as the
mic app — see `../README.md` for first-time adb pairing):

```sh
ADB="$ANDROID_HOME/platform-tools/adb"
"$ADB" connect 192.168.1.133:5555
"$ADB" -s 192.168.1.133:5555 install -r web/build/outputs/apk/debug/web-debug.apk
"$ADB" -s 192.168.1.133:5555 shell am start -n org.recall.web/.MainActivity
```

The APK is signed with the auto-generated debug key — fine for sideloading, the
only distribution path. It runs on any Android 8+ (minSdk 26) device.

## Layout

```
web/
├── build.gradle.kts                         # android app module, no Compose/AppCompat
└── src/main/
    ├── AndroidManifest.xml                   # INTERNET + cleartext; single launcher activity
    ├── kotlin/org/recall/web/MainActivity.kt # the WebView + window-inset handling
    └── res/                                  # launcher icon (purple, document glyph), theme, strings
```
