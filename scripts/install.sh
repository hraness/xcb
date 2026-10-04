#!/bin/sh
# Install Excalibur (xcb), which routes coding tasks across the Claude and Codex
# subscriptions you already pay for.
#
#   curl -fsSL https://xcb.sh/install.sh | sh
#   curl -fsSL https://xcb.sh/install.sh | XCB_VERSION=<version> sh   # one exact version
#
# This script downloads the installer from the release's tag. That installer
# fetches the archive for your platform from the GitHub Release, checks it
# against its .sha256 file, and installs ~/.local/bin/xcb. Nothing runs as root.
# Options (environment): XCB_VERSION, XCB_INSTALL_PREFIX (default ~/.local),
# XCB_ADD_PATH=yes (add the bin directory to your shell profile).
# Source: https://github.com/hraness/xcb/blob/main/scripts/install.sh
#
# Everything is inside main(), so a partial download runs nothing.

main() {
  set -eu
  # xcb.sh renders this from site/published-release.json; never type it.
  default_version="@XCB_RELEASE_VERSION@"
  repository="hraness/xcb"
  guide="https://xcb.sh/install"

  pinned=false
  [ -n "${XCB_VERSION:-}" ] && pinned=true
  version="${XCB_VERSION:-$default_version}"
  version="${version#v}"
  printf '%s\n' "$version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
    || fail "XCB_VERSION must be an exact release version, MAJOR.MINOR.PATCH (got '$version')"
  [ -n "${HOME:-}" ] || [ -n "${XCB_INSTALL_PREFIX:-}" ] \
    || fail "HOME is not set; set XCB_INSTALL_PREFIX to choose where xcb goes"

  os=$(uname -s)
  arch=$(uname -m)
  # The hosts with release archives. scripts/install-native.sh accepts the
  # same set.
  case "$os/$arch" in
    Darwin/arm64 | Darwin/aarch64) platform=darwin-aarch64 ;;
    Linux/x86_64 | Linux/amd64) platform=linux-x86_64 ;;
    Linux/aarch64 | Linux/arm64) platform=linux-aarch64 ;;
    Darwin/x86_64) fail "there is no release build for Intel Macs yet; build from source: $guide#source" ;;
    *) fail "there is no release build for $os/$arch yet; build from source: $guide#source" ;;
  esac

  command -v curl >/dev/null 2>&1 || fail "curl is required"
  command -v tar >/dev/null 2>&1 || fail "tar is required"

  # Older releases have no archive for every platform. Only a definite 404
  # stops here; any other answer is left to the installer's own download.
  asset="https://github.com/$repository/releases/download/v$version/xcb-$version-$platform.tar.gz"
  asset_status=$(curl -sIL --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 60 \
    -o /dev/null -w '%{http_code}' "$asset" 2>/dev/null) || asset_status=
  [ "$asset_status" != 404 ] \
    || fail "xcb $version has no release build for $os $arch; build from source: $guide#source"

  temporary=$(mktemp -d "${TMPDIR:-/tmp}/xcb-install.XXXXXX")
  trap 'rm -rf "$temporary"' EXIT
  trap 'exit 1' HUP INT TERM

  # A v* tag names one commit forever (repository ruleset), so this is the
  # exact installer that shipped with the version being installed.
  installer="https://raw.githubusercontent.com/$repository/v$version/scripts/install-native.sh"
  echo "Installing xcb $version for $os $arch"
  curl -fsSL --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 \
    -o "$temporary/install-native.sh" "$installer" \
    || fail "could not download the installer for v$version; check the version at https://github.com/$repository/releases"
  head -n 2 "$temporary/install-native.sh" | grep -q '^# Install native xcb' \
    || fail "the downloaded installer is not the xcb installer"

  prefix="${XCB_INSTALL_PREFIX:-$HOME/.local}"
  had_xcb=0
  [ -e "$prefix/bin/xcb" ] && had_xcb=1

  if ! XCB_VERSION="$version" XCB_INSTALL_PINNED="$pinned" sh "$temporary/install-native.sh"; then
    if [ "$os" = Linux ]; then
      echo "If the error mentions GLIBC, this release needs a newer glibc than this system has (check with: getconf GNU_LIBC_VERSION); build from source instead: $guide#source" >&2
    fi
    exit 1
  fi

  if [ "$had_xcb" = 0 ]; then
    echo
    echo "Next: xcb setup claude    (checks Claude Code and signs you in)"
    echo "Guide: $guide"
  fi
}

fail() {
  printf 'xcb install: %s\n' "$*" >&2
  exit 1
}

main "$@"
