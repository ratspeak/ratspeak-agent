#!/bin/sh
# Runs once on the Pi's first boot, started by cloud-init from the card's boot partition.
# Installs Ratspeak Pi from the release the Mac app copied onto the card, with the settings it wrote, and from the
# downloads in offline/ when the card carries them. Progress goes to status.json and install.log beside this file,
# where the Mac app can read them later, and to the Mac app over the network while it runs.
set -u
BOOT=/boot/firmware/ratspeak-pi
exec >>"$BOOT/install.log" 2>&1
echo "== first boot $(date)"
[ -f /var/lib/ratspeak-pi/config.json ] && { echo "already installed"; exit 0; }
# status [--fresh] STATE DETAIL [KEY=JSON ...]: merged into status.json, or replacing it, as each attempt starts.
status() {
  python3 - "$BOOT/status.json" "$@" <<'PY'
import json, os, sys, time
path, *args = sys.argv[1:]
fresh = args[0] == "--fresh" and args.pop(0)
state, detail, *facts = args
try:
    with open(path) as f:
        current = {} if fresh else json.load(f)
except (OSError, ValueError):
    current = {}
current = current if isinstance(current, dict) else {}
current.update(state=state, detail=detail, updated=int(time.time()))
current.update((key, json.loads(value)) for key, value in (fact.split("=", 1) for fact in facts))
try:   # from the unpacked release, once this runs inside it
    from pathlib import Path
    from ratspeak_pi import install_status
    if current.get("stage"):
        current.update(install_status.fields(current, install_status.read(Path(path).with_name("settings.json"))))
except (ImportError, OSError, ValueError, KeyError, TypeError):
    pass
with open(path + ".new", "w") as f:
    f.write(json.dumps(current, indent=2) + "\n")
os.replace(path + ".new", path)
PY
}
setting() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2]))' "$BOOT/settings.json" "$1" 2>/dev/null; }
# The green light beats while installing, then stays solid when done or blinks fast after a failure, until the next start.
led=/sys/class/leds/ACT
[ -e "$led/trigger" ] || led=/sys/class/leds/led0
was=$(sed -n 's/.*\[\([^]]*\)\].*/\1/p' "$led/trigger" 2>/dev/null)
light() { [ -n "$was" ] && echo "$1" > "$led/trigger"; } 2>/dev/null
blink() { light timer && { echo 100 > "$led/delay_on"; echo 100 > "$led/delay_off"; } 2>/dev/null; }
fail() { status failed "$1"; blink; exit 1; }
light heartbeat
started=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
from_card=false
[ -d "$BOOT/offline" ] && from_card=true
status --fresh installing "Unpacking Ratspeak Pi" stage='"unpack"' started="${started:-null}" from_card=$from_card percent=0
offline=$(setting offline)
[ "$offline" = True ] && [ "$from_card" = false ] && fail "The offline downloads are missing from the card."
RELEASE=/tmp/ratspeak-pi-release
rm -rf "$RELEASE" && mkdir -p "$RELEASE"
tar -xzf "$BOOT/release.tar.gz" -C "$RELEASE" || fail "The release on the card could not be unpacked."
cd "$RELEASE"/* || fail "The release on the card could not be unpacked."
# Install progress for the Mac app, in a unit of its own so it outlives this script; setup stops it to start the
# setup page on the same port.
serve() {
  systemctl stop ratspeak-pi-install-status.service 2>/dev/null
  systemctl reset-failed ratspeak-pi-install-status.service 2>/dev/null
  systemd-run --unit=ratspeak-pi-install-status --collect -p Restart=on-failure -p RestartSec=5 \
    --working-directory="$PWD" python3 -m ratspeak_pi install-status
}
serve
venice=false
[ "$(setting provider)" = venice ] && venice=true
# Files on the card need no network, and dpkg no clock; Venice is reached over the internet.
if [ "$offline" != True ] && { [ "$from_card" = false ] || [ "$venice" = true ]; }; then
  status installing "Waiting for the network" stage='"network"'
  n=0
  # Without a clock battery a Pi starts at its image's date until the network sets the time, and certificates
  # and package signatures fail before then. Networks that block time servers get the time from a web server.
  clock_set() {
    [ "$(timedatectl show -p NTPSynchronized --value 2>/dev/null)" = yes ] && return 0
    [ "$n" -ge 6 ] || return 1
    now=$(curl -fsSI --max-time 10 http://deb.debian.org/ 2>/dev/null | tr -d '\r' | sed -n 's/^[Dd]ate: //p')
    [ -n "$now" ] && date -s "$now" >/dev/null
  }
  probe=https://huggingface.co/robots.txt
  [ "$venice" = true ] && probe='https://api.venice.ai/api/v1/models?type=text'
  until clock_set && curl -fsS --max-time 10 -o /dev/null "$probe"; do
    n=$((n + 1))
    [ "$n" -ge 60 ] && fail "No internet after 10 minutes. Check the Pi’s Wi-Fi or Ethernet connection."
    sleep 10
  done
fi
[ "$from_card" = true ] && export RATSPEAK_OFFLINE_DIR="$BOOT/offline"
status installing "Installing system packages" stage='"packages"'
sh ./install.sh --settings "$BOOT/settings.json" || {
  grep -q '"state": "failed"' "$BOOT/status.json" || status failed "Setup stopped. See install.log on the card."
  # Setup stops the progress server just before the setup page starts; keep reporting the failure.
  systemctl is-active --quiet ratspeak-pi-page.service || serve
  blink
  exit 1; }
systemctl stop ratspeak-pi-install-status.service 2>/dev/null   # normally stopped by setup already
light default-on
# The setup code's secret and the monitor key are not needed on the card once the Pi is installed.
rm -f "$BOOT/settings.json"
echo "== done $(date)"
