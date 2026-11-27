"""Durable, default-deny Agent Rat correspondence policy.

The native transport is the cryptographic boundary: ``verified`` is true only
after signature, public-key/source binding, and recipient verification. Nothing
in this module executes messages, changes networking, or forwards correspondence.
"""

import hmac
import json
import os
import re
import secrets
import shutil
import sqlite3
import stat
import threading
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path


RETICULUM_REV = "5e35c9f9d9f40d44846aaa5e11009e03490511d3"
LXMF_REV = "70fa2242119857c63c2132ec0e36d731dd364d39"
PROTOCOL = "agent-rat"
VERSION = 1
INVITATION_SECONDS = 1800
WORK_SECONDS = 900
HELLO_RECOVERY_SECONDS = 300
ACK_DELAY_SECONDS = 3
ACK_RETRY_SECONDS = 15
MAX_TEXT_BYTES = 16 * 1024
MAX_PENDING = 256
MAX_JOBS = 20000
MAX_INBOX = 10000
MAX_IMPORTED = 50000
MAX_QUARANTINE = 256
MAX_ATTEMPTS = 1000
MAX_PEERS = 256
MAX_KEEP_SECONDS = 7 * 24 * 3600
MAX_ATTACHMENT_BYTES = 900_000          # one per message; the native message limit is 1 MB
MAX_RECEIVED_BYTES = 200 * 1024 * 1024  # received files kept on disk before the oldest go
RETRY_MIN_SECONDS = 30
RETRY_MAX_SECONDS = 300
TERMINAL = {"cancelled", "expired", "deadline_expired", "revoked", "failed"}
INVITE_FIELDS = {
    "protocol", "version", "pairing_id", "pairing_token", "inviter_address",
    "inviter_key", "expires_at", "reticulum_rev", "lxmf_rev",
}
HELLO_FIELDS = {"protocol", "version", "type", "pairing_id", "pairing_token", "hello_id", "sender_key"}
ACK_FIELDS = {"protocol", "version", "type", "pairing_id", "hello_id", "sender_key"}
MESSAGE_FIELDS = {"protocol", "version", "type", "message_id", "body"}
RECEIPT_FIELDS = {"protocol", "version", "type", "message_id"}


def _hex(value, length):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{%d}" % length, value) is not None


def _thread(value):
    if not isinstance(value, str) or not re.fullmatch(r"(?:[0-9a-f]{2}){0,64}", value):
        raise ValueError("Invalid LXMF thread ID")
    return value


def _json(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False, sort_keys=True)


def _no_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _utc(timestamp):
    return datetime.fromtimestamp(timestamp, timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


def _expiration(value):
    if not isinstance(value, str) or len(value) > 40 or not value.endswith(("Z", "+00:00")):
        raise ValueError("expires_at must be an explicit UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed.timestamp()
    except (ValueError, OverflowError) as exc:
        raise ValueError("invalid invitation expiration") from exc


class Policy:
    def __init__(self, db_path, own_address, own_key, label):
        if not _hex(own_address, 32) or not _hex(own_key, 128):
            raise ValueError("invalid local public identity")
        self.own_address, self.own_key = own_address, own_key
        self.label = self._name(label)
        self._lock = threading.RLock()
        self._paused = False
        path = os.fspath(db_path)
        self.files = Path(path).parent / "files"
        if os.path.lexists(path) and not stat.S_ISREG(os.lstat(path).st_mode):
            raise ValueError("policy database must be a regular file, never a symlink")
        flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(path, flags, 0o600)
        try:
            if not stat.S_ISREG(os.fstat(fd).st_mode):
                raise ValueError("policy database must be a regular file")
            os.fchmod(fd, 0o600)
        finally:
            os.close(fd)
        self.db = sqlite3.connect(path, timeout=30, isolation_level=None, check_same_thread=False)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.executescript("""
            CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS peers (
                address TEXT PRIMARY KEY, public_key TEXT, name TEXT,
                admitted INTEGER NOT NULL DEFAULT 0, ever_admitted INTEGER NOT NULL DEFAULT 0,
                revoked INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, updated_at REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS attempts (
                id TEXT PRIMARY KEY, inviter_address TEXT NOT NULL, pairing_id TEXT NOT NULL,
                role TEXT NOT NULL, token TEXT NOT NULL, peer_address TEXT, peer_key TEXT,
                peer_name TEXT, hello_id TEXT, created_at REAL NOT NULL, expires_at REAL NOT NULL,
                work_started_at REAL, deadline REAL, status TEXT NOT NULL,
                hello_count INTEGER NOT NULL DEFAULT 0, ack_count INTEGER NOT NULL DEFAULT 0,
                last_hello_at REAL, last_ack_at REAL, ack_confirmed INTEGER NOT NULL DEFAULT 0,
                UNIQUE(inviter_address, pairing_id)
            );
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, address TEXT NOT NULL,
                text TEXT NOT NULL, title TEXT NOT NULL, attempt_id TEXT REFERENCES attempts(id),
                message_id TEXT, status TEXT NOT NULL, created_at REAL NOT NULL,
                available_at REAL NOT NULL, claimed_at REAL, finished_at REAL,
                native_id TEXT, error TEXT, keep_until REAL
            );
            CREATE INDEX IF NOT EXISTS jobs_pending ON jobs(status, available_at);
            CREATE UNIQUE INDEX IF NOT EXISTS ordinary_app_id ON jobs(message_id) WHERE kind='message';
            CREATE TABLE IF NOT EXISTS inbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT, native_id TEXT NOT NULL UNIQUE,
                message_id TEXT, source TEXT NOT NULL, title TEXT NOT NULL, text TEXT NOT NULL,
                received_at REAL NOT NULL, unread INTEGER NOT NULL DEFAULT 1,
                suppressed INTEGER NOT NULL DEFAULT 0,
                UNIQUE(source, message_id)
            );
            CREATE TABLE IF NOT EXISTS imported (
                native_id TEXT PRIMARY KEY, received_at REAL NOT NULL, classification TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS quarantine (
                id INTEGER PRIMARY KEY AUTOINCREMENT, native_id TEXT, source TEXT,
                received_at REAL NOT NULL, reason TEXT NOT NULL, raw_b64 TEXT, text TEXT
            );
        """)
        with self._transaction():
            identity = self.db.execute("SELECT value FROM metadata WHERE key='identity'").fetchone()
            expected = _json({"address": own_address, "key": own_key})
            if identity and identity["value"] != expected:
                raise ValueError("policy database belongs to a different identity")
            self.db.execute("INSERT OR IGNORE INTO metadata VALUES ('identity', ?)", (expected,))
            columns = {row["name"] for row in self.db.execute("PRAGMA table_info(inbox)")}
            if "reply_draft" not in columns:
                self.db.execute("ALTER TABLE inbox ADD COLUMN reply_draft TEXT")
            if "suppressed" not in columns:
                self.db.execute("ALTER TABLE inbox ADD COLUMN suppressed INTEGER NOT NULL DEFAULT 0")
            if "plain" not in {row["name"] for row in self.db.execute("PRAGMA table_info(peers)")}:
                # Contacts exchange ordinary LXMF text instead of Agent Rat envelopes.
                self.db.execute("ALTER TABLE peers ADD COLUMN plain INTEGER NOT NULL DEFAULT 0")
            job_columns = {row["name"] for row in self.db.execute("PRAGMA table_info(jobs)")}
            if "keep_until" not in job_columns:
                self.db.execute("ALTER TABLE jobs ADD COLUMN keep_until REAL")
            if "attachments" not in job_columns:
                self.db.execute("ALTER TABLE jobs ADD COLUMN attachments TEXT")
            if "attachments" not in columns:
                self.db.execute("ALTER TABLE inbox ADD COLUMN attachments TEXT")
            if "thread" not in columns:
                self.db.execute("ALTER TABLE inbox ADD COLUMN thread TEXT NOT NULL DEFAULT ''")
            if "thread" not in job_columns:
                self.db.execute("ALTER TABLE jobs ADD COLUMN thread TEXT NOT NULL DEFAULT ''")
            if "context_through" not in job_columns:
                self.db.execute("ALTER TABLE jobs ADD COLUMN context_through INTEGER")
            # Preserve pre-upgrade revocation decisions without releasing old unread work.
            self.db.execute("UPDATE inbox SET suppressed=1 WHERE unread=1 AND source IN (SELECT address FROM peers WHERE revoked=1)")

    @staticmethod
    def _name(value):
        if value is None:
            return None
        if not isinstance(value, str) or len(value.encode("utf-8")) > 128:
            raise ValueError("local label must be at most 128 UTF-8 bytes")
        return value

    @contextmanager
    def _transaction(self):
        with self._lock:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                yield
            except BaseException:
                self.db.rollback()
                raise
            else:
                self.db.commit()

    def close(self):
        with self._lock:
            self.db.close()

    def set_paused(self, paused):
        with self._lock:
            self._paused = bool(paused)

    def _count(self, table):
        return self.db.execute("SELECT COUNT(*) FROM " + table).fetchone()[0]

    def _room(self):
        pending = self.db.execute("SELECT COUNT(*) FROM jobs WHERE status IN ('queued','sending')").fetchone()[0]
        return pending < MAX_PENDING and self._count("jobs") < MAX_JOBS

    def _queue(self, kind, address, payload, now, *, attempt_id=None, message_id=None, title="", available_at=None, keep_until=None,
               attachments=None, thread="", context_through=None):
        if not self._room():
            raise ValueError("outbox capacity reached; preserved history requires owner review")
        result = self.db.execute(
            "INSERT INTO jobs(kind,address,text,title,attempt_id,message_id,status,created_at,available_at,keep_until,attachments,thread,context_through)"
            " VALUES (?,?,?,?,?,?,'queued',?,?,?,?,?,?)",
            (kind, address, payload if kind == "plain" else _json(payload), title, attempt_id, message_id, now,
             now if available_at is None else available_at, keep_until, _json(attachments) if attachments else None,
             thread, context_through),
        )
        return result.lastrowid

    def _peer(self, address):
        return self.db.execute("SELECT * FROM peers WHERE address=?", (address,)).fetchone()

    def _peer_allowed(self, address, key=None, admitted=False):
        peer = self._peer(address)
        if peer is None:
            return not admitted
        return not peer["revoked"] and (not admitted or bool(peer["admitted"])) and (
            key is None or peer["public_key"] is None or hmac.compare_digest(peer["public_key"], key)
        )

    def _admit(self, address, key, name, now):
        if not self._peer_allowed(address, key):
            raise ValueError("peer is revoked or its public key conflicts")
        if self._peer(address) is None and self._count("peers") >= MAX_PEERS:
            raise ValueError("peer capacity reached")
        self.db.execute("""
            INSERT INTO peers(address,public_key,name,admitted,ever_admitted,created_at,updated_at)
            VALUES (?,?,?,1,1,?,?) ON CONFLICT(address) DO UPDATE SET
            public_key=excluded.public_key, name=COALESCE(excluded.name,peers.name),
            admitted=1, ever_admitted=1, updated_at=excluded.updated_at
        """, (address, key, name, now, now))

    def _expire(self, now):
        self.db.execute("""UPDATE attempts SET status='expired'
            WHERE status NOT IN ('cancelled','expired','deadline_expired','revoked','failed','paired')
            AND work_started_at IS NULL AND expires_at<=?""", (now,))
        self.db.execute("""UPDATE attempts SET status='deadline_expired'
            WHERE status NOT IN ('cancelled','expired','deadline_expired','revoked','failed','paired')
            AND deadline IS NOT NULL AND deadline<=?""", (now,))
        self.db.execute("""UPDATE jobs SET status='skipped', error='attempt closed', finished_at=?
            WHERE status='queued' AND attempt_id IN
            (SELECT id FROM attempts WHERE status IN ('cancelled','expired','deadline_expired','revoked','failed','paired'))""", (now,))

    def invite(self, peer_name=None):
        peer_name = self._name(peer_name)
        with self._transaction():
            now = time.time()
            if self._count("attempts") >= MAX_ATTEMPTS:
                raise ValueError("attempt capacity reached")
            pairing_id, token = secrets.token_hex(16), secrets.token_hex(32)
            expiration = now + INVITATION_SECONDS
            self.db.execute("""INSERT INTO attempts
                (id,inviter_address,pairing_id,role,token,peer_name,created_at,expires_at,status)
                VALUES (?,?,?,'inviter',?,?,?,?,'invited')""",
                (self.own_address + ":" + pairing_id, self.own_address, pairing_id, token, peer_name, now, expiration))
            return {"protocol": PROTOCOL, "version": VERSION, "pairing_id": pairing_id,
                    "pairing_token": token, "inviter_address": self.own_address, "inviter_key": self.own_key,
                    "expires_at": _utc(expiration), "reticulum_rev": RETICULUM_REV, "lxmf_rev": LXMF_REV}

    def _validate_invitation(self, invitation):
        if not isinstance(invitation, dict) or not INVITE_FIELDS <= set(invitation) or set(invitation) - INVITE_FIELDS - {"route_hints"}:
            raise ValueError("invalid invitation schema")
        if invitation["protocol"] != PROTOCOL or type(invitation["version"]) is not int or invitation["version"] != VERSION:
            raise ValueError("unsupported invitation protocol/version")
        for field, length in (("pairing_id", 32), ("pairing_token", 64), ("inviter_address", 32), ("inviter_key", 128)):
            if not _hex(invitation[field], length):
                raise ValueError("invalid invitation " + field)
        if invitation["reticulum_rev"] != RETICULUM_REV or invitation["lxmf_rev"] != LXMF_REV:
            raise ValueError("invitation source pins do not match")
        if invitation["inviter_address"] == self.own_address or invitation["inviter_key"] == self.own_key:
            raise ValueError("self-invitations are not allowed")
        if "route_hints" in invitation and len(_json(invitation["route_hints"]).encode("utf-8")) > 1024:
            raise ValueError("route hints exceed the invitation bound")
        return _expiration(invitation["expires_at"])

    def accept(self, invitation, peer_name=None):
        """Accept bounded parameters after the controller verifies key/address binding."""
        expiration = self._validate_invitation(invitation)
        name = self._name(peer_name)
        address, key = invitation["inviter_address"], invitation["inviter_key"]
        attempt_id = address + ":" + invitation["pairing_id"]
        with self._transaction():
            now = time.time()
            self._expire(now)
            existing = self.db.execute("SELECT * FROM attempts WHERE id=?", (attempt_id,)).fetchone()
            if existing:
                if existing["role"] != "acceptor" or existing["peer_key"] != key or not hmac.compare_digest(existing["token"], invitation["pairing_token"]) or abs(existing["expires_at"] - expiration) > 0.00001:
                    raise ValueError("invitation conflicts with the persisted attempt")
                return self._attempt_public(existing)
            if expiration <= now or expiration > now + INVITATION_SECONDS + 5:
                raise ValueError("invitation expired or exceeds its 30-minute validity bound")
            if not self._peer_allowed(address, key):
                raise ValueError("peer is revoked or its public key conflicts")
            if self._count("attempts") >= MAX_ATTEMPTS:
                raise ValueError("attempt capacity reached")
            hello_id = secrets.token_hex(16)
            self.db.execute("""INSERT INTO attempts
                (id,inviter_address,pairing_id,role,token,peer_address,peer_key,peer_name,hello_id,created_at,expires_at,status)
                VALUES (?,?,?,'acceptor',?,?,?,?,?,?,?,'waiting_ack')""",
                (attempt_id, address, invitation["pairing_id"], invitation["pairing_token"], address, key, name, hello_id, now, expiration))
            self._queue("hello", address, self._hello_payload(self._get_attempt(attempt_id)), now, attempt_id=attempt_id)
            return self._attempt_public(self._get_attempt(attempt_id))

    def _get_attempt(self, attempt_id):
        row = self.db.execute("SELECT * FROM attempts WHERE id=?", (attempt_id,)).fetchone()
        if row is None:
            raise ValueError("unknown attempt identifier")
        return row

    def _hello_payload(self, attempt):
        return {"protocol": PROTOCOL, "version": VERSION, "type": "hello", "pairing_id": attempt["pairing_id"],
                "pairing_token": attempt["token"], "hello_id": attempt["hello_id"], "sender_key": self.own_key}

    def _ack_payload(self, attempt):
        return {"protocol": PROTOCOL, "version": VERSION, "type": "ack", "pairing_id": attempt["pairing_id"],
                "hello_id": attempt["hello_id"], "sender_key": self.own_key}

    def _active_job(self, attempt_id):
        return self.db.execute("SELECT 1 FROM jobs WHERE attempt_id=? AND status IN ('queued','sending')", (attempt_id,)).fetchone() is not None

    def _schedule_ack(self, attempt, now):
        if attempt["ack_count"] >= 3 or self._active_job(attempt["id"]) or not self._room():
            return False
        available = max(now + ACK_DELAY_SECONDS, (attempt["last_ack_at"] or 0) + ACK_RETRY_SECONDS)
        if available >= attempt["deadline"]:
            return False
        self._queue("ack", attempt["peer_address"], self._ack_payload(attempt), now, attempt_id=attempt["id"], available_at=available)
        return True

    def _quarantine(self, event, reason, now):
        if self._count("quarantine") < MAX_QUARANTINE:
            def bounded(value, limit):
                return value.encode("utf-8", "replace")[:limit].decode("utf-8", "ignore") if isinstance(value, str) else None
            self.db.execute("INSERT INTO quarantine(native_id,source,received_at,reason,raw_b64,text) VALUES (?,?,?,?,?,?)",
                            (event.get("message_id") if _hex(event.get("message_id"), 64) else None,
                             event.get("source") if _hex(event.get("source"), 32) else None, now, reason,
                             bounded(event.get("raw_b64"), 175000), bounded(event.get("text"), MAX_TEXT_BYTES)))
        else:
            self.db.execute("INSERT INTO metadata VALUES ('quarantine_overflow','1') ON CONFLICT(key) DO UPDATE SET value=CAST(MIN(CAST(value AS INTEGER)+1,2147483647) AS TEXT)")
        return {"classification": "quarantine", "reason": reason}

    def ingest(self, event):
        """Import one native result. Invalid input is inert and never echoed."""
        if not isinstance(event, dict):
            event = {}
        with self._transaction():
            now = time.time()
            self._expire(now)
            native_id = event.get("message_id")
            if _hex(native_id, 64) and self.db.execute("SELECT 1 FROM imported WHERE native_id=?", (native_id,)).fetchone():
                return {"classification": "duplicate", "native_id": native_id}
            if self._count("imported") >= MAX_IMPORTED:
                return {"classification": "rejected", "reason": "import capacity reached"}
            valid_native = not (event.get("event") != "message" or event.get("verified") is not True or event.get("error")
                    or not _hex(native_id, 64) or not _hex(event.get("source"), 32)
                    or not _hex(event.get("source_key"), 128) or event.get("destination") != self.own_address
                    or event.get("source") == self.own_address)
            if not valid_native:
                result = self._quarantine(event, "native authentication failed", now)
            else:
                result = self._ingest_authenticated(event, now)
            if valid_native:
                self.db.execute("INSERT INTO imported VALUES (?,?,?)", (native_id, now, result["classification"]))
            return result

    def _ingest_authenticated(self, event, now):
        text, source, key = event.get("text"), event["source"], event["source_key"]
        if not isinstance(text, str):
            return self._quarantine(event, "invalid message text", now)
        try:
            size = len(text.encode("utf-8"))
        except UnicodeEncodeError:
            return self._quarantine(event, "invalid UTF-8 text", now)
        if size > MAX_TEXT_BYTES:
            return self._quarantine(event, "message exceeds size limit", now)
        try:
            payload = json.loads(text, object_pairs_hook=_no_duplicate_keys)
        except (ValueError, RecursionError):
            return self._ingest_plain(event, text, now)
        if not isinstance(payload, dict) or payload.get("protocol") != PROTOCOL:
            return self._ingest_plain(event, text, now)
        if type(payload.get("version")) is not int or payload["version"] != VERSION:
            return self._quarantine(event, "unsupported message envelope", now)
        kind = payload.get("type")
        if not isinstance(kind, str):
            return self._quarantine(event, "invalid envelope schema", now)
        fields = {"hello": HELLO_FIELDS, "ack": ACK_FIELDS, "message": MESSAGE_FIELDS, "receipt": RECEIPT_FIELDS}.get(kind)
        if fields is None or set(payload) != fields:
            return self._quarantine(event, "invalid envelope schema", now)
        if kind in ("hello", "ack"):
            if size > 4096 or not _hex(payload["pairing_id"], 32) or not _hex(payload["hello_id"], 32) or payload["sender_key"] != key:
                return self._quarantine(event, "invalid handshake fields", now)
            if kind == "hello":
                if not _hex(payload["pairing_token"], 64):
                    return self._quarantine(event, "invalid handshake fields", now)
                return self._ingest_hello(event, payload, now)
            return self._ingest_ack(event, payload, now)
        if not self._peer_allowed(source, key, admitted=True):
            return self._quarantine(event, "peer not admitted or key changed", now)
        if not _hex(payload["message_id"], 32):
            return self._quarantine(event, "invalid application message identifier", now)
        if kind == "receipt":
            job = self.db.execute("SELECT * FROM jobs WHERE kind='message' AND message_id=? AND address=?",
                                  (payload["message_id"], source)).fetchone()
            if job is None or job["status"] in ("queued", "skipped"):
                return self._quarantine(event, "receipt does not match a transmitted message", now)
            if job["status"] == "received":
                return {"classification": "duplicate_receipt", "message_id": payload["message_id"]}
            self.db.execute("UPDATE jobs SET status='received',finished_at=?,error=NULL WHERE id=?", (now, job["id"]))
            return {"classification": "receipt", "message_id": payload["message_id"], "meaning": "durably received by peer; agent reading is not implied"}
        body = payload["body"]
        if not isinstance(body, str):
            return self._quarantine(event, "invalid message body", now)
        try:
            body_size = len(body.encode("utf-8"))
        except UnicodeEncodeError:
            return self._quarantine(event, "invalid UTF-8 body", now)
        if body_size > MAX_TEXT_BYTES:
            return self._quarantine(event, "message body exceeds 16 KiB", now)
        duplicate = self.db.execute("SELECT id FROM inbox WHERE source=? AND message_id=?", (source, payload["message_id"])).fetchone()
        if duplicate:
            return {"classification": "duplicate_message", "message_id": payload["message_id"]}
        if self._count("inbox") >= MAX_INBOX or not self._room():
            return self._quarantine(event, "inbox or receipt queue capacity reached", now)
        title = event.get("title", "")
        try:
            valid_title = isinstance(title, str) and len(title.encode("utf-8")) <= 1024
        except UnicodeEncodeError:
            valid_title = False
        if not valid_title:
            return self._quarantine(event, "invalid title", now)
        cursor = self.db.execute("INSERT INTO inbox(native_id,message_id,source,title,text,received_at,thread) VALUES (?,?,?,?,?,?,?)",
                                 (event["message_id"], payload["message_id"], source, title, body, now, _thread(event.get("thread", ""))))
        self._queue("receipt", source, {"protocol": PROTOCOL, "version": VERSION, "type": "receipt", "message_id": payload["message_id"]}, now, message_id=payload["message_id"])
        return {"classification": "admitted", "inbox_id": cursor.lastrowid, "message_id": payload["message_id"]}

    def _open_inbox(self):
        row = self.db.execute("SELECT value FROM metadata WHERE key='open_inbox'").fetchone()
        return row is not None and row["value"] == "1"

    def _ingest_plain(self, event, text, now):
        """Ordinary LXMF text: admitted only from a contact, or from anyone while the inbox is open."""
        source, key = event["source"], event["source_key"]
        peer = self._peer(source)
        if peer is None:
            if not self._open_inbox():
                return self._quarantine(event, "sender is not an allowed contact", now)
            if self._count("peers") >= MAX_PEERS:
                return self._quarantine(event, "contact capacity reached", now)
        elif not peer["plain"] or not self._peer_allowed(source, key, admitted=True):
            return self._quarantine(event, "sender is not an allowed contact", now)
        attachments = self._received(event)
        if not text.strip() and not attachments:
            return {"classification": "ignored_empty"}
        title = event.get("title", "")
        try:
            valid_title = isinstance(title, str) and len(title.encode("utf-8")) <= 1024
        except UnicodeEncodeError:
            valid_title = False
        if not valid_title:
            return self._quarantine(event, "invalid title", now)
        if self._count("inbox") >= MAX_INBOX:
            return self._quarantine(event, "inbox capacity reached", now)
        self.db.execute("""INSERT INTO peers(address,public_key,admitted,ever_admitted,plain,created_at,updated_at)
            VALUES (?,?,1,1,1,?,?) ON CONFLICT(address) DO UPDATE SET public_key=excluded.public_key""",
            (source, key, now, now))
        cursor = self.db.execute("INSERT INTO inbox(native_id,source,title,text,received_at,attachments,thread) VALUES (?,?,?,?,?,?,?)",
                                 (event["message_id"], source, title, text, now, _json(attachments) if attachments else None,
                                  _thread(event.get("thread", ""))))
        return {"classification": "admitted_plain", "inbox_id": cursor.lastrowid}

    def allow(self, address, name=None, reinstate=False):
        """Let one address exchange ordinary LXMF text with this profile. `reinstate` lifts an earlier revocation."""
        if not _hex(address, 32) or address == self.own_address:
            raise ValueError("invalid contact address")
        name = self._name(name)
        with self._transaction():
            now = time.time()
            peer = self._peer(address)
            if peer is not None and peer["revoked"] and not reinstate:
                raise ValueError("this address is revoked; reauthorize it first")
            if peer is not None and peer["admitted"] and not peer["plain"]:
                raise ValueError("this address is already paired as an agent")
            if peer is None and self._count("peers") >= MAX_PEERS:
                raise ValueError("peer capacity reached")
            self.db.execute("""INSERT INTO peers(address,name,admitted,ever_admitted,plain,created_at,updated_at)
                VALUES (?,?,1,1,1,?,?) ON CONFLICT(address) DO UPDATE SET name=COALESCE(excluded.name,peers.name),
                admitted=1, ever_admitted=1, plain=1, revoked=0, updated_at=excluded.updated_at""", (address, name, now, now))
            return self._peer_public(self._peer(address))

    def set_open_inbox(self, enabled):
        with self._transaction():
            self.db.execute("INSERT INTO metadata VALUES ('open_inbox', ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                            ("1" if enabled else "0",))
            return {"open_inbox": bool(enabled)}

    def _received(self, event):
        """The native helper's saved attachments, kept only when they are files it wrote under files/incoming."""
        found = event.get("attachments")
        if not isinstance(found, list) or not found:
            return []
        root = (self.files / "incoming").resolve()
        kept = []
        for item in found[:3]:
            if not isinstance(item, dict) or item.get("kind") not in ("image", "file", "audio") or not isinstance(item.get("path"), str):
                continue
            path = Path(item["path"]).resolve()
            if path.parent.parent != root or not path.is_file() or path.is_symlink():
                continue
            entry = {"kind": item["kind"], "name": str(item.get("name") or path.name)[:128], "size": path.stat().st_size,
                     "format": item.get("format") if isinstance(item.get("format"), str) else None, "path": str(path)}
            # The helper's WAV copy of a voice note, for transcription.
            wav = Path(item["wav"]).resolve() if item["kind"] == "audio" and isinstance(item.get("wav"), str) else None
            if wav and wav.parent == path.parent and wav.suffix == ".wav" and wav.is_file() and not Path(item["wav"]).is_symlink():
                entry["wav"] = str(wav)
            kept.append(entry)
        return kept

    def discard_received(self, event):
        """Remove the files of a message that was not admitted."""
        message_id = event.get("message_id") if isinstance(event, dict) else None
        if _hex(message_id, 64):
            with self._lock:
                # A retransmission can refer to the same files as an already admitted message.
                kept = self.db.execute("SELECT 1 FROM inbox WHERE native_id=? AND attachments IS NOT NULL", (message_id,)).fetchone()
                if kept is None:
                    shutil.rmtree(self.files / "incoming" / message_id, ignore_errors=True)

    def prune_received(self, limit=MAX_RECEIVED_BYTES):
        """Keep received files under the limit by removing the oldest messages' folders."""
        folders = [f for f in (self.files / "incoming").glob("*") if f.is_dir() and _hex(f.name, 64)]
        sizes = {f: sum(p.stat().st_size for p in f.iterdir() if p.is_file()) for f in folders}
        total = sum(sizes.values())
        for folder in sorted(folders, key=lambda f: f.stat().st_mtime):
            if total <= limit:
                break
            shutil.rmtree(folder, ignore_errors=True)
            total -= sizes[folder]

    def _outgoing(self, attachments):
        """Files queued with a reply. Each must already sit in files/outbox; it is removed once the job ends."""
        if attachments is None:
            return None
        if not isinstance(attachments, list) or len(attachments) > 1:
            raise ValueError("send at most one attachment per message")
        outbox = (self.files / "outbox").resolve()
        checked = []
        for item in attachments:
            if not isinstance(item, dict) or item.get("kind") not in ("file", "image") or not isinstance(item.get("path"), str):
                raise ValueError("invalid attachment")
            path = Path(item["path"])
            if path.is_symlink() or path.resolve().parent != outbox or not path.is_file():
                raise ValueError("attachments must be files in the outbox")
            if path.stat().st_size > MAX_ATTACHMENT_BYTES:
                raise ValueError("attachment exceeds 900 KB")
            name = item.get("name") or path.name
            if not isinstance(name, str) or not name.strip() or len(name.encode("utf-8")) > 128:
                raise ValueError("invalid attachment name")
            fmt = item.get("format")
            checked.append({"path": str(path.resolve()), "name": name, "kind": item["kind"], **({"format": fmt} if isinstance(fmt, str) else {})})
        return checked or None

    def _drop_outgoing(self, job):
        outbox = (self.files / "outbox").resolve()
        for item in json.loads(job["attachments"] or "[]"):
            path = Path(item["path"])
            if path.resolve().parent == outbox:
                path.unlink(missing_ok=True)

    @staticmethod
    def _peer_public(row):
        return dict(row, admitted=bool(row["admitted"]), revoked=bool(row["revoked"]),
                    ever_admitted=bool(row["ever_admitted"]), plain=bool(row["plain"]))

    def _ingest_hello(self, event, payload, now):
        attempt_id = self.own_address + ":" + payload["pairing_id"]
        attempt = self.db.execute("SELECT * FROM attempts WHERE id=?", (attempt_id,)).fetchone()
        source, key = event["source"], event["source_key"]
        if (attempt is None or attempt["role"] != "inviter" or attempt["status"] in TERMINAL
                or not self._peer_allowed(source, key) or not hmac.compare_digest(attempt["token"], payload["pairing_token"])):
            return self._quarantine(event, "HELLO is not authorized by an active invitation", now)
        if attempt["peer_address"] is not None:
            if (source != attempt["peer_address"] or key != attempt["peer_key"] or payload["hello_id"] != attempt["hello_id"]
                    or attempt["deadline"] is None or now >= attempt["deadline"]):
                return self._quarantine(event, "consumed invitation does not match recovery", now)
            scheduled = self._schedule_ack(attempt, now)
            return {"classification": "hello_recovery", "attempt_id": attempt_id, "reply_queued": scheduled}
        if attempt["status"] != "invited" or now >= attempt["expires_at"]:
            return self._quarantine(event, "invitation is no longer active", now)
        if not self._room() or (self._peer(source) is None and self._count("peers") >= MAX_PEERS):
            return self._quarantine(event, "pairing capacity reached", now)
        self.db.execute("""UPDATE attempts SET peer_address=?,peer_key=?,hello_id=?,work_started_at=?,deadline=?,status='peer_verified'
            WHERE id=?""", (source, key, payload["hello_id"], now, now + WORK_SECONDS, attempt_id))
        self._admit(source, key, attempt["peer_name"], now)
        self._schedule_ack(self._get_attempt(attempt_id), now)
        return {"classification": "hello_verified", "attempt_id": attempt_id, "evidence": "peer verified; acknowledgement pending"}

    def _ingest_ack(self, event, payload, now):
        attempt_id = event["source"] + ":" + payload["pairing_id"]
        attempt = self.db.execute("SELECT * FROM attempts WHERE id=?", (attempt_id,)).fetchone()
        if (attempt is None or attempt["role"] != "acceptor" or attempt["status"] != "waiting_ack"
                or attempt["work_started_at"] is None or now >= attempt["deadline"]
                or attempt["peer_address"] != event["source"] or attempt["peer_key"] != event["source_key"]
                or attempt["hello_id"] != payload["hello_id"] or not self._peer_allowed(event["source"], event["source_key"])):
            return self._quarantine(event, "ACK does not match an active inbound wait", now)
        if self._peer(event["source"]) is None and self._count("peers") >= MAX_PEERS:
            return self._quarantine(event, "peer capacity reached", now)
        self._admit(event["source"], event["source_key"], attempt["peer_name"], now)
        self.db.execute("UPDATE attempts SET status='paired' WHERE id=?", (attempt_id,))
        self.db.execute("UPDATE jobs SET status='skipped',error='paired',finished_at=? WHERE attempt_id=? AND status='queued'", (now, attempt_id))
        return {"classification": "paired", "attempt_id": attempt_id, "evidence": "verified acknowledgement received"}

    def queue_message(self, address, text, title="", keep_seconds=0, *, reply_to=None, attachments=None, thread=None, reset_context=False):
        """Queue text for a peer. With keep_seconds, an unreachable peer is tried again until that long from now.
        Contacts on ordinary LXMF can also get one file or image from files/outbox."""
        if not _hex(address, 32) or not isinstance(text, str) or len(text.encode("utf-8")) > MAX_TEXT_BYTES:
            raise ValueError("message requires a full peer address and at most 16 KiB of UTF-8 text")
        if not isinstance(title, str) or len(title.encode("utf-8")) > 1024:
            raise ValueError("title exceeds 1024 UTF-8 bytes")
        if type(keep_seconds) not in (int, float) or not 0 <= keep_seconds <= MAX_KEEP_SECONDS:
            raise ValueError("keep_seconds must be between 0 and one week")
        if thread is not None:
            _thread(thread)
        if type(reset_context) is not bool or (reset_context and (not isinstance(reply_to, list) or len(reply_to) != 1)):
            raise ValueError("A context reset must reply to one message")
        attachments = self._outgoing(attachments)
        with self._transaction():
            if not self._peer_allowed(address, admitted=True):
                raise ValueError("destination is not an admitted peer")
            if reply_to is not None:
                if (not isinstance(reply_to, list) or not 1 <= len(reply_to) <= 100
                        or any(type(i) is not int or i <= 0 for i in reply_to) or len(set(reply_to)) != len(reply_to)):
                    raise ValueError("Invalid reply message IDs")
                marks = ",".join("?" for _ in reply_to)
                rows = self.db.execute(f"SELECT unread,thread FROM inbox WHERE source=? AND suppressed=0 AND id IN ({marks})",
                                       (address, *reply_to)).fetchall()
                if len(rows) != len(reply_to):
                    raise ValueError("The incoming messages have changed")
                threads = {row["thread"] for row in rows}
                if len(threads) != 1 or (thread is not None and thread not in threads):
                    raise ValueError("Reply messages must belong to the same conversation")
                thread = threads.pop()
                if not any(row["unread"] for row in rows):
                    return {"status": "already_handled"}
                if not all(row["unread"] for row in rows):
                    raise ValueError("Some of these messages were already handled")
                # Enqueuing below commits in this same transaction. A full queue rolls back both changes.
                self.db.execute(f"UPDATE inbox SET unread=0, reply_draft=NULL WHERE id IN ({marks})", reply_to)
            message_id = secrets.token_hex(16)
            now = time.time()
            keep_until = now + keep_seconds if keep_seconds else None
            conversation = {"thread": thread or "", "context_through": reply_to[0] if reset_context else None}
            if self._peer(address)["plain"]:
                text = text.strip()
                if not text and not attachments:
                    raise ValueError("message text is empty")
                job_id = self._queue("plain", address, text, now, message_id=message_id, title=title, keep_until=keep_until,
                                     attachments=attachments, **conversation)
                return {"job_id": job_id, "message_id": message_id, "status": "queued", "plain": True}
            if attachments:
                raise ValueError("attachments go only to contacts on ordinary LXMF")
            payload = {"protocol": PROTOCOL, "version": VERSION, "type": "message", "message_id": message_id, "body": text}
            if len(_json(payload).encode("utf-8")) > MAX_TEXT_BYTES:
                raise ValueError("encoded message envelope exceeds the native 16 KiB text limit")
            job_id = self._queue("message", address, payload, now, message_id=message_id, title=title, keep_until=keep_until, **conversation)
            return {"job_id": job_id, "message_id": message_id, "status": "queued"}

    def _schedule_retries(self, now, receiver_healthy):
        if not receiver_healthy:
            return
        for attempt in self.db.execute("SELECT * FROM attempts WHERE status IN ('waiting_ack','peer_verified','ack_delivered')").fetchall():
            if self._active_job(attempt["id"]) or not self._peer_allowed(attempt["peer_address"], attempt["peer_key"]):
                continue
            if attempt["deadline"] is None or now >= attempt["deadline"] or not self._room():
                continue
            if attempt["role"] == "acceptor" and attempt["hello_count"] == 1 and now >= attempt["work_started_at"] + HELLO_RECOVERY_SECONDS:
                self._queue("hello", attempt["peer_address"], self._hello_payload(attempt), now, attempt_id=attempt["id"])
            elif attempt["role"] == "inviter" and not attempt["ack_confirmed"] and 0 < attempt["ack_count"] < 3 and now >= attempt["last_ack_at"] + ACK_RETRY_SECONDS:
                # Retries retain the exact logical ACK and its original deadline.
                self._queue("ack", attempt["peer_address"], self._ack_payload(attempt), now, attempt_id=attempt["id"])

    def next_job(self, receiver_healthy=False):
        with self._transaction():
            now = time.time()
            self._expire(now)
            if self._paused or self.db.execute("SELECT 1 FROM jobs WHERE status='sending'").fetchone():
                return None
            self._schedule_retries(now, receiver_healthy)
            for job in self.db.execute("SELECT * FROM jobs WHERE status='queued' AND available_at<=? ORDER BY id", (now,)).fetchall():
                reason = None
                if job["kind"] in ("message", "receipt", "plain"):
                    if not self._peer_allowed(job["address"], admitted=True):
                        reason = "peer is not admitted"
                else:
                    attempt = self._get_attempt(job["attempt_id"])
                    if attempt["status"] in TERMINAL or attempt["status"] == "paired" or not self._peer_allowed(job["address"], attempt["peer_key"]):
                        reason = "attempt closed or peer revoked"
                    elif job["kind"] == "hello":
                        if attempt["hello_count"] >= 2:
                            reason = "HELLO send quota depleted"
                        elif attempt["hello_count"] == 0:
                            if now >= attempt["expires_at"]:
                                reason = "invitation expired before initial HELLO"
                            else:
                                self.db.execute("UPDATE attempts SET work_started_at=?,deadline=? WHERE id=?", (now, now + WORK_SECONDS, attempt["id"]))
                        elif not receiver_healthy or now < attempt["work_started_at"] + HELLO_RECOVERY_SECONDS:
                            continue
                    elif attempt["ack_count"] >= 3:
                        reason = "ACK send quota depleted"
                    elif attempt["last_ack_at"] is not None and now < attempt["last_ack_at"] + ACK_RETRY_SECONDS:
                        continue
                if reason:
                    self.db.execute("UPDATE jobs SET status='skipped',error=?,finished_at=? WHERE id=?", (reason, now, job["id"]))
                    self._drop_outgoing(job)
                    continue
                if job["kind"] in ("hello", "ack"):
                    count_col, time_col = ("hello_count", "last_hello_at") if job["kind"] == "hello" else ("ack_count", "last_ack_at")
                    self.db.execute("UPDATE attempts SET " + count_col + "=" + count_col + "+1," + time_col + "=? WHERE id=?", (now, job["attempt_id"]))
                self.db.execute("UPDATE jobs SET status='sending',claimed_at=? WHERE id=?", (now, job["id"]))
                result = dict(job)
                result.update(status="sending", claimed_at=now, attachments=json.loads(job["attachments"] or "[]"))
                result["public_key"] = self._get_attempt(job["attempt_id"])["peer_key"] if job["attempt_id"] else self._peer(job["address"])["public_key"]
                return result
            return None

    def finish_job(self, job_id, ok, native_id=None, error=None, linked=True):
        if native_id is not None and not _hex(native_id, 64):
            raise ValueError("native_id must be the actual 64-hex transport identifier")
        with self._transaction():
            now = time.time()
            job = self.db.execute("SELECT * FROM jobs WHERE id=?", (job_id,)).fetchone()
            if job is None:
                raise ValueError("unknown outbound job")
            if job["status"] != "sending":
                # A durable receipt can arrive before the transport completion event.
                return self._job_public(job)
            if ok is not True and not linked and job["keep_until"] and now < job["keep_until"]:
                # Nothing left this computer, so the same text can go again without a duplicate.
                delay = min(RETRY_MAX_SECONDS, max(RETRY_MIN_SECONDS, (now - job["created_at"]) / 4))
                self.db.execute("UPDATE jobs SET status='queued',available_at=?,claimed_at=NULL,error=? WHERE id=?",
                                (now + delay, "peer not reachable; trying again", job_id))
                return self._job_public(self.db.execute("SELECT * FROM jobs WHERE id=?", (job_id,)).fetchone())
            status = "sent" if ok is True else "uncertain"
            self.db.execute("UPDATE jobs SET status=?,native_id=?,error=?,finished_at=? WHERE id=?",
                            (status, native_id, None if ok is True else "transport delivery unconfirmed" if linked else "peer not reachable", now, job_id))
            self._drop_outgoing(job)
            if job["kind"] == "ack" and ok is True:
                self.db.execute("""UPDATE attempts SET ack_confirmed=1,
                    status=CASE WHEN status='peer_verified' THEN 'ack_delivered' ELSE status END WHERE id=?""", (job["attempt_id"],))
            return self._job_public(self.db.execute("SELECT * FROM jobs WHERE id=?", (job_id,)).fetchone())

    def recover_jobs(self):
        with self._transaction():
            now = time.time()
            count = self.db.execute("UPDATE jobs SET status='uncertain',error='interrupted; delivery unconfirmed',finished_at=? WHERE status='sending'", (now,)).rowcount
            self._expire(now)
            return {"uncertain_jobs": count, "ordinary_automatic_retry": False}

    @staticmethod
    def _job_public(job):
        return {key: job[key] for key in ("id", "kind", "address", "message_id", "status", "created_at", "claimed_at", "finished_at", "native_id", "error")}

    def message_status(self, app_id):
        with self._lock:
            job = self.db.execute("SELECT * FROM jobs WHERE kind IN ('message','plain') AND message_id=?", (app_id,)).fetchone()
            if job is None:
                raise ValueError("unknown application message identifier")
            result = self._job_public(job)
            result["trying_until"] = _utc(job["keep_until"]) if job["status"] == "queued" and job["keep_until"] else None
            if job["kind"] == "plain":
                result["receipt_meaning"] = "delivered to the contact's client" if job["status"] == "sent" else None
            else:
                result["receipt_meaning"] = "durably received by peer; agent reading is not implied" if job["status"] == "received" else None
            return result

    def inbox(self, unread=True, limit=20, peer=None, after_id=0):
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise ValueError("inbox limit must be 1..1000")
        if peer is not None and not _hex(peer, 32):
            raise ValueError("inbox peer must be a full 32-hex address")
        if type(after_id) is not int or after_id < 0:
            raise ValueError("inbox after_id must be a nonnegative integer")
        with self._lock:
            query = """SELECT inbox.* FROM inbox JOIN peers ON peers.address=inbox.source
                WHERE inbox.suppressed=0 AND peers.admitted=1 AND peers.revoked=0 AND inbox.id>?"""
            parameters = [after_id]
            if unread:
                query += " AND inbox.unread=1"
            if peer is not None:
                query += " AND inbox.source=?"
                parameters.append(peer)
            rows = self.db.execute(query + " ORDER BY inbox.id LIMIT ?", (*parameters, limit)).fetchall()
            return [dict(row, unread=bool(row["unread"]), plain=row["message_id"] is None, authenticated=True, executable=False,
                         attachments=json.loads(row["attachments"] or "[]")) for row in rows]

    def context(self, peer, thread="", limit=20):
        """Read a model's history and session revision at the same reset boundary."""
        with self._lock:
            reset = self.db.execute("""SELECT context_through,id FROM jobs
                WHERE address=? AND thread=? AND context_through IS NOT NULL ORDER BY id DESC LIMIT 1""",
                (peer, _thread(thread))).fetchone()
            inbox_id, job_id = tuple(reset) if reset else (0, 0)
            return {"messages": self.history(peer, limit, thread=thread, after=(inbox_id, job_id)), "revision": inbox_id}

    def history(self, peer, limit=20, *, thread="", after=(0, 0)):
        """Recent text in one conversation, oldest first."""
        if not _hex(peer, 32):
            raise ValueError("history peer must be a full 32-hex address")
        if type(limit) is not int or not 1 <= limit <= 100:
            raise ValueError("history limit must be 1..100")
        _thread(thread)
        inbox_id, job_id = after
        with self._lock:
            rows = self.db.execute("""SELECT received_at AS at, 'in' AS direction, 'plain' AS kind, text, id, unread, attachments FROM inbox
                    WHERE source=? AND thread=? AND suppressed=0 AND id>?
                UNION ALL SELECT created_at, 'out', kind, text, id, 0, attachments FROM jobs
                    WHERE address=? AND thread=? AND kind IN ('plain','message') AND status NOT IN ('skipped') AND id>?
                ORDER BY at DESC, direction, id DESC LIMIT ?""", (peer, thread, inbox_id, peer, thread, job_id, limit)).fetchall()
        result, budget = [], 96 * 1024
        for row in rows:
            text = json.loads(row["text"])["body"] if row["kind"] == "message" else row["text"]
            budget -= len(text.encode("utf-8"))
            if budget < 0:
                break
            entry = {"direction": row["direction"], "at": _utc(row["at"]), "text": text}
            if row["attachments"]:
                entry["attachments"] = [{"kind": a["kind"], "name": a["name"]} for a in json.loads(row["attachments"])]
            if row["direction"] == "in":
                entry.update(id=row["id"], unread=bool(row["unread"]))
            result.append(entry)
        return result[::-1]

    def ack(self, message_ids):
        if not isinstance(message_ids, (list, tuple)) or len(message_ids) > 1000:
            raise ValueError("ack requires at most 1000 inbox identifiers")
        with self._transaction():
            changed = 0
            for value in message_ids:
                if type(value) is int:
                    cursor = self.db.execute("UPDATE inbox SET unread=0, reply_draft=NULL WHERE id=? AND unread=1", (value,))
                elif isinstance(value, str):
                    if self.db.execute("SELECT COUNT(*) FROM inbox WHERE native_id=? OR message_id=?", (value, value)).fetchone()[0] > 1:
                        raise ValueError("ambiguous application identifier; acknowledge by local inbox id or native id")
                    cursor = self.db.execute("UPDATE inbox SET unread=0, reply_draft=NULL WHERE (native_id=? OR message_id=?) AND unread=1", (value, value))
                else:
                    raise ValueError("invalid inbox identifier")
                changed += cursor.rowcount
            return {"acknowledged": changed}

    @staticmethod
    def _attempt_public(row):
        result = {key: row[key] for key in row.keys() if key != "token"}
        result["expires_at"] = _utc(row["expires_at"])
        result["hello_quota_remaining"] = max(0, 2 - row["hello_count"])
        result["ack_quota_remaining"] = max(0, 3 - row["ack_count"])
        result["ack_confirmed"] = bool(row["ack_confirmed"])
        result["inbound_wait_active"] = row["role"] == "acceptor" and row["status"] == "waiting_ack"
        if row["status"] == "paired":
            result["evidence"] = "verified acknowledgement received"
        elif row["ack_confirmed"]:
            result["evidence"] = "peer verified; acknowledgement delivery confirmed"
        elif row["role"] == "inviter" and row["peer_address"] is not None:
            result["evidence"] = "peer verified; acknowledgement delivery unconfirmed"
        else:
            result["evidence"] = "pairing not verified"
        return result

    def attempts(self, limit=100, offset=0):
        if type(limit) is not int or not 1 <= limit <= 100:
            raise ValueError("attempt limit must be 1..100")
        if type(offset) is not int or offset < 0:
            raise ValueError("attempt offset must be a nonnegative integer")
        with self._transaction():
            self._expire(time.time())
            return [self._attempt_public(row) for row in self.db.execute("SELECT * FROM attempts ORDER BY created_at,id LIMIT ? OFFSET ?", (limit, offset))]

    def attempt(self, attempt_id):
        with self._transaction():
            self._expire(time.time())
            return self._attempt_public(self._get_attempt(attempt_id))

    def cancel(self, attempt_id):
        with self._transaction():
            self._get_attempt(attempt_id)
            self.db.execute("UPDATE attempts SET status='cancelled' WHERE id=?", (attempt_id,))
            self.db.execute("UPDATE jobs SET status='skipped',error='attempt cancelled',finished_at=? WHERE attempt_id=? AND status='queued'", (time.time(), attempt_id))
            return self._attempt_public(self._get_attempt(attempt_id))

    def peers(self):
        with self._lock:
            return [self._peer_public(row) for row in self.db.execute("SELECT * FROM peers ORDER BY address")]

    def revoke(self, address):
        if not _hex(address, 32) or address == self.own_address:
            raise ValueError("invalid peer address")
        with self._transaction():
            now = time.time()
            if self._peer(address) is None and self._count("peers") >= MAX_PEERS:
                raise ValueError("peer capacity reached")
            self.db.execute("""INSERT INTO peers(address,revoked,created_at,updated_at) VALUES (?,1,?,?)
                ON CONFLICT(address) DO UPDATE SET revoked=1,admitted=0,updated_at=excluded.updated_at""", (address, now, now))
            self.db.execute("UPDATE attempts SET status='revoked' WHERE peer_address=?", (address,))
            self.db.execute("UPDATE inbox SET suppressed=1 WHERE source=? AND unread=1", (address,))
            self.db.execute("UPDATE jobs SET status='skipped',error='peer revoked',finished_at=? WHERE address=? AND status='queued'", (now, address))
            return dict(self._peer(address))

    def reauthorize(self, address):
        with self._transaction():
            peer = self._peer(address)
            if peer is None:
                raise ValueError("unknown peer; pair using a fresh invitation")
            self.db.execute("UPDATE peers SET revoked=0,admitted=ever_admitted,updated_at=? WHERE address=?", (time.time(), address))
            return dict(self._peer(address))

    def status(self):
        with self._transaction():
            self._expire(time.time())
            return {"address": self.own_address, "label": self.label, "paused": self._paused, "open_inbox": self._open_inbox(),
                    "admitted_peers": self.db.execute("SELECT COUNT(*) FROM peers WHERE admitted=1 AND revoked=0").fetchone()[0],
                    "unread": self.db.execute("""SELECT COUNT(*) FROM inbox JOIN peers ON peers.address=inbox.source
                        WHERE inbox.unread=1 AND inbox.suppressed=0 AND peers.admitted=1 AND peers.revoked=0""").fetchone()[0],
                    "quarantine": self._count("quarantine"), "imported": self._count("imported"),
                    "outbox": {row["status"]: row["n"] for row in self.db.execute("SELECT status,COUNT(*) AS n FROM jobs GROUP BY status")},
                    "ordinary_automatic_retry": False, "executes_messages": False,
                    "quarantine_metadata": [dict(row) for row in self.db.execute("SELECT id,native_id,source,received_at,reason FROM quarantine ORDER BY id DESC LIMIT 20")]}
