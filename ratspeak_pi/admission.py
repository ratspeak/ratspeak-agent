"""Limit authenticated work before it enters the durable agent inbox."""
from __future__ import annotations

import json
import time
from pathlib import Path

from policy import (MAX_IMPORTED, MAX_INBOX, MAX_JOBS, MAX_PEERS, PROTOCOL, Policy,
                    _hex, _no_duplicate_keys)

from . import claim
from .limits import CLAIM, OWNER_SLOTS, PENDING_PER_SENDER, SHARED_SLOTS, PublicPacing, moderation

BUSY = "I'm busy right now. Please try again in a few minutes."
WAIT = "I'm still answering your earlier messages. Please resend this one after my reply."
FULL = "My message storage is full. The owner needs to free up space."


class AgentInbox(Policy):
    def __init__(self, db_path, own_address, own_key, label):
        super().__init__(db_path, own_address, own_key, label)
        self.config = Path(db_path).parent.parent / "config.json"
        self.config_version = None
        self.access = ("owners", frozenset(), None, {})
        self.pacing = PublicPacing()
        self.notice = None
        self.next_notice = 0
        self.notified = {}
        self.db.executescript("""
            CREATE INDEX IF NOT EXISTS agent_unread ON inbox(source) WHERE unread=1 AND suppressed=0;
            CREATE INDEX IF NOT EXISTS agent_arrival ON inbox(source,received_at);
            CREATE INDEX IF NOT EXISTS agent_imported ON imported(native_id) WHERE classification!='quarantine';
            CREATE INDEX IF NOT EXISTS agent_replies ON jobs(address)
                WHERE kind IN ('plain','message') AND status IN ('queued','sending');
        """)

    def settings(self):
        # Setup and owner commands replace this file atomically, including on existing installations.
        with self._lock:
            try:
                stat = self.config.stat()
                version = (stat.st_ino, stat.st_mtime_ns, stat.st_size)
                if version != self.config_version:
                    cfg = json.loads(self.config.read_text())
                    access = cfg["access"]
                    mode, owners = access["mode"], access["owners"]
                    if mode not in ("owners", "anyone", "later") or not isinstance(owners, list) or not all(_hex(p, 32) for p in owners):
                        raise ValueError("invalid access settings")
                    if cfg.get("provider") == "hermes" and not (mode == "later" and not owners or mode == "owners" and len(owners) == 1):
                        raise ValueError("Hermes requires a single paired owner")
                    blocked = {peer: float(entry.get("until", 0)) for peer, entry in claim.attempts(cfg).items()}
                    self.access = (mode, frozenset(owners), next(iter(owners), None), blocked)
                    self.config_version = version
            except (OSError, ValueError, TypeError, KeyError, AttributeError):
                self.config_version = None
                self.access = ("owners", frozenset(), None, {})
            return self.access

    def native_access(self):
        mode, owners, _, _ = self.settings()
        return mode, sorted(owners)

    def _busy(self, event, text, now):
        if self.notice is None and now >= self.next_notice and self.notified.get(event["source"], 0) <= now:
            self.notice = (event["source"], event["source_key"], text, event.get("thread", ""), now + 10)
            self.next_notice = now + 30
            self.notified = {p: end for p, end in self.notified.items() if end > now}
            self.notified[event["source"]] = now + 180
        return {"classification": "busy"}

    def take_notice(self):
        with self._lock:
            notice, self.notice = self.notice, None
            if not notice or notice[4] <= time.monotonic():
                return None
            mode, allowed, _, _ = self.settings()
            if (mode == "anyone" or (mode == "owners" and notice[0] in allowed)) and self._peer_allowed(notice[0], notice[1]):
                return notice[:4]
            return None

    def _quarantine(self, event, reason, now):
        # The appliance has no quarantine viewer. Unadmitted input needs no durable copy.
        return {"classification": "discarded"}

    def _pending(self, source):
        # Inbox acknowledgement and outbox enqueue share a transaction. Count either side,
        # including the reply being delivered, so a receiver restart cannot free occupied slots.
        rows = self.db.execute("""
            SELECT source FROM inbox WHERE unread=1 AND suppressed=0
            UNION ALL SELECT address FROM jobs WHERE kind IN ('plain','message') AND status IN ('queued','sending')
            LIMIT ?
        """, (SHARED_SLOTS + OWNER_SLOTS,))
        total = personal = 0
        for (peer,) in rows:
            total += 1
            personal += peer == source
        return total, personal

    def _guest_slot(self, allowed):
        # Retire only idle, unnamed public LXMF contacts. Keep chat history, revocations,
        # explicit contacts and all addresses involved in pending deliveries or pairing.
        for (peer,) in self.db.execute("""
            SELECT address FROM peers p WHERE plain=1 AND revoked=0 AND name IS NULL
              AND public_key IS NOT NULL AND updated_at=created_at
              AND EXISTS (SELECT 1 FROM inbox WHERE source=p.address AND received_at=p.created_at AND message_id IS NULL)
              AND NOT EXISTS (SELECT 1 FROM inbox WHERE source=p.address AND unread=1 AND suppressed=0)
              AND NOT EXISTS (SELECT 1 FROM jobs WHERE address=p.address AND status IN ('queued','sending'))
              AND NOT EXISTS (SELECT 1 FROM attempts WHERE peer_address=p.address)
            ORDER BY updated_at
        """):
            if peer not in allowed:
                self.db.execute("DELETE FROM peers WHERE address=?", (peer,))
                return True
        return False

    def ingest(self, event):
        ignored = {"classification": "discarded"}
        if not isinstance(event, dict) or event.get("event") != "message" or event.get("verified") is not True or event.get("error"):
            return ignored
        source, mid, key = event.get("source"), event.get("message_id"), event.get("source_key")
        if (not _hex(source, 32) or not _hex(key, 128) or not _hex(mid, 64)
                or event.get("destination") != self.own_address or source == self.own_address):
            return ignored
        text = event.get("text")
        if not isinstance(text, str) or not (text.strip() or event.get("attachments")):
            return ignored

        with self._lock:
            mode, allowed, owner, blocked = self.settings()
            if mode == "owners" and source not in allowed:
                return ignored
            if mode == "later" and (blocked.get(source, 0) > time.time() or not CLAIM.fullmatch(text.strip())):
                return ignored
            if not self._peer_allowed(source, key):
                return ignored
            if self.db.execute("SELECT 1 FROM imported WHERE native_id=?", (mid,)).fetchone():
                return {"classification": "duplicate"}

            # Pairing and receipt envelopes do not invoke the model. The helper checks
            # their invitation, job and key bindings. Chat envelopes share the normal budget.
            control = False
            payload = None
            if text.lstrip().startswith("{"):
                try:
                    payload = json.loads(text, object_pairs_hook=_no_duplicate_keys)
                except (ValueError, RecursionError):
                    payload = None
                if isinstance(payload, dict) and payload.get("protocol") == PROTOCOL:
                    control = payload.get("type") in ("hello", "ack", "receipt")

            now = time.monotonic()
            public = mode == "anyone" and source != owner
            if not control and not (source == owner and moderation(text)):
                total, personal = self._pending(source)
                # Trusted private conversations can arrive in several pieces after reconnecting.
                waiting = mode != "owners" and personal >= PENDING_PER_SENDER
                if waiting or total >= SHARED_SLOTS + (OWNER_SLOTS if source == owner else 0):
                    return ignored if mode == "later" else self._busy(event, WAIT if waiting else BUSY, now)
            if public and not self.pacing.available(source, now):
                return self._busy(event, BUSY, now) if not control else ignored

            reserve = 16 if public else 0
            imports_full = (self._count("imported") >= MAX_IMPORTED - reserve and
                            self.db.execute("SELECT COUNT(*) FROM imported WHERE classification!='quarantine'").fetchone()[0]
                            >= MAX_IMPORTED - reserve)
            if (imports_full or
                    (not control and (self._count("inbox") >= MAX_INBOX - reserve or
                                      self._count("jobs") >= MAX_JOBS - reserve))):
                return self._busy(event, FULL, now) if public and not control else ignored

            peer = self._peer(source)
            with self._transaction():
                if not control and peer is None and self._count("peers") >= MAX_PEERS - 1:
                    if not self._open_inbox() or (isinstance(payload, dict) and payload.get("protocol") == PROTOCOL):
                        return ignored
                    if not self._guest_slot(allowed) and source != owner:
                        return self._busy(event, FULL, now) if public else ignored
                wall = time.time()
                result = self._ingest_authenticated(event, wall)
                if result["classification"] in ("discarded", "ignored_empty", "duplicate_message", "duplicate_receipt"):
                    return ignored
                self.db.execute("INSERT INTO imported VALUES (?,?,?)", (mid, wall, result["classification"]))
            if public:
                self.pacing.spend(source, now)
            if result["classification"] == "admitted_plain":
                result["keys_changed"] = peer is None or peer["public_key"] is None
            return result
