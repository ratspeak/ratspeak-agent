"""Clear local conversations before the receiver starts on the next boot."""
import json
import re
import shutil
import sqlite3
import subprocess
import sys
from contextlib import closing
from pathlib import Path

from . import units, voice, wiki


def clear_chats(database: Path, link: str, request_id: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{32}", request_id):
        raise ValueError("Invalid chat erasure request.")
    with closing(sqlite3.connect(database.as_uri() + "?mode=rw", uri=True)) as db, db:
        db.execute("PRAGMA secure_delete=ON")
        db.execute("BEGIN IMMEDIATE")
        identity = json.loads(db.execute("SELECT value FROM metadata WHERE key='identity'").fetchone()[0])
        if link != f"lxma://{identity['address']}:{identity['key']}":
            raise ValueError("The request belongs to a different Pi.")
        last = db.execute("SELECT value FROM metadata WHERE key='last_chat_erasure'").fetchone()
        # Power can fail between this commit and retiring the request on the boot volume.
        if last != (request_id,):
            db.execute("DELETE FROM inbox")
            db.execute("DELETE FROM jobs WHERE kind IN ('plain','message','receipt')")
            db.execute("DELETE FROM quarantine")
            db.execute("INSERT OR REPLACE INTO metadata VALUES ('last_chat_erasure', ?)", (request_id,))
        queued = {item["path"] for (files,) in db.execute("SELECT attachments FROM jobs WHERE attachments IS NOT NULL")
                  for item in json.loads(files or "[]")}
    with closing(sqlite3.connect(database.as_uri() + "?mode=rw", uri=True)) as db:
        db.execute("PRAGMA wal_checkpoint(TRUNCATE)")              # the log still holds the erased rows
    for folder in ("incoming", "outbox"):                           # photos, voice notes, and files sent or received
        for item in (database.parent / "files" / folder).glob("*"):
            if str(item.resolve()) in queued:
                continue
            if item.is_dir() and not item.is_symlink():
                shutil.rmtree(item, ignore_errors=True)
            else:
                item.unlink(missing_ok=True)
    shutil.rmtree(voice.FOLDER / "heard", ignore_errors=True)   # what earlier voice notes said
    wiki.VIEWS.unlink(missing_ok=True)                           # what each person looked up last


def apply_chat_request(boot: Path) -> bool:
    pending = boot / "erase-chats.json"
    if not pending.exists():
        return True
    try:
        request = json.loads(pending.read_text())
        if not re.fullmatch(r"lxma://[0-9a-f]{32}:[0-9a-f]{128}", request["link"]):
            raise ValueError("Invalid Pi address.")
        subprocess.run(["runuser", "-u", units.USER, "--", sys.executable, "-m", "ratspeak_pi.maintenance",
                        request["link"], request["id"]], check=True, cwd=units.PREFIX)
    except (OSError, ValueError, TypeError, KeyError, subprocess.CalledProcessError):
        pending.replace(boot / "erase-chats-failed.json")
        return False
    (boot / "erase-chats-failed.json").unlink(missing_ok=True)
    pending.unlink()
    return True


if __name__ == "__main__":
    clear_chats(units.PROFILE / "state.sqlite3", *sys.argv[1:])
