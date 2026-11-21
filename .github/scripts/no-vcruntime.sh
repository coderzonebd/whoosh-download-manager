#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Fails if a Windows executable imports the Visual C++ runtime (VCRUNTIME140.dll,
# MSVCP140.dll): a fresh Windows does not have it, and the program then does
# not start at all. The C runtime is linked statically instead
# (.cargo/config.toml, RUSTFLAGS in release.yml, tauri-build for the app).
set -euo pipefail
status=0
for exe in "$@"; do
  if grep -aqiE 'vcruntime140|msvcp140' "$exe"; then
    echo "::error::$exe needs the Visual C++ runtime (VCRUNTIME140.dll); link the C runtime statically"
    status=1
  else
    echo "ok: $exe"
  fi
done
exit "$status"
