#!/bin/sh
# Install native xcb from an exact release or the locked local source tree.
set -eu

: "${XCB_INSTALL_PREFIX:=$HOME/.local}"
: "${CARGO:=cargo}"
: "${XCB_VERSION:=}"
: "${XCB_GITHUB:=hraness/xcb}"

fail() { echo "error: $*" >&2; exit 1; }
version_valid() {
  printf '%s\n' "$1" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
}
sha256() {
  if [ "$sha256_kind" = sha256sum ]; then
    hash_output=$("$sha256_cmd" "$1") || return 1
  else
    hash_output=$("$sha256_cmd" -a 256 "$1") || return 1
  fi
  hash_value=${hash_output%% *}
  [ "${#hash_value}" -eq 64 ] || return 1
  case "$hash_value" in *[!0-9a-f]*) return 1 ;; esac
  printf '%s\n' "$hash_value"
}
regular_file() { [ -f "$1" ] && [ ! -L "$1" ]; }

bin_dir="$XCB_INSTALL_PREFIX/bin"
mkdir -p "$bin_dir"
bin_dir=$(cd "$bin_dir" && pwd -P)
lock="$bin_dir/.xcb-install-lock"
mkdir "$lock" 2>/dev/null || fail "another installation owns $lock; retain it until that installer has stopped"
stage=
cleanup() {
  if [ -n "$stage" ]; then rm -rf "$stage"; fi
  rmdir "$lock"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
stage=$(mktemp -d "$bin_dir/.xcb-install.XXXXXX")
chmod 0700 "$stage"

sha256_cmd=$(command -v sha256sum || true)
sha256_kind=sha256sum
if [ -z "$sha256_cmd" ]; then
  sha256_cmd=$(command -v shasum || true)
  sha256_kind=shasum
fi
[ -n "$sha256_cmd" ] || fail "neither sha256sum nor shasum found"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
  x86_64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) fail "unsupported architecture: $arch" ;;
esac

install_from_release() {
  expected_version=$XCB_VERSION
  version_valid "$expected_version" || fail "release version must be a stable semantic version"
  asset="xcb-${expected_version}-${os}-${arch}.tar.gz"
  base_url="https://github.com/$XCB_GITHUB/releases/download/v$expected_version"
  curl -fsSL -o "$stage/archive.tar.gz" "$base_url/$asset"
  curl -fsSL -o "$stage/checksum" "$base_url/$asset.sha256"
  expected=$(tr -d '[:space:]' < "$stage/checksum")
  [ "${#expected}" -eq 64 ] || fail "invalid release checksum"
  case "$expected" in *[!0-9a-f]*) fail "invalid release checksum" ;; esac
  [ "$expected" = "$(sha256 "$stage/archive.tar.gz")" ] || fail "checksum mismatch for $asset"

  # Admit one logical regular entry before extraction. Stream its contents to
  # our own path: archive permissions, links and paths never create objects.
  LC_ALL=C tar -tzf "$stage/archive.tar.gz" > "$stage/members"
  LC_ALL=C tar -tvzf "$stage/archive.tar.gz" > "$stage/types"
  [ "$(wc -l < "$stage/members" | tr -d '[:space:]')" = 1 ] || fail "archive must contain only the xcb binary"
  [ "$(cat "$stage/members")" = xcb ] || fail "unsafe archive path"
  [ "$(wc -l < "$stage/types" | tr -d '[:space:]')" = 1 ] || fail "unsafe archive entry type"
  case "$(cat "$stage/types")" in -*) ;; *) fail "archive xcb must be a regular file" ;; esac
  tar -xzOf "$stage/archive.tar.gz" xcb > "$stage/candidate"
}

install_from_source() {
  root=$(cd "$(dirname "$0")/.." && pwd -P)
  expected_version=$(awk '
    /^\[workspace\.package\]$/ { section=1; next }
    section && /^\[/ { exit }
    section && /^version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*$/ {
      sub(/^[^"]*"/, ""); sub(/"[[:space:]]*$/, ""); print; exit
    }
  ' "$root/Cargo.toml")
  version_valid "$expected_version" || fail "invalid Cargo workspace version"
  cd "$root"
  "$CARGO" build --release --locked -p xcb-cli
  source="$root/target/release/xcb"
  regular_file "$source" || fail "source candidate must be a regular non-symlink file"
  source_digest=$(sha256 "$source")
  cp "$source" "$stage/candidate"
  [ "$source_digest" = "$(sha256 "$source")" ] && [ "$source_digest" = "$(sha256 "$stage/candidate")" ] || fail "source candidate changed during staging"
}

if [ -n "$XCB_VERSION" ]; then
  XCB_VERSION=${XCB_VERSION#v}
  install_from_release
else
  install_from_source
fi
regular_file "$stage/candidate" || fail "candidate must be a regular non-symlink file"
chmod 0755 "$stage/candidate"
candidate_digest=$(sha256 "$stage/candidate")
reported_version=$("$stage/candidate" --version) || fail "candidate --version failed"
[ "$reported_version" = "xcb $expected_version" ] || fail "candidate reports '$reported_version', expected 'xcb $expected_version'"
[ "$candidate_digest" = "$(sha256 "$stage/candidate")" ] || fail "candidate changed during verification"

previous_digest=
destination="$bin_dir/xcb"
if [ -e "$destination" ] || [ -L "$destination" ]; then
  regular_file "$destination" || fail "existing xcb must be a regular non-symlink file"
  previous_digest=$(sha256 "$destination")
  cp "$destination" "$stage/previous"
  [ "$previous_digest" = "$(sha256 "$stage/previous")" ] || fail "existing xcb changed during backup"
  chmod 0500 "$stage/previous"
  backup="$bin_dir/xcb.previous.$previous_digest"
  if [ -e "$backup" ] || [ -L "$backup" ]; then
    regular_file "$backup" && [ "$previous_digest" = "$(sha256 "$backup")" ] || fail "existing backup is unsafe or has changed: $backup"
  else
    # The private stage is on the destination filesystem. A link publishes the
    # complete backup without clobbering any existing backup of these bytes.
    ln "$stage/previous" "$backup"
  fi
  regular_file "$destination" && [ "$previous_digest" = "$(sha256 "$destination")" ] || fail "existing xcb changed before replacement"
else
  [ ! -e "$destination" ] && [ ! -L "$destination" ] || fail "installation destination changed"
fi

# A same-filesystem rename never truncates the executable held by a running
# process. POSIX shell has no portable file/directory fsync primitive, so this
# guarantees atomic visibility, not persistence through sudden power loss.
mv -f "$stage/candidate" "$destination"
[ "$candidate_digest" = "$(sha256 "$destination")" ] || fail "installed binary changed"
if [ -n "$previous_digest" ]; then echo "Previous binary preserved at $backup"; fi

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *)
    if [ "${XCB_ADD_PATH:-ask}" = yes ]; then
      printf '\nexport PATH="%s:$PATH"\n' "$bin_dir" >> "$HOME/.profile"
      echo "Added $bin_dir to PATH in $HOME/.profile"
    else
      echo "$bin_dir is not on PATH. Add it with:"
      echo "  export PATH=\"$bin_dir:\$PATH\""
    fi
    ;;
esac

echo "Installed $destination ($candidate_digest)"
echo "$reported_version"
echo "Restart open xcb terminals, then run xcb doctor to refresh provider pins."
