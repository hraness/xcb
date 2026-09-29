#!/bin/sh
# Build the native xcb (Excalibur) CLI from source and emit a packaged
# tarball plus a SHA-256 checksum. The tarball name includes the version,
# OS, and architecture so the install script can fetch exact release assets.
set -eu

: "${CARGO:=cargo}"
# Release assets always use the `release` profile. CI's per-change native
# build sets XCB_CARGO_PROFILE=ci (no LTO) to prove the same binary builds and
# runs without paying the release link on every pull request.
: "${XCB_CARGO_PROFILE:=release}"
case "$XCB_CARGO_PROFILE" in
  release|ci) ;;
  *) echo "error: XCB_CARGO_PROFILE must be release or ci" >&2; exit 1 ;;
esac
root="$(cd "$(dirname "$0")/.." && pwd)"

if [ -n "${XCB_VERSION:-}" ]; then
  version="${XCB_VERSION#v}"
else
  version=$(grep -m1 '^version = ' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
  if [ -z "$version" ]; then
    echo "error: could not read version from workspace Cargo.toml" >&2
    exit 1
  fi
fi

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
  x86_64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
esac

cd "$root"
"$CARGO" build --profile "$XCB_CARGO_PROFILE" --locked -p xcb-cli

binary="$root/target/$XCB_CARGO_PROFILE/xcb"
if [ ! -f "$binary" ]; then
  echo "error: expected $binary after build" >&2
  exit 1
fi
reported_version=$("$binary" --version)
if [ "$reported_version" != "xcb $version" ]; then
  echo "error: native binary reports '$reported_version', expected 'xcb $version'" >&2
  exit 1
fi

sha256_cmd=$(command -v sha256sum || command -v shasum || true)
if [ -z "$sha256_cmd" ]; then
  echo "error: neither sha256sum nor shasum found" >&2
  exit 1
fi
if [ "$sha256_cmd" != "${sha256_cmd%shasum}" ]; then
  sha256_cmd="$sha256_cmd -a 256"
fi

mkdir -p "$root/artifacts"
name="xcb-${version}-${os}-${arch}"
archive="$root/artifacts/${name}.tar.gz"
checksum_file="${archive}.sha256"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
install -m 0755 "$binary" "$work/xcb"
# The archive must hold exactly one regular member named `xcb`: no AppleDouble
# `._xcb` companions, extended attributes, or directory entries. The installer
# rejects anything else, so prove it here before the bytes leave the builder.
COPYFILE_DISABLE=1 tar -czf "$archive" -C "$work" xcb
$sha256_cmd "$archive" | sed 's/ .*//' > "$checksum_file"
# Re-admit the packaged bytes exactly as the installer will: one regular
# member, matching checksum, and an extracted binary reporting this version.
"$root/scripts/check-native-archive.sh" "$version" "$archive"
echo "tarball=$archive"
echo "sha256=$checksum_file"
