#!/usr/bin/env bash
# Smoke test of the release installers on a real machine of each platform (release.yml, job
# `smoke`): installs the desktop app the way a person would, then checks what a person would
# notice first.
#
#   1. The installer installs (Linux .deb through apt, Windows NSIS silently, macOS .dmg).
#   2. The installed daemon starts with media support, and the CLI downloads a file through it
#      (size checked).
#   3. The daemon fetches yt-dlp, ffmpeg and Deno by itself in the background (the installers
#      ship none), and those download a YouTube video and merge video and audio (the YouTube
#      part is a warning only: YouTube sometimes asks datacenter machines to sign in).
#   4. The app's window opens; a screenshot is kept as an artifact.
#
# Usage: smoke-test.sh <desktop-artifact-dir> <binaries-artifact-dir> <out-dir>
set -euo pipefail

desktop=$1
binaries=$2
out=$3
mkdir -p "$out"
work=$(mktemp -d)
log() { printf '\n== %s\n' "$*"; }
fail() { echo "::error::$*"; exit 1; }
# macOS has no `timeout` unless coreutils is installed.
limit() { if command -v timeout > /dev/null; then timeout "$@"; elif command -v gtimeout > /dev/null; then gtimeout "$@"; else shift; "$@"; fi; }

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  MINGW* | MSYS* | CYGWIN*) os=windows ;;
  *) fail "unknown system $(uname -s)" ;;
esac
exe=""; [ "$os" = windows ] && exe=.exe

# ---------------------------------------------------------------- 1. install
log "install ($os)"
case "$os" in
  linux)
    deb=$(ls "$desktop"/*.deb | head -n 1)
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$deb" xvfb imagemagick > /dev/null
    app_bin=$(command -v wdm-desktop)
    daemon=$(command -v wdm-daemon)
    ;;
  macos)
    dmg=$(ls "$desktop"/*.dmg | head -n 1)
    mnt=$(hdiutil attach -nobrowse -readonly "$dmg" | awk -F'\t' '/\/Volumes\//{print $NF}' | tail -n 1)
    cp -R "$mnt"/*.app /Applications/
    hdiutil detach "$mnt" > /dev/null
    app=$(ls -d /Applications/*[Ww]hoosh*.app | head -n 1)
    app_bin="$app/Contents/MacOS/wdm-desktop"
    daemon="$app/Contents/MacOS/wdm-daemon"
    ;;
  windows)
    setup=$(ls "$desktop"/*-setup.exe | head -n 1)
    # NSIS, per-user, silent; Start-Process waits for the installer to finish.
    powershell -NoProfile -Command "Start-Process -FilePath '$(cygpath -w "$setup")' -ArgumentList '/S' -Wait"
    dir=$(cygpath -u "$LOCALAPPDATA")/"Whoosh Download Manager"
    [ -d "$dir" ] || dir=$(dirname "$(find "$(cygpath -u "$LOCALAPPDATA")" "$(cygpath -u "$PROGRAMFILES")" -maxdepth 3 -name 'wdm-daemon.exe' 2>/dev/null | head -n 1)")
    app_bin="$dir/wdm-desktop.exe"
    daemon="$dir/wdm-daemon.exe"
    ;;
esac
for f in "$app_bin" "$daemon"; do
  [ -e "$f" ] || fail "not installed: $f"
done
echo "app: $app_bin"; echo "daemon: $daemon"

# The CLI comes from the release's CLI archive.
archive=$(ls "$binaries"/wdm-[0-9]*.tar.gz "$binaries"/wdm-[0-9]*.zip 2>/dev/null | head -n 1)
[ -n "$archive" ] || fail "no CLI archive in $binaries"
case "$archive" in
  *.zip) unzip -q "$archive" -d "$work/cli" ;;
  *) mkdir -p "$work/cli" && tar -xzf "$archive" -C "$work/cli" ;;
esac
cli=$(find "$work/cli" -type f -name "wdm$exe" | head -n 1)
[ -n "$cli" ] || fail "no wdm$exe in $archive"
chmod +x "$cli" 2>/dev/null || true

# ---------------------------------------------------------- 2. daemon and CLI
log "daemon"
data="$work/data"
mkdir -p "$data" "$work/downloads"
export WDM_DATA_DIR="$data"
"$daemon" --data-dir "$data" --port 0 > "$out/daemon.log" 2>&1 &
daemon_pid=$!
trap 'kill $daemon_pid 2>/dev/null || true' EXIT
for _ in $(seq 1 60); do
  [ -s "$data/runtime.json" ] && break
  sleep 1
done
[ -s "$data/runtime.json" ] || { cat "$out/daemon.log"; fail "the daemon did not start"; }
export WDM_NO_START=1
"$cli" daemon status | tee "$out/status.txt"
for _ in $(seq 1 30); do
  grep -q "media downloads available" "$out/daemon.log" && break
  sleep 1
done
grep -q "media downloads available" "$out/daemon.log" \
  || { cat "$out/daemon.log"; fail "the daemon has no media support"; }

log "download a file through the daemon"
url="https://ash-speed.hetzner.com/10MB.bin"
dl="$work/downloads"; [ "$os" = windows ] && dl=$(cygpath -w "$dl")
saved=$("$cli" get --dir "$dl" "$url" | tail -n 1) \
  || { cat "$out/daemon.log"; fail "wdm get failed"; }
[ "$os" = windows ] && saved=$(cygpath -u "$saved")
size=$(wc -c < "$saved" | tr -d ' ')
[ "$size" = 10485760 ] || fail "downloaded $size bytes, expected 10485760 ($saved)"
echo "ok: $saved ($size bytes)"

# ---------------------------------------- 3. the media tools, fetched by the daemon
log "the daemon fetches yt-dlp, ffmpeg and Deno by itself"
bin="$data/media/bin"
for _ in $(seq 1 120); do
  [ -x "$bin/yt-dlp$exe" ] && [ -x "$bin/ffmpeg$exe" ] && [ -x "$bin/deno$exe" ] && break
  sleep 5
done
ls -l "$bin" || true
for name in yt-dlp ffmpeg deno; do
  [ -x "$bin/$name$exe" ] || { grep -i "fetch\|media" "$out/daemon.log" | tail -n 20; fail "the daemon did not fetch $name"; }
done
"$bin/deno$exe" --version | head -n 1 | tee "$out/deno-version.txt" || fail "the shipped Deno does not run"
"$bin/ffmpeg$exe" -hide_banner -version | head -n 1 || fail "the shipped ffmpeg does not run"
"$bin/yt-dlp$exe" --version || fail "the shipped yt-dlp does not run"
video="$work/video"
if (cd "$work" && limit 300 "$bin/yt-dlp$exe" --ignore-config --no-playlist --no-cache-dir \
      --js-runtimes "deno:$bin/deno$exe" --ffmpeg-location "$bin" \
      -f 'bv*[height<=360]+ba/b[height<=360]' --merge-output-format mkv \
      -o "$video.%(ext)s" "https://www.youtube.com/watch?v=jNQXAC9IVRw" > "$out/yt-dlp.log" 2>&1); then
  # ffprobe where the build has one (not every macOS build does), else ffmpeg's own listing.
  if [ -x "$bin/ffprobe$exe" ]; then
    "$bin/ffprobe$exe" -v error -show_entries stream=codec_type -of csv=p=0 "$video.mkv" | sort | tee "$out/streams.txt"
  else
    "$bin/ffmpeg$exe" -hide_banner -i "$video.mkv" 2>&1 | grep -o 'Stream #[^:]*:[0-9]*[^:]*: [A-Za-z]*' | awk '{print tolower($NF)}' | tee "$out/streams.txt"
  fi
  grep -q video "$out/streams.txt" && grep -q audio "$out/streams.txt" \
    || fail "the merged video lacks a video or an audio stream"
  echo "ok: YouTube video merged"
else
  tail -n 20 "$out/yt-dlp.log"
  echo "::warning::YouTube download failed on this runner (often a sign-in check for datacenter addresses); see yt-dlp.log"
fi

# ---------------------------------------------------------- 4. the window
log "the app's window"
shot="$out/screenshot.png"
case "$os" in
  linux)
    xvfb-run -a -s "-screen 0 1280x800x24" bash -c "
      '$app_bin' > '$out/app.log' 2>&1 &
      sleep 20
      import -window root '$shot'
      kill %1 || true" || echo "::warning::no screenshot"
    ;;
  macos)
    "$app_bin" > "$out/app.log" 2>&1 &
    app_pid=$!
    sleep 20
    screencapture -x "$shot" || echo "::warning::no screenshot"
    kill $app_pid 2>/dev/null || true
    ;;
  windows)
    "$app_bin" > "$out/app.log" 2>&1 &
    app_pid=$!
    sleep 25
    powershell -NoProfile -Command "
      Add-Type -AssemblyName System.Windows.Forms, System.Drawing
      \$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
      \$img = New-Object System.Drawing.Bitmap \$b.Width, \$b.Height
      [System.Drawing.Graphics]::FromImage(\$img).CopyFromScreen(\$b.Location, [System.Drawing.Point]::Empty, \$b.Size)
      \$img.Save('$(cygpath -w "$shot")')" || echo "::warning::no screenshot"
    kill $app_pid 2>/dev/null || true
    taskkill //F //IM wdm-desktop.exe > /dev/null 2>&1 || true
    ;;
esac
[ -f "$shot" ] && echo "screenshot: $shot"
log "smoke test passed ($os)"
