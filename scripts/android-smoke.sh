#!/usr/bin/env bash
# Start check for the Android build, run inside the CI emulator:
# installs the APK, opens the app, walks through the main views and saves a
# screenshot of each, plus the log.
set -x
PKG=io.github.speteai.aipgdesktop
mkdir -p out
APK=$(find apk -name '*x86_64*.apk' | head -1)

shot() { sleep "${2:-4}"; adb exec-out screencap -p > "out/$1.png"; }
# tap the first on-screen element whose text matches $1 (exact)
tap_text() {
  adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
  adb pull /sdcard/ui.xml out/ui.xml >/dev/null 2>&1
  local xy
  xy=$(python3 - "$1" <<'PY'
import re, sys, xml.etree.ElementTree as ET
want = sys.argv[1]
try:
    root = ET.parse("out/ui.xml").getroot()
except Exception:
    sys.exit(0)
for n in root.iter("node"):
    if (n.get("text") or "").strip() == want or (n.get("content-desc") or "").strip() == want:
        a = list(map(int, re.findall(r"\d+", n.get("bounds"))))
        print((a[0] + a[2]) // 2, (a[1] + a[3]) // 2)
        break
PY
)
  if [ -n "$xy" ]; then adb shell input tap $xy; echo "tapped '$1' at $xy" >> out/steps.txt; return 0; fi
  echo "NOT FOUND: '$1'" >> out/steps.txt; return 1
}

adb shell settings put global hide_error_dialogs 1
adb install "$APK"
adb logcat -c
adb shell monkey -p $PKG -c android.intent.category.LAUNCHER 1
sleep 35
tap_text "Wait" || true
shot 01-welcome 2
tap_text "Not now"; shot 02-overview
tap_text "Chat"; shot 03-chat
adb shell input tap 540 2000; shot 04-chat-keyboard 5
adb shell input keyevent KEYCODE_BACK; sleep 2
tap_text "Images"; shot 05-images
tap_text "Earnings"; shot 06-earnings
tap_text "More"; shot 07-more
tap_text "Network"; shot 08-network 8
tap_text "More"; tap_text "Settings"; shot 09-settings
tap_text "More"; tap_text "Models"; shot 10-models 6

adb shell pidof $PKG > out/pid.txt || echo "NOT RUNNING" > out/pid.txt
adb shell wm size > out/size.txt
adb logcat -d > out/logcat.txt
grep -E "$(cat out/pid.txt) " out/logcat.txt | grep -iE 'FATAL|panicked|Console|E Tauri|Exception' | tail -80 > out/logcat-short.txt || true
cat out/steps.txt out/pid.txt
