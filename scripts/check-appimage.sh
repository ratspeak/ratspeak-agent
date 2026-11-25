#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || ! -x "$1" ]]; then
  echo "Expected one executable AppImage" >&2
  exit 1
fi
image=$(realpath "$1")
helper=$(realpath "$(dirname "$0")/../app/src-tauri/linux/appimage-card-helper.sh")
cd "$(mktemp -d)"
echo "Inspecting the AppImage filesystem"
offset=$("$image" --appimage-offset)
unsquashfs -no-progress -no-xattrs -offset "$offset" "$image" >/dev/null
test -x squashfs-root/AppRun
test -x squashfs-root/usr/bin/ratspeak-agent
indicator="$PWD/squashfs-root/usr/lib/libayatana-appindicator3.so.1"
dependencies=$(LD_LIBRARY_PATH="${indicator%/*}" ldd "$indicator")
if grep -q 'not found' <<< "$dependencies"; then
  printf '%s\n' "$dependencies" >&2
  exit 1
fi

# Missing writer arguments must exit before opening a card or starting the GUI.
echo "Starting the elevated card helper"
helper_status=0
sudo -n -- /bin/sh "$helper" "$image" --appimage-extract-and-run --write-image || helper_status=$?
test "$helper_status" -eq 2

echo "Opening the AppImage window"
export TMPDIR="$PWD/tmp" XDG_CONFIG_HOME="$PWD/config" XDG_DATA_HOME="$PWD/data" XDG_CACHE_HOME="$PWD/cache"
mkdir -m 700 "$TMPDIR"
timeout -k 5s 60s xvfb-run -a dbus-run-session -- bash -eu -s -- "$image" <<'BASH'
  "$1" --appimage-extract-and-run >app.log 2>&1 &
  app_pid=$!
  trap 'kill "$app_pid" 2>/dev/null || true' EXIT
  for _ in {1..40}; do
    if xdotool search --onlyvisible --name "^Ratspeak Agent$" >/dev/null; then
      if grep -F "System tray unavailable:" app.log >&2; then exit 1; fi
      echo "AppImage opened the Ratspeak Agent window"
      exit 0
    fi
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 1
  done
  cat app.log >&2
  exit 1
BASH
