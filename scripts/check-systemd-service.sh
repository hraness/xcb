#!/bin/sh
# Round-trip `xcb service` and `xcb update enable|disable` against this
# user's real systemd user manager:
#
#   scripts/check-systemd-service.sh path/to/xcb
#
# It installs the supervisor unit and checks systemd enabled and started it,
# proves an edited unit is left alone, uninstalls, then does the same for
# the daily update timer. It writes to ~/.config/systemd/user and
# ~/.local/state/xcb, so run it on a disposable host (CI does, after
# `loginctl enable-linger` gives the runner a user manager).
set -eu

fail() { echo "error: $*" >&2; exit 1; }
[ "$#" = 1 ] || { echo "usage: $0 XCB" >&2; exit 2; }
xcb=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")
[ -x "$xcb" ] || fail "$1 is not executable"
command -v systemctl >/dev/null 2>&1 || fail "systemctl not found"

: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
export XDG_RUNTIME_DIR
waited=0
until systemctl --user show-environment >/dev/null 2>&1; do
  waited=$((waited + 1))
  [ "$waited" -le 30 ] || fail "no systemd user manager answers at $XDG_RUNTIME_DIR"
  sleep 1
done

units="$HOME/.config/systemd/user"
state=$(mktemp -d "${TMPDIR:-/tmp}/xcb-systemd.XXXXXX")
chmod 0700 "$state"
trap 'rm -rf "$state"' EXIT
run() { "$xcb" --state "$state" "$@"; }
# field NAME [NAME...]: one value from the JSON on stdin, by key path.
field() {
  python3 -I -c 'import json, sys
value = json.load(sys.stdin)
for key in sys.argv[1:]:
    value = value[key]
print(value)' "$@"
}

echo "--- xcb service install"
run service install
unit=$(run --json service status | field service manifest)
case "$unit" in
  "$units"/xcb-habitat-*.service) ;;
  *) fail "unexpected unit path: $unit" ;;
esac
[ -f "$unit" ] || fail "$unit was not written"
name=$(basename "$unit")
systemctl --user is-enabled --quiet "$name" || fail "$name is not enabled"
grep -qx 'Restart=always' "$unit" || fail "$name does not restart the supervisor"
[ "$(run --json service status | field installed)" = True ] || fail "status does not report installed"
[ "$(run --json service status | field registered)" = True ] || fail "status does not report registered"
log=$(run --json service status | field log)
# systemd opens the append: log when it starts the unit, so the file is the
# evidence that the supervisor was launched.
waited=0
until [ -f "$log" ]; do
  waited=$((waited + 1))
  [ "$waited" -le 30 ] || fail "systemd never started $name (no $log)"
  sleep 1
done
run service install >/dev/null || fail "a second install is not idempotent"
run service status

echo "--- an edited unit is preserved"
cp "$unit" "$state/unit.original"
printf '# edited by hand\n' >> "$unit"
if run service status >/dev/null 2>&1; then fail "status accepted an edited unit"; fi
if run service uninstall >/dev/null 2>&1; then fail "uninstall accepted an edited unit"; fi
tail -n 1 "$unit" | grep -qx '# edited by hand' || fail "uninstall changed an edited unit"
cp "$state/unit.original" "$unit"

echo "--- xcb service uninstall"
# Uninstall refuses while the supervisor runs; an idle one exits within 30s.
waited=0
until run service uninstall >/dev/null 2>"$state/uninstall.err"; do
  grep -q 'active' "$state/uninstall.err" || { cat "$state/uninstall.err" >&2; fail "uninstall failed"; }
  waited=$((waited + 1))
  [ "$waited" -le 180 ] || fail "the supervisor never went idle"
  sleep 1
done
[ ! -e "$unit" ] || fail "$unit is still there"
if systemctl --user is-enabled --quiet "$name" 2>/dev/null; then fail "$name is still enabled"; fi
[ "$(run --json service status | field installed)" = False ] || fail "status still reports installed"

echo "--- xcb update enable"
run update enable
for file in xcb-update.timer xcb-update.service; do
  [ -f "$units/$file" ] || fail "$file was not written"
done
systemctl --user is-enabled --quiet xcb-update.timer || fail "xcb-update.timer is not enabled"
systemctl --user is-active --quiet xcb-update.timer || fail "xcb-update.timer is not active"
run update enable --policy auto >/dev/null || fail "a second enable is not idempotent"

echo "--- xcb update disable"
run update disable
for file in xcb-update.timer xcb-update.service; do
  [ ! -e "$units/$file" ] || fail "$file is still there"
done
if systemctl --user is-enabled --quiet xcb-update.timer 2>/dev/null; then fail "xcb-update.timer is still enabled"; fi

echo "--- a foreign xcb-update.timer is preserved"
printf '[Timer]\nOnCalendar=hourly\n' > "$units/xcb-update.timer"
if run update enable >/dev/null 2>&1; then fail "enable replaced a foreign timer"; fi
if run update disable >/dev/null 2>&1; then fail "disable removed a foreign timer"; fi
[ "$(cat "$units/xcb-update.timer")" = "$(printf '[Timer]\nOnCalendar=hourly')" ] || fail "the foreign timer changed"
[ ! -e "$units/xcb-update.service" ] || fail "enable wrote a service beside a foreign timer"
rm -f "$units/xcb-update.timer"

echo "ok: xcb service and xcb update round-trip through systemd --user"
