"""Six-character, one-use ownership codes. Both ends use the same one-minute UTC counter."""
import hashlib
import hmac
import time

PERIOD = 60
ALPHABET = "123456789ABCDEFGHJKMNPQRSTUVWXYZ"   # 32 symbols, 30 bits per code: no I, L, O, or 0
TRIES, BLOCK = 3, 600


def code(secret, timestamp):
    counter = int(timestamp) // PERIOD
    digest = hmac.digest(bytes.fromhex(secret), counter.to_bytes(8, "big"), hashlib.sha256)
    offset = digest[-1] & 15
    number = int.from_bytes(digest[offset:offset + 4], "big") & 0x3fffffff
    return "".join(ALPHABET[number >> shift & 31] for shift in range(25, -1, -5))


def current(cfg):
    secret = cfg.get("claim_secret")
    if cfg["access"]["mode"] != "later" or not secret:
        return {}
    now = int(time.time())
    expires = now - now % PERIOD + PERIOD
    return {"claim_code": code(secret, now), "next_claim_code": code(secret, expires), "claim_expires": expires}


def accepts(cfg, supplied, now):
    secret = cfg.get("claim_secret")
    supplied = "".join(supplied.split()).upper()
    if not secret or len(supplied) != 6 or not set(supplied) <= set(ALPHABET):
        return False
    if hmac.compare_digest(code(secret, now), supplied):
        return True
    # Give a message already on the mesh ten seconds to cross a code boundary.
    return now % PERIOD < 10 and hmac.compare_digest(code(secret, now - PERIOD), supplied)


def attempts(cfg):
    """Wrong-code records by sender. Older configs kept one shared record; it no longer applies."""
    return {peer: entry for peer, entry in (cfg.get("claim_attempts") or {}).items() if isinstance(entry, dict)}


def blocked_until(cfg, peer):
    return attempts(cfg).get(peer, {}).get("until", 0)


def missed(cfg, peer, now):
    """Record a wrong code. Three block the sender for ten minutes; after that, each wrong code blocks it again.
    Returns when the block ends, or 0."""
    kept = {p: e for p, e in attempts(cfg).items() if e.get("at", 0) > now - 3600 or e.get("until", 0) > now}
    entry = kept.pop(peer, {"misses": 0})
    entry.update(misses=entry.get("misses", 0) + 1, at=now)
    entry["until"] = now + BLOCK if entry["misses"] >= TRIES else 0
    kept[peer] = entry
    cfg["claim_attempts"] = dict(list(kept.items())[-256:])
    return entry["until"]
