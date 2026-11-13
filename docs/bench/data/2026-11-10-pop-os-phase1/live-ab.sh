#!/bin/bash
# Interleaved A/B on the live link: wire MB/s (NIC rx) and CPU s/GB.
IF=$(ip route show default | awk '{print $5; exit}'); secs=${SECS:-20}
B=~/projects/download-manager/target/release; W=$HOME/.cache/vayu-bench/live
U="${VAYU_LIVE_URL:?set VAYU_LIVE_URL to the test URL}"
rx() { cat /sys/class/net/$IF/statistics/rx_bytes; }
report() { echo "$1 $(echo "scale=1; $2/$secs/1000000" | bc) MB/s cpu=$3 s => $(echo "scale=2; $3/($2/1000000000)" | bc) s/GB $4"; }
vayu() { label=$1; shift
  rm -rf "$W"; mkdir -p "$W/out"; export VAYU_DATA_DIR="$W/data" VAYU_LOG=info,vayu_engine=debug
  env $DAEMON_ENV "$B/vayu-daemon" --port 0 > "$W/d.log" 2>&1 & D=$!
  while [ ! -s "$VAYU_DATA_DIR/runtime.json" ]; do sleep 0.05; done
  r0=$(rx); "$B/vayu" --no-start add "$U" -o "$W/out" "$@" >/dev/null; sleep $secs; r1=$(rx)
  read u s < <(awk '{print $14, $15}' /proc/$D/stat)
  "$B/vayu" --no-start daemon stop >/dev/null; wait $D
  t=$(sed 's/\x1b\[[0-9;]*m//g' "$W/d.log" | grep -o 'to=[0-9]*' | tr '\n' ' ')
  report "$label" $((r1-r0)) $(echo "scale=2; ($u+$s)/100" | bc) "targets: $t"; rm -rf "$W"; }
aria() { rm -rf "$W"; mkdir -p "$W"; r0=$(rx)
  /usr/bin/time -f "%U %S" -o "$W.t" timeout -s INT $secs aria2c -q -x16 -s16 -k1M --file-allocation=none -d "$W" "$U"; r1=$(rx)
  report "aria2c-x16" $((r1-r0)) $(awk '/^[0-9]/{print $1+$2}' "$W.t"); rm -rf "$W" "$W.t"; }
for i in $(seq ${ROUNDS:-4}); do vayu "vayu-c16  " -c 16; aria; vayu "vayu-auto "; done
