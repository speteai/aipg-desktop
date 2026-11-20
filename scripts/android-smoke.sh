#!/usr/bin/env bash
# Start check for the Android build, run inside the CI emulator:
# installs the APK, opens the app, walks through the main views and saves a
# screenshot of each, plus the log.
set -x
PKG=io.github.speteai.aipgmobile
mkdir -p out
APK=$(find apk -name '*x86_64*.apk' | head -1)

shot() { sleep "${2:-4}"; adb exec-out screencap -p > "out/$1.png"; }
back() { adb shell input keyevent KEYCODE_BACK; }
front() { adb shell dumpsys activity activities | grep -m1 -E 'topResumedActivity|mResumedActivity' | grep -c "$PKG"; }
# tap the first on-screen element whose text is $1 ("Abc*" = starts with Abc);
# upper and lower case are not told apart, because uppercase styling reaches
# the accessibility text
tap_text() {
  adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
  adb pull /sdcard/ui.xml out/ui.xml >/dev/null 2>&1
  local xy
  xy=$(python3 - "$1" <<'PY'
import re, sys, xml.etree.ElementTree as ET
want = sys.argv[1].lower()
try:
    root = ET.parse("out/ui.xml").getroot()
except Exception:
    sys.exit(0)
for n in root.iter("node"):
    t = " ".join((n.get("text") or n.get("content-desc") or "").split()).lower()
    if t == want or (want.endswith("*") and t.startswith(want[:-1])):
        a = list(map(int, re.findall(r"\d+", n.get("bounds"))))
        print((a[0] + a[2]) // 2, (a[1] + a[3]) // 2)
        break
PY
)
  if [ -n "$xy" ]; then adb shell input tap $xy; echo "tapped '$1' at $xy" >> out/steps.txt; return 0; fi
  echo "NOT FOUND: '$1'" >> out/steps.txt
  cp out/ui.xml "out/ui-missing-$(date +%s).xml" 2>/dev/null
  return 1
}

adb shell settings put global hide_error_dialogs 1
adb install "$APK"
adb logcat -c
adb shell monkey -p $PKG -c android.intent.category.LAUNCHER 1
# the first seconds, several frames a second: is the start logo there?
for n in $(seq -w 1 36); do
  ( sleep "$(python3 -c "print(int('$n') * 0.4)")"; adb exec-out screencap -p > "out/00-start-$n.png" ) &
done
wait
sleep 18
tap_text "Wait" || true
shot 01-welcome 2
tap_text "Not now"; shot 02-start-chat
if tap_text "Model*"; then shot 03-model-picker 2; back; sleep 2; fi
if tap_text "Options"; then shot 04-chat-options 2; back; sleep 2; fi
tap_text "Images"; shot 05-images
# the switch for a copy in the phone's gallery: on, look at the folders, off again
adb shell input swipe 500 1500 500 600 300; sleep 2
if tap_text "Also keep*"; then
  shot 05a-shared-copy-on 3
  { echo "shared folders after switching on:"; adb shell ls -ld /sdcard/Pictures/AIPG /sdcard/Movies/AIPG; adb shell ls -la /sdcard/Pictures/AIPG /sdcard/Movies/AIPG; } >> out/steps.txt 2>&1
  tap_text "Also keep*"; shot 05a-shared-copy-off 3
fi
tap_text "Video"; shot 05b-video
tap_text "Workers"; shot 06-workers
tap_text "More"; shot 07-more
if tap_text "Models *"; then
  shot 08-models 6
  # back: sub-page -> More -> start tab (Chat) -> out of the app
  back; shot 09-back-1-expect-more 2
  back; shot 10-back-2-expect-chat 2
  echo "after 2x back, app in front: $(front)" >> out/steps.txt
  back; shot 11-back-3-expect-home-screen 3
  echo "after 3x back, app in front: $(front)" >> out/steps.txt
fi

adb shell pidof $PKG > out/pid.txt || echo "NOT RUNNING" > out/pid.txt
adb shell wm size > out/size.txt
adb logcat -d > out/logcat.txt
grep -E "$(cat out/pid.txt) " out/logcat.txt | grep -iE 'FATAL|panicked|Console|E Tauri|Exception' | tail -80 > out/logcat-short.txt || true
cat out/steps.txt out/pid.txt
