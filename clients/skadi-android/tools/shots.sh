#!/usr/bin/env bash
# Boot an emulator, pair the app against a running skadi, and capture screenshots
# (SKADI-T-0571).
#
# Why this exists: the Android UI had no way to be *looked at* without a physical
# device. SKADI-T-0336/T-0344 (real-device passes) are blocked on hardware, so
# every UI change has been made blind. This does not replace a device pass — it
# cannot prove QR scanning, doze, or lockscreen transport — but it does let a
# change to a Compose screen be seen before it ships.
#
# Apple Silicon: uses the **arm64** system image, so the emulator runs natively.
# (The APK build under docker is x86_64-emulated and slow; that is a separate
# concern — see deploy/publish-apk.sh.)
#
# Usage:
#   tools/shots.sh                      # against http://10.0.2.2:8090 (host's prod)
#   SKADI_HOST=10.0.2.2:8091 tools/shots.sh   # the lab stack instead
#   SKADI_SHOTS_OUT=/tmp/shots tools/shots.sh
#
# PRIVACY: screenshots show whatever library the server has. Pointed at a real
# library they contain real titles and covers — do not commit those to a public
# repo. Point it at a lab seeded with synthetic data if the images are to be
# shared. Output goes to a gitignored directory by default for that reason.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
android="$here/.."
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"

AVD="${SKADI_AVD:-skadi-shots}"
IMAGE="${SKADI_AVD_IMAGE:-system-images;android-34;google_apis;arm64-v8a}"
HOSTPORT="${SKADI_HOST:-10.0.2.2:8090}"   # 10.0.2.2 is the emulator's route to the host
OUT="${SKADI_SHOTS_OUT:-$android/build/shots}"
APK="${SKADI_APK:-$android/app/build/outputs/apk/debug/app-debug.apk}"
PKG=com.skadi.app

need() { command -v "$1" >/dev/null || { echo "missing: $1 (is the SDK installed?)" >&2; exit 1; }; }
need adb; need emulator; need avdmanager

mkdir -p "$OUT"

# --- the AVD -------------------------------------------------------------
if ! avdmanager list avd 2>/dev/null | grep -q "Name: $AVD"; then
  echo "creating AVD $AVD …" >&2
  echo no | avdmanager create avd -n "$AVD" -k "$IMAGE" -d pixel_6 >/dev/null
fi

# --- boot ----------------------------------------------------------------
# Headless (-no-window) so this can run over ssh/CI; drop it to watch it work.
if ! adb devices | grep -q emulator; then
  echo "booting $AVD …" >&2
  emulator -avd "$AVD" -no-window -no-audio -no-snapshot -gpu swiftshader_indirect \
    >"$OUT/emulator.log" 2>&1 &
  adb wait-for-device
fi
# `wait-for-device` returns as soon as adb can talk to it — long before Android is
# usable. Poll the real signal instead, or every command below races the boot.
echo -n "waiting for boot" >&2
for _ in $(seq 1 120); do
  [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ] && break
  echo -n "." >&2; sleep 2
done
echo >&2
[ "$(adb shell getprop sys.boot_completed | tr -d '\r')" = "1" ] || {
  echo "emulator did not finish booting; see $OUT/emulator.log" >&2; exit 1
}
# Kill the "system UI isn't responding" class of dialog and the demo-unfriendly
# status bar clutter before any capture.
adb shell settings put global window_animation_scale 0 || true
adb shell settings put global transition_animation_scale 0 || true
adb shell settings put global animator_duration_scale 0 || true
adb shell cmd statusbar overlay-icons false >/dev/null 2>&1 || true

# --- install -------------------------------------------------------------
[ -f "$APK" ] || { echo "no APK at $APK — build one first (see deploy/publish-apk.sh)" >&2; exit 1; }
echo "installing $(basename "$APK") …" >&2
adb install -r -g "$APK" >/dev/null

shot() {  # shot <name> [settle-seconds]
  sleep "${2:-2}"
  adb exec-out screencap -p > "$OUT/$1.png"
  printf "  %-28s %s\n" "$1.png" "$(wc -c < "$OUT/$1.png" | tr -d ' ') bytes" >&2
}

# --- drive ---------------------------------------------------------------
adb shell pm clear "$PKG" >/dev/null 2>&1 || true
adb shell am start -n "$PKG/.MainActivity" >/dev/null
# The pairing screen runs a 5 s NSD discovery first and only then offers the
# manual field, so anything typed before that is dropped.
sleep 8
shot 01-pairing 0

# Manual pair. The daemon issues the token itself, so host:port is all it needs.
# `input text` needs the colon escaped or it is swallowed.
adb shell input text "${HOSTPORT/:/%3A}"
# **Dismiss the IME before tapping anything.** Gboard opens a clipboard
# suggestion panel over the lower third of the screen, so a tap aimed at
# "Connect" lands on the keyboard instead — which is exactly what happened the
# first time this was run by hand, and the screenshot showed the panel rather
# than the library.
adb shell input keyevent 4
sleep 1
shot 02-pairing-filled 1
# "Connect" sits below the field; tap by text position rather than coordinates
# where possible — uiautomator gives us the bounds.
adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1 || true
adb pull /sdcard/ui.xml "$OUT/ui-pairing.xml" >/dev/null 2>&1 || true
CONNECT=$(python3 - "$OUT/ui-pairing.xml" <<'PY' 2>/dev/null || true
import re,sys
try: xml=open(sys.argv[1]).read()
except OSError: sys.exit()
m=re.search(r'text="Connect"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml)
if m:
    x1,y1,x2,y2=map(int,m.groups()); print((x1+x2)//2,(y1+y2)//2)
PY
)
if [ -n "$CONNECT" ]; then adb shell input tap $CONNECT; else adb shell input keyevent 66; fi

shot 03-library 6
# Series view — the segmented control's third segment.
adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1 || true
adb pull /sdcard/ui.xml "$OUT/ui-library.xml" >/dev/null 2>&1 || true
tap_text() {
  local t="$1" xml="$OUT/ui-library.xml"
  local c
  c=$(python3 - "$xml" "$t" <<'PY' 2>/dev/null || true
import re,sys
try: xml=open(sys.argv[1]).read()
except OSError: sys.exit()
m=re.search(r'text="%s"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"' % re.escape(sys.argv[2]), xml)
if m:
    x1,y1,x2,y2=map(int,m.groups()); print((x1+x2)//2,(y1+y2)//2)
PY
)
  [ -n "$c" ] && adb shell input tap $c && return 0
  return 1
}
tap_text "Series" && shot 04-library-series 2
tap_text "Downloaded" && shot 05-library-downloaded 2
tap_text "Storage" && shot 06-storage 2

echo "screenshots in $OUT" >&2
