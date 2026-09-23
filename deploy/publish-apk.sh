#!/bin/sh
# Build + publish the skadi-android APK for LAN install (SKADI-T-0340).
#
# Builds the signed release APK, drops it into deploy/apk/ (mounted into the
# skadi container at /apk = SKADI_APK_DIR), and writes the manifest.json the
# daemon's /app/* + install-QR endpoints read. Re-run after app changes; the
# desktop UI's "Install the app" QR always points at the manifest's file.
#
# Build modes (SKADI-T-0345):
#   docker (DEFAULT) — gradle runs in an Android-SDK builder image, so a host
#     with ONLY Docker (no Android SDK / JDK 17) can produce the APK. A named
#     volume caches ~/.gradle so re-builds don't re-download ~2GB each time.
#   host            — use the host's ./gradlew (needs the Android SDK + JDK 17).
# Select with `--host` / `--docker`, or SKADI_APK_BUILD=host|docker.
# Override the image with SKADI_ANDROID_SDK_IMAGE.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
android="$here/../clients/skadi-android"
out="$here/apk"

mode="${SKADI_APK_BUILD:-docker}"
case "${1:-}" in
  --host) mode=host ;;
  --docker) mode=docker ;;
  "") : ;;
  *) echo "usage: publish-apk.sh [--docker|--host]" >&2; exit 2 ;;
esac
image="${SKADI_ANDROID_SDK_IMAGE:-ghcr.io/cirruslabs/android-sdk:34}"
# Android's Linux build tools are x86_64; build amd64 (native on amd64 hosts,
# emulated on Apple Silicon). Override if you have a true-arm64 SDK image.
platform="${SKADI_ANDROID_BUILD_PLATFORM:-linux/amd64}"

build_host() {
  ( cd "$android" && ./gradlew --no-daemon -q assembleRelease )
}

build_docker() {
  command -v docker >/dev/null 2>&1 || {
    echo "docker not found — install Docker, or re-run with --host if you have the Android SDK + JDK 17" >&2
    exit 1
  }
  echo "building the APK in $image on $platform (gradle cache: skadi-gradle-cache volume)…" >&2
  # The Android Linux build tools (AAPT2) are x86_64, so the build targets amd64.
  # On a normal amd64 Docker/CI/deploy host this is NATIVE (the intended use). On
  # an Apple-Silicon (arm64) dev machine, Docker runs it under emulation — slower,
  # but you'd build the APK on the deploy host, not your laptop. Override arch with
  # SKADI_ANDROID_BUILD_PLATFORM if you ever get a true-arm64 SDK image.
  # The repo's android dir is the Gradle root; keystore.properties + the keystore
  # live inside it, so the container signs without any extra mounts. Outputs land
  # in app/build/ on the host (root-owned — harmless; publish only reads them).
  docker run --rm \
    --platform "$platform" \
    -v "$android":/workspace \
    -v skadi-gradle-cache:/root/.gradle \
    -w /workspace \
    "$image" \
    ./gradlew --no-daemon -q assembleRelease
}

case "$mode" in
  host) build_host ;;
  docker) build_docker ;;
  *) echo "unknown build mode: $mode (want docker|host)" >&2; exit 2 ;;
esac

meta="$android/app/build/outputs/apk/release/output-metadata.json"
apk="$android/app/build/outputs/apk/release/app-release.apk"
unsigned="$android/app/build/outputs/apk/release/app-release-unsigned.apk"

# An unconfigured signing keystore yields an -unsigned APK that phones refuse
# to install — diagnose that specifically rather than "no APK produced".
if [ ! -f "$apk" ] && [ -f "$unsigned" ]; then
  echo "release APK is UNSIGNED — set up keystore.properties (see keystore.properties.example)" >&2
  exit 1
fi
[ -f "$apk" ] || { echo "no release APK produced (gradle build failed?)" >&2; exit 1; }

# Fail loudly on a parse miss instead of silently publishing skadi-0.apk.
version_name=$(sed -n 's/.*"versionName": *"\([^"]*\)".*/\1/p' "$meta" | head -1)
version_code=$(sed -n 's/.*"versionCode": *\([0-9]*\).*/\1/p' "$meta" | head -1)
[ -n "$version_code" ] || { echo "could not parse versionCode from $meta" >&2; exit 1; }
file="skadi-${version_code}.apk"

# New-before-old: copy to a temp name, write the manifest, THEN remove the
# previous APKs — so a failed cp never leaves manifest.json pointing at a file
# that no longer exists (a persistent 404 on /app/*).
mkdir -p "$out"
cp "$apk" "$out/$file.tmp"
mv "$out/$file.tmp" "$out/$file"
cat > "$out/manifest.json" <<EOF
{"file":"$file","version_name":"${version_name:-unknown}","version_code":${version_code}}
EOF
for old in "$out"/skadi-*.apk; do
  [ "$old" = "$out/$file" ] || rm -f "$old"
done
echo "published $file (v${version_name:-?}) to $out"
