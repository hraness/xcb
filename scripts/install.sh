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
#
# It also installs aicharts beside xcb for local usage history across your
# agents (https://aicharts.io/usage), checked against the digest pinned below,
# and on a first install turns that history on. It stays on this computer;
# nothing is uploaded. XCB_AICHARTS=no skips aicharts; XCB_USAGE_HISTORY=no
# installs it but leaves history off.
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

  first_install=no
  [ "$had_xcb" = 1 ] || first_install=yes
  install_aicharts "$prefix/bin" "$platform" "$first_install"

  if [ "$had_xcb" = 0 ]; then
    echo
    echo "Next: xcb setup claude    (checks Claude Code and signs you in)"
    echo "Guide: $guide"
  fi
}

# The aicharts release this installer adds, with its reviewed archive digests.
AICHARTS_VERSION=0.3.1
AICHARTS_SHA256_DARWIN_AARCH64=e79a19b0b174845c939e2472b4dbf3e5738b6bf867bd16aba2daa86be6f049b6
AICHARTS_SHA256_LINUX_X86_64=c2a8acf56019565668bbcf84884503428d857ab5c54fecec85ae145644f83559

# install_aicharts BIN PLATFORM FIRST_INSTALL installs or upgrades the pinned
# aicharts in BIN, leaves an aicharts installed elsewhere or a newer one alone,
# and on a first install turns on local usage history. A failure here only
# warns: xcb is already installed.
install_aicharts() {
  case "${XCB_AICHARTS:-yes}" in no | 0 | false | off) return 0 ;; esac
  case "$2" in
    darwin-aarch64) aicharts_target=aarch64-apple-darwin aicharts_sha256=$AICHARTS_SHA256_DARWIN_AARCH64 ;;
    linux-x86_64) aicharts_target=x86_64-unknown-linux-gnu aicharts_sha256=$AICHARTS_SHA256_LINUX_X86_64 ;;
    *) echo "aicharts has no release for this platform yet, so usage history is not installed"; return 0 ;;
  esac
  aicharts_base="https://github.com/hraness/aicharts/releases/download/cli-v$AICHARTS_VERSION"
  if [ -n "${XCB_AICHARTS_BASE_URL:-}" ]; then
    # Tests serve their own build; only a loopback server may replace the digest.
    printf '%s\n' "$XCB_AICHARTS_BASE_URL" | LC_ALL=C grep -Eq '^http://127\.0\.0\.1:[0-9]{1,5}$' \
      || { warn "XCB_AICHARTS_BASE_URL may only name a loopback test server"; return 0; }
    aicharts_base=$XCB_AICHARTS_BASE_URL
    aicharts_sha256=${XCB_AICHARTS_SHA256:-$aicharts_sha256}
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    aicharts_digest() { sha256sum "$1" | cut -d ' ' -f 1; }
  elif command -v shasum >/dev/null 2>&1; then
    aicharts_digest() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
  else
    warn "sha256sum or shasum is required for aicharts; usage history is not installed"
    return 0
  fi
  aicharts="$1/aicharts"
  elsewhere=$(command -v aicharts 2>/dev/null || true)
  if [ -n "$elsewhere" ] && [ "$elsewhere" != "$aicharts" ]; then
    echo "Using $elsewhere for local usage history"
    aicharts=$elsewhere
  else
    current=$("$aicharts" --version 2>/dev/null | sed -n 's/^aicharts \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' || true)
    if [ -z "$current" ] || version_older "$current" "$AICHARTS_VERSION"; then
      fetch_aicharts "$1" "$2" || return 0
      # xcb usage connect pins the aicharts build it registered; renew it.
      if [ -x "$1/xcb" ] && "$1/xcb" --json tools list 2>/dev/null | grep -q '"name":"aicharts"'; then
        "$1/xcb" usage connect >/dev/null 2>&1 \
          || warn "run xcb usage connect so tasks use the updated aicharts tools"
      fi
    fi
  fi
  [ "$3" = yes ] || return 0
  case "${XCB_USAGE_HISTORY:-yes}" in no | 0 | false | off) return 0 ;; esac
  # Leave history alone unless it has never been turned on.
  status=$(HRANESS_SUPPORT_AUDIENCE=off "$aicharts" history status --json 2>/dev/null) || return 0
  case "$status" in *'"collecting":"off"'*) ;; *) return 0 ;; esac
  if HRANESS_SUPPORT_AUDIENCE=off "$aicharts" history enable >/dev/null 2>&1; then
    echo
    echo "Local usage history is on: aicharts records your agents' daily token totals"
    echo "on this computer and never uploads them."
    echo "  See them:  aicharts history report"
    echo "  Turn off:  aicharts history disable"
  else
    warn "could not turn on local usage history; run: aicharts history enable"
  fi
}

# fetch_aicharts BIN PLATFORM downloads, checks and installs the pinned build.
fetch_aicharts() {
  aicharts_root="aicharts-$AICHARTS_VERSION-$aicharts_target"
  aicharts_asset="$aicharts_root.tar.gz"
  case "$aicharts_base" in
    https://*) aicharts_curl="--proto =https --tlsv1.2" ;;
    *) aicharts_curl= ;;
  esac
  # shellcheck disable=SC2086 # the protocol options are deliberately split
  curl -fsSL $aicharts_curl --connect-timeout 15 --max-time 300 -o "$temporary/$aicharts_asset" "$aicharts_base/$aicharts_asset" \
    || { warn "could not download $aicharts_asset; usage history is not installed"; return 1; }
  aicharts_actual=$(aicharts_digest "$temporary/$aicharts_asset")
  [ "$aicharts_actual" = "$aicharts_sha256" ] \
    || { warn "checksum mismatch for $aicharts_asset (expected $aicharts_sha256, got $aicharts_actual); usage history is not installed"; return 1; }
  mkdir "$temporary/aicharts"
  tar -xzf "$temporary/$aicharts_asset" -C "$temporary/aicharts" "$aicharts_root/bin/aicharts" 2>/dev/null \
    || { warn "$aicharts_asset has no bin/aicharts; usage history is not installed"; return 1; }
  aicharts_candidate="$temporary/aicharts/$aicharts_root/bin/aicharts"
  [ -f "$aicharts_candidate" ] && [ ! -L "$aicharts_candidate" ] \
    || { warn "$aicharts_asset must contain a regular bin/aicharts"; return 1; }
  if [ "$2" = darwin-aarch64 ]; then
    requirement='anchor apple generic and identifier "dev.hraness.aicharts" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "8AAP53VTW3"'
    if [ ! -x /usr/bin/codesign ] || ! /usr/bin/codesign --verify --strict --all-architectures --test-requirement "=$requirement" "$aicharts_candidate"; then
      warn "aicharts does not have the required Apple Developer ID signature; usage history is not installed"
      return 1
    fi
  fi
  chmod 0755 "$aicharts_candidate"
  case "$("$aicharts_candidate" --version 2>/dev/null)" in
    "aicharts $AICHARTS_VERSION" | "aicharts $AICHARTS_VERSION "*) ;;
    *) warn "the downloaded aicharts does not report version $AICHARTS_VERSION"; return 1 ;;
  esac
  mkdir -p "$1"
  [ ! -L "$1/aicharts" ] || { warn "$1/aicharts is a symlink; leaving it alone"; return 1; }
  cp "$aicharts_candidate" "$1/.aicharts-install.$$"
  chmod 0755 "$1/.aicharts-install.$$"
  mv -f "$1/.aicharts-install.$$" "$1/aicharts"
  echo "Installed $1/aicharts $AICHARTS_VERSION for local usage history ($aicharts_actual)"
}

# version_older A B succeeds when MAJOR.MINOR.PATCH A sorts before B.
version_older() {
  printf '%s %s\n' "$1" "$2" | awk '{ split($1, a, "."); split($2, b, "."); for (i = 1; i <= 3; i++) { if (a[i] + 0 < b[i] + 0) exit 0; if (a[i] + 0 > b[i] + 0) exit 1 } exit 1 }'
}

warn() {
  printf 'xcb install: %s\n' "$*" >&2
}

fail() {
  printf 'xcb install: %s\n' "$*" >&2
  exit 1
}

main "$@"
