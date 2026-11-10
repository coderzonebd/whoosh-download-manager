#!/usr/bin/env bash
# Signs files with the Developer ID certificate (hardened runtime, secure
# timestamp). Used for the standalone binaries and the .dmg; the .app inside
# the .dmg is signed by the Tauri CLI itself.
#
#   macos-sign.sh <file>...
#
# Needs APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD and
# APPLE_SIGNING_IDENTITY (written by apple-signing-env.sh).
set -euo pipefail

: "${APPLE_CERTIFICATE:?}" "${APPLE_SIGNING_IDENTITY:?}" "${RUNNER_TEMP:?}"

keychain="$RUNNER_TEMP/vayu-signing.keychain-db"
if [ ! -f "$keychain" ]; then
  pw=$(openssl rand -hex 24)
  security create-keychain -p "$pw" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$pw" "$keychain"
  printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$RUNNER_TEMP/vayu-cert.p12"
  security import "$RUNNER_TEMP/vayu-cert.p12" -k "$keychain" -f pkcs12 \
    -P "${APPLE_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign
  rm -f "$RUNNER_TEMP/vayu-cert.p12"
  security set-key-partition-list -S apple-tool:,apple: -s -k "$pw" "$keychain" >/dev/null
  # Put the keychain first in the search list so codesign finds the identity.
  # shellcheck disable=SC2046
  security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
fi

for f in "$@"; do
  case "$f" in
    *.dmg) codesign --force --timestamp --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" "$f" ;;
    *) codesign --force --timestamp --options runtime --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" "$f" ;;
  esac
  codesign --verify --strict --verbose=2 "$f"
done
