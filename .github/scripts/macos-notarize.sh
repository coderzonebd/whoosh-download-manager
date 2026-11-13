#!/usr/bin/env bash
# Submits a signed .dmg or .zip to Apple's notary service and waits for the
# verdict; a .dmg is stapled afterwards. Bare Mach-O binaries cannot be
# stapled: zip them, notarize the zip, ship the binaries (Gatekeeper checks
# the ticket online).
#
#   macos-notarize.sh <file.dmg|file.zip>
#
# Uses APPLE_API_KEY + APPLE_API_ISSUER + APPLE_API_KEY_PATH, or
# APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID (written by apple-signing-env.sh).
# Without either set it prints a warning and exits 0.
set -euo pipefail

file=${1:?usage: macos-notarize.sh <file>}

if [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_KEY_PATH:-}" ]; then
  auth=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "${APPLE_API_ISSUER:?}")
elif [ -n "${APPLE_ID:-}" ]; then
  auth=(--apple-id "$APPLE_ID" --password "${APPLE_PASSWORD:?}" --team-id "${APPLE_TEAM_ID:?}")
else
  echo "::warning::No notarization credentials; $file is signed but not notarized."
  exit 0
fi

out=$(mktemp)
xcrun notarytool submit "$file" "${auth[@]}" --wait --timeout 30m --output-format json | tee "$out"
field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], ""))' "$out" "$1"; }
status=$(field status)
if [ "$status" != "Accepted" ]; then
  id=$(field id)
  [ -n "$id" ] && xcrun notarytool log "$id" "${auth[@]}" || true
  echo "::error::Notarization of $file finished with status '$status'"
  exit 1
fi

case "$file" in
  *.dmg) xcrun stapler staple "$file" && xcrun stapler validate "$file" ;;
esac
