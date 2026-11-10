#!/usr/bin/env bash
# Run a command (usually cargo) while holding one of the machine-wide build
# slots from ADR 0004, so parallel sessions never compile Rust more than
# VAYU_BUILD_SLOTS (default 2) at a time.
#
#   scripts/cargo-slot.sh cargo test -p vayu-net
#
# Linux/macOS development machines only; needs `flock` (util-linux).
set -euo pipefail
slots=${VAYU_BUILD_SLOTS:-2}
dir=${XDG_RUNTIME_DIR:-/tmp}/vayu-build-slots
mkdir -p "$dir"
waited=0
while :; do
  for i in $(seq 1 "$slots"); do
    exec {fd}>"$dir/slot$i"
    if flock -n "$fd"; then
      # The child inherits the locked descriptor; the slot frees when the
      # command and anything it left running exit.
      "$@"
      exit $?
    fi
    exec {fd}>&-
  done
  if [ "$waited" -eq 0 ]; then
    echo "cargo-slot: all $slots build slots busy, waiting…" >&2
    waited=1
  fi
  sleep 2
done
