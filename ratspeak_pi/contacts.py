"""Contacts and reviewed exchanges belonging to one installed agent."""
from __future__ import annotations

import base64
import binascii
from contextlib import closing, contextmanager
import fcntl
import hashlib
import json
import os
import re
import shutil
import sqlite3
import threading
import time
from pathlib import Path

from .contact_mailbox import (Mailbox, FIELDS, PEER_TITLE, MAX_FILE, key_ok, valid_text, encode, draft_grammar,
                              descriptor, migrate, safe_name, sha256_file)

ACTIONS = {"send", "send-reply", "draft-request", "draft-reply", "decline", "decline-notify", "retry"}
MAX_CONTACTS, MAX_RECORDS, MAX_EVENTS = 64, 4000, 128
MAX_FILES_BYTES = 200 * 1024 * 1024     # exchange files kept before closed exchanges' files go
CLOSED = ("answered", "returned", "declined", "notified", "expired")
FILES = "contacts-files"
# A beta participation gate, intentionally discoverable in the source.
BETA_PASSWORD = "blackout"
BETA_FILE = "contacts-beta"
SCHEMA = """
CREATE TABLE IF NOT EXISTS contacts (
    address TEXT PRIMARY KEY, public_key TEXT NOT NULL DEFAULT '', name TEXT NOT NULL,
    paused INTEGER NOT NULL DEFAULT 0, owner_address TEXT NOT NULL DEFAULT '',
    reported_name TEXT NOT NULL DEFAULT '', last_seen REAL
);
CREATE TABLE IF NOT EXISTS contact_events (
    native_id TEXT PRIMARY KEY, peer TEXT NOT NULL, payload TEXT NOT NULL, created REAL NOT NULL,
    attachment TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS contact_actions (
    number INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL, peer TEXT NOT NULL,
    choice TEXT NOT NULL, payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued',
    error TEXT NOT NULL DEFAULT '', created REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS contact_seen (id TEXT PRIMARY KEY, peer TEXT NOT NULL, created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS contact_outbox (
    digest TEXT PRIMARY KEY, peer TEXT NOT NULL, payload TEXT NOT NULL, created REAL NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0, next_attempt REAL NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS contact_runtime (id INTEGER PRIMARY KEY, heartbeat REAL NOT NULL);
"""


def connect(state):
    db = sqlite3.connect(Path(state) / "contacts.sqlite3", timeout=5)
    db.execute("PRAGMA journal_mode=WAL")
    db.executescript(SCHEMA)
    if "attachment" not in {row[1] for row in db.execute("PRAGMA table_info(contact_events)")}:
        try:
            db.execute("ALTER TABLE contact_events ADD COLUMN attachment TEXT NOT NULL DEFAULT ''")
        except sqlite3.OperationalError:
            pass  # Another process added it first.
    migrate(db)
    return db


def files_size(root):
    return sum(f.stat().st_size for f in root.rglob("*") if f.is_file() and not f.is_symlink()) if root.is_dir() else 0


def prune_files(state, db, room=0):
    """Drop abandoned staging and inbox folders, then closed exchanges' files until `room` bytes fit."""
    root = Path(state) / FILES
    now = time.time()
    for folder, age in (("staged", 86400), ("inbox", 600)):
        for item in (root / folder).glob("*"):
            if item.is_dir() and item.stat().st_mtime < now - age and (
                    folder == "staged" or not db.execute("SELECT 1 FROM contact_events WHERE native_id=?", (item.name,)).fetchone()):
                shutil.rmtree(item, ignore_errors=True)
    total = files_size(root)
    if total + room <= MAX_FILES_BYTES:
        return True
    if db.execute("SELECT 1 FROM sqlite_master WHERE name='friend_mail'").fetchone():
        marks = ",".join("?" * len(CLOSED))
        for (key,) in db.execute(f"SELECT id FROM friend_mail WHERE stage IN ({marks}) AND (attachment!='' OR reply_attachment!='') ORDER BY created", CLOSED).fetchall():
            folder = root / key
            if folder.is_dir():
                total -= files_size(folder)
                shutil.rmtree(folder, ignore_errors=True)
            if total + room <= MAX_FILES_BYTES:
                return True
    return False


def stage_file(state, key, name, data):
    """Hold an owner-chosen file for an exchange until the reviewed request or reply is sent."""
    if not key_ok(key):
        raise ValueError("Invalid request ID.")
    try:
        raw = base64.b64decode(data, validate=True) if isinstance(data, str) else b""
    except (binascii.Error, ValueError):
        raw = b""
    if not raw:
        raise ValueError("That file is empty.")
    if len(raw) > MAX_FILE:
        raise ValueError("Choose a file under 900 KB.")
    with closing(connect(state)) as db:
        if not prune_files(state, db, len(raw)):
            raise ValueError("The agent's A2A file storage is full.")
    folder = Path(state) / FILES / "staged" / key
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(mode=0o700, parents=True)
    name = safe_name(name)
    (folder / name).write_bytes(raw)
    return {"attachment": {"name": name, "size": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}}


def read_file(state, key, part):
    """The bytes of a request or reply file, for the owner to save."""
    if not key_ok(key) or part not in ("request", "reply"):
        raise ValueError("Invalid file request.")
    with closing(connect(state)) as db:
        row = db.execute(f"SELECT {'attachment' if part == 'request' else 'reply_attachment'} FROM friend_mail WHERE id=?", (key,)).fetchone()
    info = descriptor(json.loads(row[0])) if row and row[0] else None
    path = Path(state) / FILES / key / part / info["name"] if info else None
    if not path or path.is_symlink() or not path.is_file() or path.stat().st_size != info["size"]:
        raise ValueError("This file is no longer on the agent.")
    return {"name": info["name"], "data": base64.b64encode(path.read_bytes()).decode()}


def configuration(state):
    state = Path(state)
    cfg = json.loads((state / "config.json").read_text())
    manifest = json.loads((state / "profile/manifest.json").read_text())
    if cfg["link"] != "lxma://" + manifest["address"] + ":" + manifest["public_key"]:
        raise ValueError("The agent identity has changed.")
    return cfg


def owned(cfg):
    access = cfg.get("access", {})
    return access.get("mode") != "later" and bool(access.get("owners"))


def beta_enabled(state):
    try:
        return (Path(state) / BETA_FILE).read_text() == "1"
    except OSError:
        return False


def addresses(state):
    if not beta_enabled(state):
        return []
    with closing(connect(state)) as db:
        return [row[0] for row in db.execute("SELECT address FROM contacts")]


def contact_rows(db):
    cursor = db.execute("SELECT address,public_key,name,paused,owner_address,reported_name,last_seen FROM contacts ORDER BY name COLLATE NOCASE")
    return [dict(zip([d[0] for d in cursor.description], row)) for row in cursor]


def snapshot(state, before=None, current=None):
    if not beta_enabled(state):
        return {"beta_locked": True}
    cfg = configuration(state)
    with closing(connect(state)) as db:
        rows, total, cursor = [], 0, None
        if db.execute("SELECT 1 FROM sqlite_master WHERE name='friend_mail'").fetchone():
            total = db.execute("SELECT count(*) FROM friend_mail").fetchone()[0]
            boundary = before or [float("inf"), "f"*32]
            page = db.execute(f"SELECT peer,{FIELDS} FROM friend_mail WHERE (created,id)<(?,?) ORDER BY created DESC,id DESC LIMIT 400", boundary).fetchall()
            if page:
                cursor = [page[-1][8],page[-1][1]]
            # Include older open work while keeping every status response bounded.
            pending = db.execute(f"SELECT peer,{FIELDS} FROM friend_mail WHERE stage IN ('pending','generating','draft','clarification','interrupted') ORDER BY created,id LIMIT 128").fetchall()
            selected = db.execute(f"SELECT peer,{FIELDS} FROM friend_mail WHERE id=?", (current,)).fetchall()
            records = {row[1]:row for row in [*page,*pending,*selected]}
            rows = [dict(zip(["peer", *FIELDS.split(",")], row)) for row in records.values()]
        op = db.execute("SELECT request_id,peer,choice FROM contact_actions WHERE status IN ('queued','running') ORDER BY number LIMIT 1").fetchone()
        last = db.execute("SELECT request_id,peer,error FROM contact_actions ORDER BY number DESC LIMIT 1").fetchone()
        heartbeat = db.execute("SELECT heartbeat FROM contact_runtime WHERE id=1").fetchone()
        me = {"id": cfg["address"], "address": cfg["address"], "name": cfg["name"],
              "owner_address": (cfg["access"].get("owners") or [""])[0], "link": cfg["link"]}
        return dict(beta_locked=False, me=me, contacts=contact_rows(db), boxes={me["id"]: rows}, notes={me["id"]: []},
                    busy=bool(op), operation=dict(id=op[0],peer=op[1],choice=op[2],role=me["id"]) if op else None,
                    error=last[2] if last else "", error_request=last[0] if last else "",
                    error_peer=last[1] if last else "", claimed=owned(cfg),
                    ready=bool(heartbeat and heartbeat[0] > time.time()-330), version=1, total=total, cursor=cursor, before=before)


def management(state, request):
    """Local IPC only. The selected installed identity must match on every operation."""
    state = Path(state)
    cfg = configuration(state)
    if request.get("link") != cfg["link"]:
        raise ValueError("The selected agent has changed.")
    choice = request.get("choice", "state")
    if choice == "unlock":
        if request.get("password") != BETA_PASSWORD:
            raise ValueError("That password isn’t right. Try again.")
        from runtime.common import atomic_text
        atomic_text(state / BETA_FILE, "1")
        return snapshot(state)
    if not beta_enabled(state):
        if choice == "state":
            return {"beta_locked": True}
        raise ValueError("Unlock the Contacts beta first.")
    before, current = request.get("before"), request.get("current_id",request.get("id"))
    if before is not None and (not isinstance(before,list) or len(before)!=2 or
                              type(before[0]) not in (int,float) or not 0<=before[0]<=time.time()+60 or not key_ok(before[1])):
        raise ValueError("Invalid history position.")
    if current is not None and not key_ok(current):
        raise ValueError("Invalid request ID.")
    if choice == "state":
        return snapshot(state, before, current)
    if not owned(cfg):
        raise ValueError("Pair your agent with its owner first.")
    if choice == "stage":
        return stage_file(state, request.get("id"), request.get("name", ""), request.get("data"))
    if choice == "file":
        return read_file(state, request.get("id"), request.get("part"))
    peer = request.get("peer_address")
    with closing(connect(state)) as db, db:
        db.execute("BEGIN IMMEDIATE")
        if choice == "add":
            name = request.get("name", "").strip()
            raw = request.get("address", "").strip().lower()
            match = re.fullmatch(r"lxma://([0-9a-f]{32}):([0-9a-f]{128})(?:\?.*)?", raw)
            address, key = (match[1], match[2]) if match else (raw, "")
            if not key_ok(address) or address == cfg["address"] or not name or len(name) > 64:
                raise ValueError("Enter a name and another agent's Ratspeak or LXMF address.")
            old = db.execute("SELECT public_key FROM contacts WHERE address=?", (address,)).fetchone()
            if old and old[0] and key and old[0] != key:
                raise ValueError("The contact's public key does not match.")
            if not old and db.execute("SELECT count(*) FROM contacts").fetchone()[0] >= MAX_CONTACTS:
                raise ValueError("The contact list is full.")
            db.execute("INSERT INTO contacts(address,public_key,name) VALUES(?,?,?) ON CONFLICT(address) DO UPDATE SET name=excluded.name,public_key=CASE WHEN contacts.public_key='' THEN excluded.public_key ELSE contacts.public_key END", (address,key,name))
        elif choice == "pause":
            if type(request.get("paused")) is not bool:
                raise ValueError("Choose whether to pause this contact.")
            if db.execute("UPDATE contacts SET paused=? WHERE address=?", (request["paused"],peer)).rowcount != 1:
                raise ValueError("Choose an existing contact.")
        elif choice in ACTIONS:
            contact = db.execute("SELECT paused FROM contacts WHERE address=?", (peer,)).fetchone()
            key = request.get("id")
            if not contact or not key_ok(key):
                raise ValueError("Choose a contact and request.")
            if contact[0] and choice in ("send","draft-request"):
                raise ValueError("Resume this contact before sending a new request.")
            payload = {k:request[k] for k in ("question","context","answer","brief","clarification_answer","ready_to_draft","in_reply_to","attachment") if request.get(k) is not None}
            if any(not valid_text(v) for k,v in payload.items() if k not in ("ready_to_draft","attachment")):
                raise ValueError("Each field supports up to 2,000 characters.")
            if "attachment" in payload:
                info = descriptor(payload["attachment"])
                staged = state / FILES / "staged" / key / info["name"] if info else None
                recorded = db.execute("SELECT 1 FROM friend_mail WHERE id=? AND ? IN (attachment,reply_attachment)",
                                      (key, encode(info))).fetchone() if info and db.execute(
                                      "SELECT 1 FROM sqlite_master WHERE name='friend_mail'").fetchone() else None
                if choice not in ("send","send-reply") or not info or not (recorded or (staged.is_file() and not staged.is_symlink())):
                    raise ValueError("Attach the file again.")
            if "ready_to_draft" in payload and type(payload["ready_to_draft"]) is not bool:
                raise ValueError("Invalid drafting option.")
            payload.update(id=key, choice=choice, peer_address=peer)
            encoded = encode(payload)
            if len(encoded.encode()) > 15800:
                raise ValueError("Shorten this request before sending.")
            pending = db.execute("SELECT peer,payload FROM contact_actions WHERE status IN ('queued','running')").fetchone()
            if pending and pending != (peer,encoded):
                raise ValueError("Your agent is finishing the previous action.")
            if not pending:
                db.execute("INSERT INTO contact_actions(request_id,peer,choice,payload,created) VALUES(?,?,?,?,?)",
                           (key,peer,choice,encoded,time.time()))
        else:
            raise ValueError("Unknown contact action.")
    return snapshot(state, before, current)


def receive(state, event):
    """Only authenticated LXMF events from locally approved agents enter this inbox."""
    if event.get("title") != PEER_TITLE:
        return False
    state = Path(state)
    if not beta_enabled(state):
        return True
    cfg = configuration(state)
    if not owned(cfg) or event.get("verified") is not True or event.get("error") or event.get("destination") != cfg["address"]:
        return True
    peer, key, mid = event.get("source"), event.get("source_key"), event.get("message_id")
    if not key_ok(peer) or not isinstance(key,str) or not re.fullmatch("[0-9a-f]{128}",key) or not isinstance(mid,str) or not re.fullmatch("[0-9a-f]{64}",mid):
        return True
    text = event.get("text")
    if not isinstance(text,str) or len(text.encode()) > 15800:
        return True
    try:
        data = json.loads(text)
    except (ValueError, RecursionError):
        return True
    if not isinstance(data,dict) or not key_ok(data.get("id")):
        return True
    if data.get("kind") not in ("request","reply","receipt","reply-received","busy","decline","notice-received"):
        return True
    received = None
    if "attachment" in data:
        received = verified_file(state, event, data)
        if not received:
            return True
    with closing(connect(state)) as db, db:
        db.execute("BEGIN IMMEDIATE")
        contact = db.execute("SELECT public_key,paused FROM contacts WHERE address=?", (peer,)).fetchone()
        if not contact or contact[0] not in ("",key) or (contact[1] and data["kind"]=="request"):
            return True
        if db.execute("SELECT 1 FROM contact_seen WHERE id=?", (mid,)).fetchone():
            return True
        if db.execute("SELECT count(*) FROM contact_events").fetchone()[0] >= MAX_EVENTS or db.execute("SELECT count(*) FROM contact_events WHERE peer=?", (peer,)).fetchone()[0] >= 8:
            return True
        if db.execute("SELECT count(*) FROM contact_seen WHERE peer=? AND created>?", (peer,time.time()-60)).fetchone()[0] >= 30:
            return True
        if received:
            if not prune_files(state, db, received["info"]["size"]):
                return True
            folder = state / FILES / "inbox" / mid
            folder.mkdir(mode=0o700, parents=True, exist_ok=True)
            target = folder / received["info"]["name"]
            os.replace(received["path"], target)
            received = encode({"path": str(target), "info": received["info"]})
        db.execute("UPDATE contacts SET public_key=?,last_seen=? WHERE address=?", (key,time.time(),peer))
        db.execute("INSERT INTO contact_events VALUES (?,?,?,?,?)", (mid,peer,text,time.time(),received or ""))
        db.execute("INSERT INTO contact_seen VALUES (?,?,?)", (mid,peer,time.time()))
        db.execute("DELETE FROM contact_seen WHERE id NOT IN (SELECT id FROM contact_seen ORDER BY created DESC LIMIT 4096)")
    return True


def verified_file(state, event, data):
    """The one file the native helper saved with this signed frame, if it matches the frame's description."""
    info = descriptor(data["attachment"])
    found = [a for a in event.get("attachments") or [] if isinstance(a, dict) and a.get("kind") == "file"]
    if data["kind"] not in ("request", "reply") or not info or len(found) != 1 or not isinstance(found[0].get("path"), str):
        return None
    path = Path(found[0]["path"])
    folder = (Path(state) / "profile/files/incoming" / event["message_id"]).resolve()
    try:
        if path.is_symlink() or path.resolve().parent != folder or not path.is_file() or path.stat().st_size != info["size"]:
            return None
        return {"path": path, "info": info} if sha256_file(path) == info["sha256"] else None
    except OSError:
        return None


@contextmanager
def model_lock(state):
    with (Path(state) / ".inference.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def infer(state, cfg, op, key, system, text, **constraints):
    if not beta_enabled(state):
        raise ValueError("Unlock the Contacts beta first.")
    from . import llm, venice, dashboard, hermes
    import answer
    messages = [{"role":"system","content":system}, {"role":"user","content":text}]
    def usage(counts):
        try:
            dashboard.usage(state, counts)
        except (OSError, sqlite3.Error):
            pass
    if cfg.get("provider") == "hermes":
        # Owner approval is the only path that reaches this tool-capable provider.
        turn = hashlib.sha256(encode([key,system,text]).encode()).hexdigest()
        session = "ratspeak-contact-" + hashlib.sha256((cfg["address"]+key).encode()).hexdigest()[:32]
        result = hermes.exchange(state, {"session":session, "turn":turn,
            "text":system+"\n\n"+text})
        if result.get("usage"):
            usage(result["usage"])
        return result["reply"]
    def complete(*args):
        return answer.complete(*args, on_usage=usage)
    if cfg.get("provider") == "venice":
        return venice.complete(state, complete, messages)
    return answer.complete(f"http://127.0.0.1:{llm.PORT}/v1","local",messages,None,"none",
                           on_usage=usage, grammar=draft_grammar(constraints.get("required_result")))


def work(state, rpc, stop):
    """One durable queue per agent, independent of whether its setup window is open."""
    state = Path(state)
    with (state / ".contacts-worker.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        with closing(connect(state)) as db:
            with db:
                if db.execute("SELECT 1 FROM sqlite_master WHERE name='friend_mail'").fetchone():
                    db.execute("UPDATE friend_mail SET stage='failed' WHERE stage='queued' AND id NOT IN (SELECT json_extract(payload,'$.id') FROM contact_outbox)")
                db.execute("UPDATE contact_actions SET status='failed',error='Drafting or delivery was interrupted. Review the saved exchange before retrying.' WHERE status='running'")
            boxes = {}
            def mailbox(peer):
                if peer not in boxes:
                    def transmit(text, title):
                        digest = hashlib.sha256((peer+text).encode()).hexdigest()
                        with db:
                            if db.execute("SELECT count(*) FROM contact_outbox").fetchone()[0] >= MAX_EVENTS:
                                raise RuntimeError("The outgoing queue is full. Try again after pending deliveries finish.")
                            db.execute("INSERT OR IGNORE INTO contact_outbox(digest,peer,payload,created) VALUES(?,?,?,?)", (digest,peer,text,time.time()))
                    def generate(*args, **kwargs):
                        with model_lock(state):
                            return infer(state,configuration(state),*args,**kwargs)
                    boxes[peer] = Mailbox(db,state,peer,generate,transmit,lambda **event: None)
                return boxes[peer]
            pruned = 0
            while not stop.wait(.4):
                try:
                    with db:
                        db.execute("INSERT INTO contact_runtime VALUES(1,?) ON CONFLICT(id) DO UPDATE SET heartbeat=excluded.heartbeat", (time.time(),))
                    if not beta_enabled(state):
                        continue
                    cfg = configuration(state)
                    if not owned(cfg):
                        continue
                    # Bound new exchanges while retaining saved history.
                    full = db.execute("SELECT count(*) FROM sqlite_master WHERE name='friend_mail'").fetchone()[0] and db.execute("SELECT count(*) FROM friend_mail").fetchone()[0] >= MAX_RECORDS
                    if time.monotonic() > pruned + 60:
                        prune_files(state, db)
                        pruned = time.monotonic()
                    for mid, peer, payload, created, received in db.execute("SELECT native_id,peer,payload,created,attachment FROM contact_events ORDER BY created LIMIT 16").fetchall():
                        data = json.loads(payload)
                        box = mailbox(peer)
                        try:
                            if not full or box.row(data["id"]):
                                box.incoming(data, json.loads(received) if received else None)
                            elif data["kind"] == "request":
                                box.frame("busy", data["id"])
                        except RuntimeError:
                            break  # Keep the event until its acknowledgement can be queued.
                        except (ValueError, KeyError, TypeError):
                            pass
                        with db:
                            db.execute("DELETE FROM contact_events WHERE native_id=?", (mid,))
                        shutil.rmtree(state / FILES / "inbox" / mid, ignore_errors=True)
                        full = db.execute("SELECT count(*) FROM friend_mail").fetchone()[0] >= MAX_RECORDS
                    job = db.execute("SELECT number,peer,payload FROM contact_actions WHERE status='queued' ORDER BY number LIMIT 1").fetchone()
                    if job:
                        number, peer, payload = job
                        box = mailbox(peer)
                        data = json.loads(payload)
                        with db:
                            db.execute("UPDATE contact_actions SET status='running' WHERE number=?", (number,))
                        try:
                            contact = db.execute("SELECT paused FROM contacts WHERE address=?", (peer,)).fetchone()
                            if not contact or (contact[0] and data["choice"] in ("send","draft-request")):
                                raise ValueError("This contact is paused.")
                            if full and not box.row(data["id"]):
                                raise ValueError("Request storage is full.")
                            box.participants = {"your agent":cfg["name"], "contact":db.execute("SELECT name FROM contacts WHERE address=?", (peer,)).fetchone()[0]}
                            box.perform(data["choice"],data["id"],data)
                        except (ValueError,RuntimeError,OSError) as error:
                            with db:
                                db.execute("UPDATE contact_actions SET status='failed',error=? WHERE number=?", (str(error)[:280],number))
                        else:
                            with db:
                                db.execute("UPDATE contact_actions SET status='done' WHERE number=?", (number,))
                    deliveries = db.execute("SELECT digest,peer,payload,created,attempts,next_attempt FROM contact_outbox ORDER BY created").fetchall()
                    transmitted = 0
                    for digest, peer, text, created, attempts, next_attempt in deliveries:
                        data = json.loads(text)
                        box = mailbox(peer)
                        row = box.row(data["id"])
                        waiting = {"request":"queued", "reply":"replying", "decline":"notice-pending"}.get(data["kind"])
                        if waiting and (not row or row["stage"] != waiting):
                            with db:
                                db.execute("DELETE FROM contact_outbox WHERE digest=?", (digest,))
                            continue
                        if next_attempt > time.time() or transmitted >= 4:
                            continue
                        transmitted += 1
                        delivered = False
                        attachment = {}
                        if data.get("attachment"):
                            part = "request" if data["kind"] == "request" else "reply"
                            # A file can take minutes on a slow link; the helper sets its own airtime deadline.
                            attachment = {"attachment": str(state / FILES / data["id"] / part / data["attachment"]["name"]),
                                          "rpc_timeout": 630}
                        try:
                            delivered = bool(rpc("contacts_send", address=peer, text=text, **attachment).get("transport_delivered"))
                        except (OSError,ValueError,RuntimeError):
                            pass
                        # Requests and replies wait for a signed application receipt. A transport
                        # receipt alone cannot prove the peer admitted a message to its queue.
                        if not delivered or waiting:
                            attempts += 1
                            if attempts < 8:
                                with db:
                                    db.execute("UPDATE contact_outbox SET attempts=?,next_attempt=? WHERE digest=?",
                                               (attempts,time.time()+min(300,5*3**(attempts-1)),digest))
                                continue
                            failed = {"request":"failed","reply":"reply-failed"}.get(data["kind"])
                            if row and failed and row["stage"] == waiting:
                                box.stage(data["id"],failed)
                        with db:
                            db.execute("DELETE FROM contact_outbox WHERE digest=?", (digest,))
                    with db:
                        db.execute("DELETE FROM contact_actions WHERE status IN ('done','failed') AND number NOT IN (SELECT number FROM contact_actions ORDER BY number DESC LIMIT 64)")
                except (OSError, ValueError, sqlite3.Error):
                    stop.wait(2)


def start(state, rpc):
    stop = threading.Event()
    thread = threading.Thread(target=work,args=(state,rpc,stop),name="contacts",daemon=True)
    thread.start()
    return stop


if __name__ == "__main__":
    import sys
    from . import units
    try:
        raw = sys.stdin.buffer.read(1_300_001)
        data = json.loads(raw)
        if not isinstance(data,dict) or len(raw) > (1_300_000 if data.get("choice") == "stage" else 32768):
            raise ValueError("Invalid contact request.")
        print(encode({"ok":True,"state":management(units.STATE,data)}))
    except ValueError as error:
        print(encode({"ok":False,"error":str(error)[:280]}))
    except (OSError,KeyError,sqlite3.Error):
        print(encode({"ok":False,"error":"Contacts could not complete this action. Check the agent and try again."}))
