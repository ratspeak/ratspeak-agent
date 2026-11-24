#!/bin/sh
# Debian reads 0.1.0-beta as release 0.1.0 with package revision "beta", which sorts after 0.1.0, so the final
# release would never replace the beta. Rewrite a prerelease as 0.1.0~beta, which sorts first. (RPM already does.)
# Usage: scripts/deb-version.sh path/to/package.deb ...
set -eu
for deb in "$@"; do
  version=$(dpkg-deb -f "$deb" Version)
  case "$version" in *-*) ;; *) continue ;; esac
  fixed=$(printf '%s' "$version" | sed 's/-/~/')
  work=$(mktemp -d)
  dpkg-deb -R "$deb" "$work/package"
  sed -i "s/^Version: .*/Version: $fixed/" "$work/package/DEBIAN/control"
  dpkg-deb --root-owner-group -b "$work/package" "$deb" >/dev/null
  rm -rf "$work"
  echo "$deb: $version -> $fixed"
done
