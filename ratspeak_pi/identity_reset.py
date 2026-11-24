"""Replace the messaging identity at boot, before the receiver opens its profile."""
import fcntl
import json
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
from contextlib import closing
from pathlib import Path

from . import units

LINK = re.compile(r"lxma://[0-9a-f]{32}:[0-9a-f]{128}")


def public_link(identity):
    return f"lxma://{identity['address']}:{identity['public_key']}"


def replace_identity(request):
    from common import atomic_json, native_identity, profile_manifest
    from agent_rat import setup_lock

    old, new = request["old"], request["new"]
    if not all(isinstance(link, str) and LINK.fullmatch(link) for link in (old, new)) or old == new:
        raise ValueError("Invalid replacement address.")
    seed = bytes.fromhex(request["seed"])
    if len(seed) != 64:
        raise ValueError("Invalid replacement key.")
    profile = units.PROFILE
    binary = str(units.PREFIX / "bin/ratspeak-pi-native")
    cfg_path = units.STATE / "config.json"
    with setup_lock(profile), (profile / "service.lock").open("a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest = profile_manifest(profile)
        cfg = json.loads(cfg_path.read_text())
        key_path = profile / "identity"
        current = public_link(native_identity(binary, key_path))
        if any(link not in (old, new) for link in (current, cfg["link"], public_link(manifest))):
            raise ValueError("This request belongs to another Pi.")
        with tempfile.NamedTemporaryFile(dir=profile, delete=False) as tmp:
            tmp.write(seed)
            tmp.flush()
            os.fsync(tmp.fileno())
        staged = Path(tmp.name)
        try:
            identity = native_identity(binary, staged)
            if public_link(identity) != new:
                raise ValueError("The replacement key does not match its address.")
            database = profile / "state.sqlite3"
            with closing(sqlite3.connect(database.as_uri() + "?mode=rw", uri=True)) as db, db:
                db.execute("PRAGMA foreign_keys=ON")
                db.execute("BEGIN IMMEDIATE")
                record = json.loads(db.execute("SELECT value FROM metadata WHERE key='identity'").fetchone()[0])
                owner = f"lxma://{record['address']}:{record['key']}"
                if owner not in (old, new):
                    raise ValueError("The chat database belongs to another Pi.")
                if owner == old:
                    # Delivery attempts and handshakes signed by the retired key cannot be resumed.
                    db.execute("DELETE FROM jobs WHERE kind IN ('hello','ack','receipt')")
                    db.execute("DELETE FROM attempts")
                    db.execute("UPDATE jobs SET status='uncertain',keep_until=NULL WHERE status IN ('sending','queued')")
                    db.execute("UPDATE inbox SET unread=0")
                    value = json.dumps(dict(address=identity["address"], key=identity["public_key"]),
                                       sort_keys=True, separators=(",", ":"))
                    db.execute("UPDATE metadata SET value=? WHERE key='identity'", (value,))
            if current == old:
                staged.replace(key_path)
            manifest.update(identity)
            atomic_json(profile / "manifest.json", manifest)
            cfg.update(address=identity["address"], link=new)
            atomic_json(cfg_path, cfg)
        finally:
            staged.unlink(missing_ok=True)


def apply_request(boot: Path):
    pending = boot / "new-identity.json"
    if not pending.exists():
        return True
    from .setup import sync_status
    try:
        with pending.open("rb") as source:
            subprocess.run(["runuser", "-u", units.USER, "--", sys.executable, "-m", "ratspeak_pi.identity_reset"],
                           stdin=source, cwd=units.PREFIX, check=True)
        sync_status()
        pending.unlink()
        (boot / "identity-error").unlink(missing_ok=True)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError):
        # The same identity request can finish after power loss at any point.
        (boot / "identity-error").touch()
        return False
    return True


if __name__ == "__main__":
    sys.path.insert(0, str(units.PREFIX / "runtime"))
    replace_identity(json.load(sys.stdin))
