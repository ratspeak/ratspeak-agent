#!/bin/sh
# Ratspeak Pi installer. Run on 64-bit Raspberry Pi OS (Trixie or later):
#   sudo ./install.sh            from an unpacked release
# Any arguments go to `ratspeak-pi setup` (try --help). Safe to run again: it updates the program and re-runs setup.
set -eu
[ "$(id -u)" = 0 ] || { echo "Run this with sudo." >&2; exit 1; }
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = aarch64 ] || {
  echo "This needs 64-bit Raspberry Pi OS (aarch64). Re-flash with the 64-bit Lite image." >&2; exit 1; }
# The pinned llama.cpp build links glibc 2.38: Raspberry Pi OS based on Debian 13 "Trixie" (October 2025) or later.
GLIBC=$(ldd --version 2>/dev/null | head -1 | grep -o '[0-9]*\.[0-9]*$' || echo 0.0)
[ "${GLIBC%%.*}" -gt 2 ] || { [ "${GLIBC%%.*}" -eq 2 ] && [ "${GLIBC##*.}" -ge 38 ]; } || {
  echo "This needs Raspberry Pi OS Trixie or later (found glibc $GLIBC). Re-flash with the current 64-bit Lite image." >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
for f in ratspeak_pi/__main__.py runtime/agent_rat.py bin/ratspeak-pi-native; do
  [ -f "$HERE/$f" ] || { echo "Incomplete release: $f is missing." >&2; exit 1; }
done
need=""
# RATSPEAK_OFFLINE_DIR: install from the downloads on the card and fetch nothing, so no clock or network is waited for.
if [ -n "${RATSPEAK_OFFLINE_DIR:-}" ] && ! command -v qrencode >/dev/null; then
  (cd "$RATSPEAK_OFFLINE_DIR" && printf '%s\n' \
    '8a52834e626c6f9c662005e4db5ee45043e1ed50f6a7833ff7e18c2f3a8b4edb  libqrencode4_4.1.1-2_arm64.deb' \
    '5d2832d42f85bb915ee6c5038c1dfa3e94035cbb54816e5173bab8362dfb4291  qrencode_4.1.1-2_arm64.deb' | sha256sum -c - && \
    dpkg -i libqrencode4_4.1.1-2_arm64.deb qrencode_4.1.1-2_arm64.deb)
fi
if [ -n "${RATSPEAK_OFFLINE_DIR:-}" ] && ! python3 -c 'import astral' 2>/dev/null; then
  (cd "$RATSPEAK_OFFLINE_DIR" && printf '%s\n' \
    '552ee2e7fa100b647df799d00f41c38232b4eba050ab5426df681edeefbf8c63  python3-astral_3.2-2_all.deb' | sha256sum -c - && \
    dpkg -i python3-astral_3.2-2_all.deb)
fi
for tool in python3 curl qrencode; do command -v "$tool" >/dev/null 2>&1 || need="$need $tool"; done
python3 -c 'import astral' 2>/dev/null || need="$need python3-astral"     # /sun and /moon
if [ -z "${RATSPEAK_OFFLINE_DIR:-}" ] && ! command -v pdftotext >/dev/null; then
  need="$need poppler-utils"   # Read selectable text from PDF attachments.
fi
ldconfig -p | grep -q 'libgomp\.so\.1' || need="$need libgomp1"   # llama-server links OpenMP
# Cloud-init images normally provide this. Offline pairing also works over the card.
if [ -z "${RATSPEAK_OFFLINE_DIR:-}" ] && ! python3 -c 'from cryptography.hazmat.primitives.ciphers.aead import AESGCM' 2>/dev/null; then
  need="$need python3-cryptography"
fi
if ! ldconfig -p | grep -q 'libatomic\.so\.1'; then   # Vosk voice transcription
  if [ -z "${RATSPEAK_OFFLINE_DIR:-}" ]; then
    need="$need libatomic1"
  elif [ -f "$RATSPEAK_OFFLINE_DIR/libatomic1_14.2.0-19_arm64.deb" ]; then
    (cd "$RATSPEAK_OFFLINE_DIR" && printf '%s\n' \
      '7ccff90234484bf21ae5ecb79d25b6a84b2f4038f65dd139085b2e3a937cd8dc  libatomic1_14.2.0-19_arm64.deb' | sha256sum -c - && \
      dpkg -i libatomic1_14.2.0-19_arm64.deb)
  fi
fi
if [ -n "$need" ]; then
  [ -z "${RATSPEAK_OFFLINE_DIR:-}" ] || { echo "The base operating system is missing:$need"; exit 1; }
  echo "Installing:$need"
  apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $need ca-certificates
fi
if [ -n "${RATSPEAK_OFFLINE_DIR:-}" ] && [ -f "$RATSPEAK_OFFLINE_DIR/wiki-reader.whl" ]; then
  (cd "$RATSPEAK_OFFLINE_DIR" && printf '%s\n' '791fccae1b3364c310320326dfea0277bc4677cb72719472ab9591750dd4a5e8  wiki-reader.whl' | sha256sum -c -)
  python3 - "$RATSPEAK_OFFLINE_DIR/wiki-reader.whl" <<'PY'
import sys, sysconfig, zipfile
with zipfile.ZipFile(sys.argv[1]) as wheel:
    wheel.extractall(sysconfig.get_path("platlib"))
from libzim.reader import Archive
PY
fi
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || {
  echo "Python 3.11 or newer is needed." >&2; exit 1; }
PREFIX=/opt/ratspeak-pi
mkdir -p "$PREFIX"
rm -rf "$PREFIX/ratspeak_pi" "$PREFIX/runtime" "$PREFIX/bin"
cp -R "$HERE/ratspeak_pi" "$HERE/runtime" "$HERE/bin" "$PREFIX/"
chmod 755 "$PREFIX/bin/ratspeak-pi-native"
printf '#!/bin/sh\ncd %s && exec python3 -m ratspeak_pi "$@"\n' "$PREFIX" > /usr/local/bin/ratspeak-pi
chmod 755 /usr/local/bin/ratspeak-pi
exec /usr/local/bin/ratspeak-pi setup "$@"
