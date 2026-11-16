#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Installs Whoosh Download Manager on Linux or macOS with one command:
#
#   curl -fsSL https://raw.githubusercontent.com/coderzonebd/whoosh-download-manager/pub/packaging/install/install.sh | sh
#
# - macOS: Homebrew's cask when Homebrew is there, else the .dmg into /Applications.
# - Debian, Ubuntu, Mint…: the WDM apt repository when it is set up (updates then come with
#   `apt upgrade` too), else the release's .deb through apt.
# - Fedora, openSUSE…: the WDM rpm repository, else the release's .rpm through dnf/zypper.
# - Anything else: the AppImage in ~/.local/bin, with a menu entry.
#
# Every download is checked against the release's SHA256SUMS. The app keeps itself up to date
# afterwards. WDM_VERSION=v1.2.3 installs that release; WDM_CHANNEL=beta prefers pre-releases.
set -eu

REPO="coderzonebd/whoosh-download-manager"
# The signed apt/rpm repository (Cloudsmith, free for open source). Empty until it exists.
APT_REPO="${WDM_APT_REPO-}"
RPM_REPO="${WDM_RPM_REPO-}"
REPO_KEY="${WDM_REPO_KEY-}"
CASK="coderzonebd/tap/whoosh-download-manager"

say() { printf '%s\n' "wdm: $*"; }
die() { printf '%s\n' "wdm: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

if have curl; then fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }; get() { curl -fsSL --retry 3 "$1"; }
elif have wget; then fetch() { wget -qO "$2" "$1"; }; get() { wget -qO- "$1"; }
else die "needs curl or wget"; fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if have sudo; then SUDO="sudo"; elif have doas; then SUDO="doas"; fi
fi
root() { [ -n "$SUDO" ] || [ "$(id -u)" -eq 0 ] || die "needs root for this step (sudo or doas)"; $SUDO "$@"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# The release tag: WDM_VERSION, else the newest stable release, else (before the first stable
# release, or with WDM_CHANNEL=beta) the newest pre-release.
release_tag() {
  if [ -n "${WDM_VERSION-}" ]; then printf '%s\n' "$WDM_VERSION"; return; fi
  api="https://api.github.com/repos/$REPO/releases"
  if [ "${WDM_CHANNEL-}" != "beta" ]; then
    tag="$(get "$api/latest" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)" || tag=""
    [ -n "$tag" ] && { printf '%s\n' "$tag"; return; }
  fi
  get "$api?per_page=20" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1
}

# Downloads release asset $1 into $TMP and checks it against SHA256SUMS.
asset() {
  base="https://github.com/$REPO/releases/download/$TAG"
  [ -f "$TMP/SHA256SUMS" ] || fetch "$base/SHA256SUMS" "$TMP/SHA256SUMS"
  fetch "$base/$1" "$TMP/$1"
  want="$(awk -v f="$1" '{ n = $2; sub(/^\*/, "", n); if (n == f) print $1 }' "$TMP/SHA256SUMS")"
  [ -n "$want" ] || die "$1 is not in SHA256SUMS"
  if have sha256sum; then got="$(sha256sum "$TMP/$1" | cut -d' ' -f1)"
  else got="$(shasum -a 256 "$TMP/$1" | cut -d' ' -f1)"; fi
  [ "$got" = "$want" ] || die "$1: checksum mismatch, nothing was installed"
}

install_macos() {
  if have brew; then
    say "installing with Homebrew"
    brew install --cask "$CASK"
    return
  fi
  TAG="$(release_tag)"; [ -n "$TAG" ] || die "no release found"
  v="${TAG#v}"
  dmg="Whoosh-Download-Manager_${v}_universal.dmg"
  say "downloading $dmg"
  asset "$dmg"
  mnt="$TMP/mnt"; mkdir -p "$mnt"
  hdiutil attach -nobrowse -quiet -mountpoint "$mnt" "$TMP/$dmg"
  dest="/Applications"; [ -w "$dest" ] || dest="$HOME/Applications"; mkdir -p "$dest"
  rm -rf "$dest/Whoosh Download Manager.app"
  cp -R "$mnt/Whoosh Download Manager.app" "$dest/"
  hdiutil detach -quiet "$mnt"
  say "installed in $dest; open it from Launchpad or Spotlight"
}

arch_deb() { case "$(uname -m)" in x86_64|amd64) echo amd64 ;; aarch64|arm64) echo arm64 ;; *) die "no Linux build for $(uname -m)" ;; esac; }
arch_rpm() { case "$(uname -m)" in x86_64|amd64) echo x86_64 ;; aarch64|arm64) echo aarch64 ;; *) die "no Linux build for $(uname -m)" ;; esac; }

install_apt() {
  if [ -n "$APT_REPO" ] && [ -n "$REPO_KEY" ]; then
    say "adding the WDM apt repository"
    fetch "$REPO_KEY" "$TMP/wdm.asc"
    root install -d -m 0755 /etc/apt/keyrings
    root sh -c "gpg --dearmor < '$TMP/wdm.asc' > /etc/apt/keyrings/whoosh-download-manager.gpg" 2>/dev/null \
      || root cp "$TMP/wdm.asc" /etc/apt/keyrings/whoosh-download-manager.asc
    key=/etc/apt/keyrings/whoosh-download-manager.gpg
    [ -f "$key" ] || key=/etc/apt/keyrings/whoosh-download-manager.asc
    echo "deb [signed-by=$key] $APT_REPO any-version main" | root tee /etc/apt/sources.list.d/whoosh-download-manager.list >/dev/null
    root apt-get update -o Dir::Etc::sourcelist=sources.list.d/whoosh-download-manager.list -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0
    root apt-get install -y whoosh-download-manager
    return
  fi
  TAG="$(release_tag)"; [ -n "$TAG" ] || die "no release found"
  deb="Whoosh-Download-Manager_${TAG#v}_$(arch_deb).deb"
  say "downloading $deb"
  asset "$deb"
  chmod 0644 "$TMP/$deb"
  root apt-get install -y "$TMP/$deb"
}

install_rpm() {
  pm="$1"
  if [ -n "$RPM_REPO" ]; then
    say "adding the WDM rpm repository"
    dir=/etc/yum.repos.d; [ "$pm" = zypper ] && dir=/etc/zypp/repos.d
    printf '[whoosh-download-manager]\nname=Whoosh Download Manager\nbaseurl=%s\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\ngpgkey=%s\n' \
      "$RPM_REPO" "$REPO_KEY" | root tee "$dir/whoosh-download-manager.repo" >/dev/null
    if [ "$pm" = zypper ]; then root zypper --non-interactive --gpg-auto-import-keys install whoosh-download-manager
    else root "$pm" install -y whoosh-download-manager; fi
    return
  fi
  TAG="$(release_tag)"; [ -n "$TAG" ] || die "no release found"
  v="${TAG#v}"
  rpm="Whoosh-Download-Manager-${v}-1.$(arch_rpm).rpm"
  say "downloading $rpm"
  asset "$rpm"
  if [ "$pm" = zypper ]; then root zypper --non-interactive install --allow-unsigned-rpm "$TMP/$rpm"
  else root "$pm" install -y "$TMP/$rpm"; fi
}

install_appimage() {
  TAG="$(release_tag)"; [ -n "$TAG" ] || die "no release found"
  case "$(uname -m)" in x86_64|amd64) a=amd64 ;; aarch64|arm64) a=aarch64 ;; *) die "no Linux build for $(uname -m)" ;; esac
  img="Whoosh-Download-Manager_${TAG#v}_${a}.AppImage"
  say "downloading $img"
  asset "$img"
  bin="$HOME/.local/bin"; mkdir -p "$bin" "$HOME/.local/share/applications"
  install -m 0755 "$TMP/$img" "$bin/Whoosh-Download-Manager.AppImage"
  cat > "$HOME/.local/share/applications/com.coderzonebd.whoosh.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Whoosh Download Manager
Exec=$bin/Whoosh-Download-Manager.AppImage
Icon=com.coderzonebd.whoosh
Categories=Network;FileTransfer;
DESKTOP
  say "installed $bin/Whoosh-Download-Manager.AppImage"
}

case "$(uname -s)" in
  Darwin) install_macos ;;
  Linux)
    if have apt-get && have dpkg; then install_apt
    elif have dnf; then install_rpm dnf
    elif have zypper; then install_rpm zypper
    elif have yum; then install_rpm yum
    else install_appimage; fi
    ;;
  *) die "this script is for Linux and macOS; on Windows run: winget install --id CoderZoneBD.Whoosh -e" ;;
esac
say "done. Whoosh Download Manager keeps itself up to date."
