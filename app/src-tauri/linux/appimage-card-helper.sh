set -eu

# The AppImage runtime derives its extraction directory from the image contents.
# Start in a fresh private directory so the elevated process never reuses another user's files.
scratch=$(mktemp -d /tmp/ratspeak-card-helper.XXXXXXXXXX)
trap 'rmdir "$scratch" 2>/dev/null || true' EXIT
TMPDIR="$scratch" "$@"
