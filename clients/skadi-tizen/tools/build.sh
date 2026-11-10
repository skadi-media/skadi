#!/bin/sh
# Build Skadi.wgt from the app's files, leaving tests and tools out.
#
# Usage: tools/build.sh [signing-profile]
# Needs the `tizen` CLI on PATH (Tizen Studio: tools/ide/bin) and a signing
# profile (see the README). Writes build/wgt/Skadi.wgt.
set -eu
cd "$(dirname "$0")/.."
PROFILE="${1:-skadidev}"
OUT=build/wgt
mkdir -p "$OUT/css" "$OUT/js"
cp config.xml index.html "$OUT/"
cp css/*.css "$OUT/css/"
cp js/*.js "$OUT/js/"
# An earlier signature in the folder makes `tizen package` fail.
for f in author-signature.xml signature1.xml .manifest.tmp Skadi.wgt; do
    if [ -e "$OUT/$f" ]; then unlink "$OUT/$f"; fi
done
tizen package -t wgt -s "$PROFILE" -- "$OUT"
echo "built $OUT/Skadi.wgt"
