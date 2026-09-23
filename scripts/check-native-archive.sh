#!/bin/sh
# Admit packaged native release archives exactly the way scripts/install-native.sh
# does before it installs one: the adjacent `.sha256` describes the archive
# bytes, the archive lists exactly one regular member named `xcb`, and the
# extracted binary reports `xcb <version>`. Run it on the platform that built
# the archive; the binary is executed.
set -eu

usage() { echo "usage: $0 VERSION ARCHIVE.tar.gz [ARCHIVE.tar.gz ...]" >&2; exit 2; }
fail() { echo "error: $*" >&2; exit 1; }
[ "$#" -ge 2 ] || usage
version=${1#v}
shift
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || fail "version must be a stable semantic version"

sha256_cmd=$(command -v sha256sum || true)
sha256_kind=sha256sum
if [ -z "$sha256_cmd" ]; then
  sha256_cmd=$(command -v shasum || true)
  sha256_kind=shasum
fi
[ -n "$sha256_cmd" ] || fail "neither sha256sum nor shasum found"
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

# macOS bsdtar folds AppleDouble `._xcb` companions into `xcb` as extended
# attributes and hides them from listings; `!mac-ext` lists them like GNU tar
# does. GNU tar has no --options, so fall back to its plain listing there.
tar_list() {
  if listed=$(LC_ALL=C tar --options '!mac-ext' "$1" "$2" 2>/dev/null); then
    printf '%s\n' "$listed"
  else
    LC_ALL=C tar "$1" "$2"
  fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for archive in "$@"; do
  [ -f "$archive" ] && [ ! -L "$archive" ] || fail "$archive is not a regular file"
  base=$(basename "$archive")
  case "$base" in
    "xcb-$version-"*-*.tar.gz) ;;
    *) fail "$base is not an xcb-$version-<os>-<arch>.tar.gz release archive" ;;
  esac
  checksum_file="$archive.sha256"
  [ -f "$checksum_file" ] && [ ! -L "$checksum_file" ] || fail "$checksum_file is missing"
  recorded=$(tr -d '[:space:]' < "$checksum_file")
  [ "${#recorded}" -eq 64 ] || fail "$checksum_file is not one SHA-256 digest"
  case "$recorded" in *[!0-9a-f]*) fail "$checksum_file is not one SHA-256 digest" ;; esac
  actual=$(sha256 "$archive") || fail "could not hash $archive"
  [ "$recorded" = "$actual" ] || fail "checksum mismatch for $base"

  members=$(tar_list -tzf "$archive") || fail "$base is not a readable gzip tar archive"
  [ "$members" = xcb ] || fail "$base must list exactly one member 'xcb' (got: $(printf '%s' "$members" | tr '\n' ' '))"
  types=$(tar_list -tvzf "$archive")
  [ "$(printf '%s\n' "$types" | wc -l | tr -d '[:space:]')" = 1 ] || fail "$base must contain exactly one entry"
  case "$types" in -*) ;; *) fail "$base member xcb must be a regular file" ;; esac

  rm -f "$work/xcb"
  tar -xzOf "$archive" xcb > "$work/xcb"
  chmod 0755 "$work/xcb"
  reported=$("$work/xcb" --version) || fail "extracted xcb --version failed for $base"
  [ "$reported" = "xcb $version" ] || fail "extracted binary reports '$reported', expected 'xcb $version'"
  echo "ok: $base sha256=$actual reports '$reported'"
done
