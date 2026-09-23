#!/bin/sh
# Advertise skadi over mDNS/DNS-SD from the DOCKER HOST (SKADI-T-0339).
#
# Why host-side: the skadi container runs in Docker bridge networking, and on
# Docker Desktop for Mac even network_mode:host binds to the hidden VM — LAN
# multicast can never leave the container. But macOS itself IS a Bonjour
# machine, so the host advertises on the container's behalf. The Android app
# (SKADI-I-0049, pairing module) browses `_skadi._tcp` and resolves this.
#
# Usage:  ./advertise-mdns.sh [port]
#         Port defaults to SKADI_PORT from ./.env (else 8080) — so it always
#         matches what compose actually publishes. Runs in the FOREGROUND
#         (Ctrl-C to stop); it dies with the terminal/reboot. For boot
#         persistence, install it as a launchd agent — example plist:
#
#   ~/Library/LaunchAgents/com.skadi.mdns.plist
#   <plist version="1.0"><dict>
#     <key>Label</key><string>com.skadi.mdns</string>
#     <key>ProgramArguments</key>
#       <array><string>/usr/bin/dns-sd</string><string>-R</string>
#       <string>skadi</string><string>_skadi._tcp</string><string>.</string>
#       <string>8080</string></array>
#     <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
#   </dict></plist>
#   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.skadi.mdns.plist
#
# The in-daemon advertisement (SKADI-T-0340, SKADI_MDNS_ADVERTISE=1) covers
# Linux/bare-metal deployments; this script is the Docker-on-Mac answer.
env_port="$(sed -n 's/^SKADI_PORT=//p' "$(dirname "$0")/.env" 2>/dev/null | head -1)"
PORT="${1:-${env_port:-8080}}"
echo "advertising _skadi._tcp on port ${PORT} (Ctrl-C to stop)"
exec dns-sd -R "skadi" _skadi._tcp . "${PORT}"
