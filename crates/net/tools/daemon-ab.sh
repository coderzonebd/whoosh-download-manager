#!/usr/bin/env bash
# Interleaved A/B of whole-daemon CPU per GiB, measured as vayu-bench's
# resident-daemon mode does: per run a fresh `vayu-daemon --port 0` with its
# own data dir, then `vayu --no-start get`; CPU = the CLI's user+sys plus the
# daemon's utime+stime (/proc) from just before the CLI starts until it goes
# quiet after the CLI exits.
#
#   tools/daemon-ab.sh <rounds> <variant>... -- <scenario/file>...
#
# A variant is a directory under $VARIANTS (default target/ab) holding `vayu`
# and `vayu-daemon`, optionally followed by `:K=V,K=V` extra environment for
# the daemon (e.g. `old:GLIBC_TUNABLES=glibc.malloc.trim_threshold=1073741824`).
# Each round runs every variant once, in alternating order. Output: one line
# per run (wall, CLI and daemon CPU, CPU-s/GiB, size, daemon page faults,
# load average). Linux only.
set -u
VARIANTS=${VARIANTS:-target/ab}
LABSERVER=${LABSERVER:-target/release/vayu-labserver}
WORK=${WORK:-$HOME/.cache/vayu-bench/work/ab-net}
rounds=$1; shift; vs=(); while [ "$1" != "--" ]; do vs+=("$1"); shift; done; shift
PORT=$((20000 + RANDOM % 20000))
"$LABSERVER" --port $PORT --no-tls --cache-mb 256 > "$WORK.lab.log" 2>&1 &
LAB=$!
trap "kill $LAB 2>/dev/null; wait $LAB 2>/dev/null" EXIT
mkdir -p "$WORK"
for i in $(seq 1 100); do curl -sf http://127.0.0.1:$PORT/_lab/health >/dev/null && break; sleep 0.1; done
CLK=$(getconf CLK_TCK)
dcpu() { awk '{print $14+$15}' /proc/$1/stat; }
dflt() { awk '{print $10}' /proc/$1/stat; }
run_one() {
  local v=$1 sf=$2 st=$WORK/ab-$$-$RANDOM dir=${1%%:*} extra=""
  [ "$dir" != "$v" ] && extra=${v#*:}
  mkdir -p $st/home/run $st/out; chmod 700 $st/home/run
  local envs=(HOME=$st/home XDG_CONFIG_HOME=$st/home/.config XDG_DATA_HOME=$st/home/.local/share
              XDG_CACHE_HOME=$st/home/.cache XDG_RUNTIME_DIR=$st/home/run VAYU_DATA_DIR=$st/data
              PATH=/usr/bin:/bin ${extra//,/ })
  env -i "${envs[@]}" "$VARIANTS/$dir/vayu-daemon" --port 0 > $st/daemon.log 2>&1 & local d=$!
  for k in $(seq 1 3000); do [ -s $st/data/runtime.json ] && break; sleep 0.01; done
  curl -s -X POST http://127.0.0.1:$PORT/_lab/reset >/dev/null
  local c0 f0 f1 c1 prev=-1; c0=$(dcpu $d); f0=$(dflt $d)
  env -i "${envs[@]}" /usr/bin/time -o $st/time -f "%e %U %S" "$VARIANTS/$dir/vayu" --no-start get \
    http://127.0.0.1:$PORT/s/$sf -o $st/out >/dev/null 2>$st/cli.err
  for k in $(seq 1 10); do c1=$(dcpu $d); [ "$c1" = "$prev" ] && break; prev=$c1; sleep 0.05; done
  f1=$(dflt $d)
  kill $d; wait $d 2>/dev/null
  local size; size=$(find $st/out -type f -printf '%s\n' | head -1)
  read wall u s < $st/time
  awk -v v="$v" -v sf=$sf -v w=$wall -v u=$u -v s=$s -v dt=$((c1-c0)) -v clk=$CLK -v sz=${size:-0} \
      -v flt=$((f1-f0)) -v l="$(cut -d' ' -f1 /proc/loadavg)" \
    'BEGIN{cpu=u+s+dt/clk; gib=sz/1073741824; printf "%-28s %-20s wall %6.3f  cli %5.2f  daemon %5.2f  cpu/GiB %5.2f  size %d  faults %6d  load %s\n", v, sf, w, u+s, dt/clk, (gib>0?cpu/gib:-1), sz, flt, l}'
  rm -rf $st
}
for sf in "$@"; do
  for r in $(seq 1 $rounds); do
    order=("${vs[@]}")
    if [ $((r % 2)) -eq 0 ]; then order=(); for ((x=${#vs[@]}-1; x>=0; x--)); do order+=("${vs[$x]}"); done; fi
    for v in "${order[@]}"; do run_one "$v" $sf; done
  done
done
