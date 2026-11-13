#!/usr/bin/env bash
# Run a command (usually cargo) while holding one of the machine-wide build
# slots from ADR 0004, so parallel sessions never compile Rust more than
# WHOOSH_BUILD_SLOTS (default 2) at a time.
#
#   scripts/cargo-slot.sh cargo test -p whoosh-net
#
# Linux/macOS development machines only. Uses `flock` when it is installed
# (util-linux); otherwise an atomic `mkdir` lock with the holder's pid, so
# it also runs on a stock macOS (bash 3.2, no flock).
set -euo pipefail
slots=${WHOOSH_BUILD_SLOTS:-2}
dir=${XDG_RUNTIME_DIR:-/tmp}/whoosh-build-slots
mkdir -p "$dir"

# Tries slot $1; on success runs the command and exits with its status.
try_slot() {
  local slot="$dir/slot$1"
  shift
  if command -v flock >/dev/null 2>&1; then
    exec 9>"$slot"
    if flock -n 9; then
      # The child inherits the locked descriptor; the slot frees when the
      # command and anything it left running exit.
      "$@"
      exit $?
    fi
    exec 9>&-
    return 0
  fi
  # mkdir is atomic: whoever creates the directory holds the slot. A slot
  # whose holder died without cleaning up is taken over.
  if ! mkdir "$slot.d" 2>/dev/null; then
    local pid
    pid=$(cat "$slot.d/pid" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      return 0
    fi
    rm -rf "$slot.d"
    mkdir "$slot.d" 2>/dev/null || return 0
  fi
  echo $$ >"$slot.d/pid"
  # shellcheck disable=SC2064 # expand now: $slot is local to this function
  trap "rm -rf '$slot.d'" EXIT
  trap 'exit 130' INT TERM
  local status=0
  "$@" || status=$?
  exit "$status"
}

waited=0
while :; do
  i=1
  while [ "$i" -le "$slots" ]; do
    try_slot "$i" "$@"
    i=$((i + 1))
  done
  if [ "$waited" -eq 0 ]; then
    echo "cargo-slot: all $slots build slots busy, waiting…" >&2
    waited=1
  fi
  sleep 2
done
