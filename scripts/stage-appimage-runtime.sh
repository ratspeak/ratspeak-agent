#!/usr/bin/env bash
set -euo pipefail

stage="$(dirname "$0")/../app/src-tauri/target/appimage-runtime"
mkdir -p "$stage"
libraries=$(ldconfig -p)

# The tray dlopens its indicator library, which linuxdeploy cannot see in the executable.
for soname in libayatana-appindicator3.so.1 libayatana-ido3-0.4.so.0 \
  libayatana-indicator3.so.7 libdbusmenu-glib.so.4 libdbusmenu-gtk3.so.4; do
  source_path=$(awk -v name="$soname" '$1 == name { print $NF; exit }' <<< "$libraries")
  if [[ ! -f "$source_path" ]]; then
    echo "Could not find AppImage runtime library: $soname" >&2
    exit 1
  fi
  install -m 0644 "$source_path" "$stage/$soname"
done
