#!/bin/sh
# Install native xcb from an exact release or the locked local source tree.
set -eu

: "${XCB_INSTALL_PREFIX:=$HOME/.local}"
: "${CARGO:=cargo}"
: "${XCB_VERSION:=}"
: "${XCB_GITHUB:=hraness/xcb}"

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source_root=
install_method=release

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
  install_method=source
  root=$(cd "$(dirname "$script_path")/.." && pwd -P)
  source_root=$root
  expected_version=$(awk '
    /^\[workspace\.package\]$/ { section=1; next }
    section && /^\[/ { exit }
    section && /^version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*$/ {
      sub(/^[^"]*"/, ""); sub(/"[[:space:]]*$/, ""); print; exit
    }
  ' "$root/Cargo.toml")
  version_valid "$expected_version" || fail "invalid Cargo workspace version"
  cd "$root"
  # Cargo owns artifact selection, including configured target directories and
  # triples. Install into our empty private stage so a stale default target
  # binary can never be selected. `cargo install` builds release by default.
  "$CARGO" install --path "$root/crates/xcb-cli" --locked --bin xcb --root "$stage/cargo-install" --no-track
  source="$stage/cargo-install/bin/xcb"
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

# Keep the exact installer beside the user-global binary so `xcb upgrade` can
# delegate release updates to the same checksum and atomic-swap contract. This
# share directory is also the default private state root, so it must satisfy
# the owner-only directory contract.
install_prefix=$(cd "$(dirname "$bin_dir")" && pwd -P)
share_dir="$install_prefix/share/xcb"
[ ! -L "$install_prefix" ] && [ ! -L "$share_dir" ] || fail "install metadata parent must not be a symlink"
mkdir -p "$share_dir"
chmod 0700 "$share_dir"
if [ -e "$share_dir/install-native.sh" ] || [ -L "$share_dir/install-native.sh" ]; then
  regular_file "$share_dir/install-native.sh" || fail "existing installer metadata helper is unsafe"
fi
cp "$script_path" "$stage/install-native.sh"
chmod 0755 "$stage/install-native.sh"
mv -f "$stage/install-native.sh" "$share_dir/install-native.sh"
# Paths are host-generated absolute paths; reject control characters before
# writing the small machine-readable manifest. Escape the two JSON characters
# that can occur in a user-selected install prefix.
case "$install_prefix$share_dir$source_root" in *[![:print:]]*) fail "install path contains a control character" ;; esac
json_escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
manifest_prefix=$(json_escape "$install_prefix")
manifest_helper=$(json_escape "$share_dir/install-native.sh")
manifest_source=$(json_escape "$source_root")
manifest_binary=$(json_escape "$destination")
printf '{"version":1,"installMethod":"%s","channel":"stable","versionString":"%s","prefix":"%s","helperPath":"%s","sourceRoot":"%s","binaryPath":"%s"}\n' \
  "$install_method" "$expected_version" "$manifest_prefix" "$manifest_helper" "$manifest_source" "$manifest_binary" > "$stage/install.json"
chmod 0600 "$stage/install.json"
if [ -e "$share_dir/install.json" ] || [ -L "$share_dir/install.json" ]; then
  regular_file "$share_dir/install.json" || fail "existing install manifest is unsafe"
fi
mv -f "$stage/install.json" "$share_dir/install.json"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *)
    if [ "${XCB_ADD_PATH:-ask}" = yes ]; then
      startup="$HOME/.profile"
      case "${SHELL##*/}" in
        zsh) startup="$HOME/.zprofile" ;;
        bash) startup="$HOME/.bash_profile" ;;
      esac
      mkdir -p "$(dirname "$startup")"
      path_line="export PATH=\"$bin_dir:\$PATH\""
      if [ ! -f "$startup" ] || ! grep -Fqx "$path_line" "$startup"; then
        printf '\n%s\n' "$path_line" >> "$startup"
      fi
      echo "Added $bin_dir to PATH in $startup"
    else
      echo "$bin_dir is not on PATH. Add it with:"
      echo "  export PATH=\"$bin_dir:\$PATH\""
    fi
    ;;
esac

echo "Installed $destination ($candidate_digest)"
echo "$reported_version"
echo "Restart open xcb terminals, then run xcb doctor to refresh provider pins."
