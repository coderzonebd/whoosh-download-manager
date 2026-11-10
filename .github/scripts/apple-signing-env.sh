#!/usr/bin/env bash
# Exports the Apple signing / notarization secrets into $GITHUB_ENV under the
# names the Tauri CLI and macos-sign.sh / macos-notarize.sh read:
#   APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD, APPLE_SIGNING_IDENTITY
#   APPLE_API_KEY, APPLE_API_ISSUER, APPLE_API_KEY_PATH   (App Store Connect key), or
#   APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID               (Apple ID + app-specific password)
#
# Input (step env, from secrets; see docs/release.md):
#   CERT, CERT_PASSWORD, IDENTITY, TEAM_ID,
#   API_KEY_ID, API_ISSUER, API_KEY_P8, APPLE_ID_EMAIL, APPLE_ID_PASSWORD
#
# Variables are only written when non-empty: the Tauri CLI treats a set but
# empty variable as configured.
set -euo pipefail

: "${GITHUB_ENV:?}" "${RUNNER_TEMP:?}"
: "${CERT:?APPLE_CERTIFICATE secret is empty}"
: "${IDENTITY:?APPLE_SIGNING_IDENTITY secret is empty}"

{
  # base64 -i cert.p12 may wrap lines; the CLI wants one line.
  echo "APPLE_CERTIFICATE=$(printf '%s' "$CERT" | tr -d '\r\n ')"
  echo "APPLE_CERTIFICATE_PASSWORD=${CERT_PASSWORD:-}"
  echo "APPLE_SIGNING_IDENTITY=$IDENTITY"
} >>"$GITHUB_ENV"

if [ -n "${API_KEY_ID:-}" ] && [ -n "${API_ISSUER:-}" ] && [ -n "${API_KEY_P8:-}" ]; then
  key_path="$RUNNER_TEMP/AuthKey_${API_KEY_ID}.p8"
  umask 077
  printf '%s\n' "$API_KEY_P8" >"$key_path"
  {
    echo "APPLE_API_KEY=$API_KEY_ID"
    echo "APPLE_API_ISSUER=$API_ISSUER"
    echo "APPLE_API_KEY_PATH=$key_path"
  } >>"$GITHUB_ENV"
  echo "notarization: App Store Connect API key"
elif [ -n "${APPLE_ID_EMAIL:-}" ] && [ -n "${APPLE_ID_PASSWORD:-}" ] && [ -n "${TEAM_ID:-}" ]; then
  {
    echo "APPLE_ID=$APPLE_ID_EMAIL"
    echo "APPLE_PASSWORD=$APPLE_ID_PASSWORD"
    echo "APPLE_TEAM_ID=$TEAM_ID"
  } >>"$GITHUB_ENV"
  echo "notarization: Apple ID"
else
  echo "::warning::Developer ID certificate present but no notarization credentials; builds will be signed, not notarized."
fi
