# recall web viewer (Android)

A full-screen WebView of `https://recall.xinutec.org/` (`MainActivity.RECALL_URL`,
reachable on the VPN): a separate app, `org.recall.web`, sharing only the Gradle
project with `recall-mic`. Permission: `INTERNET` only. minSdk 26.

`MainActivity` keeps navigation in-app, lets Back walk the SPA's history, plays
media without a gesture, and pads the WebView clear of the system bars, painting
the strips in the page's surface colour.

```sh
cd android
nix develop ..#android -c ./gradlew :web:assembleDebug   # web/build/outputs/apk/debug/web-debug.apk
adb -s <phone> install -r web/build/outputs/apk/debug/web-debug.apk
adb -s <phone> shell am start -n org.recall.web/.MainActivity
```

Phone addresses are in `../deploy.sh`.
