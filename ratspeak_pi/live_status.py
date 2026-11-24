"""Encrypt a read-only LAN snapshot for the computer that prepared this card."""
import json
import os
import re

from . import dashboard

REQUEST, RESPONSE = b"ratspeak-status-request-v1", b"ratspeak-status-response-v1"


def challenge(key, query):
    """The 16 bytes the computer sealed with the card's monitor key, or None for any other request."""
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    from cryptography.exceptions import InvalidTag
    if not re.fullmatch("[0-9a-f]{88}", query) or not re.fullmatch("[0-9a-f]{64}", str(key)):
        return None
    packet = bytes.fromhex(query)
    try:
        return AESGCM(bytes.fromhex(key)).decrypt(packet[:12], packet[12:], REQUEST)
    except InvalidTag:
        return None


def seal(key, payload):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    iv = os.urandom(12)
    return (iv + AESGCM(bytes.fromhex(key)).encrypt(iv, json.dumps(payload).encode(), RESPONSE)).hex().encode()


def downloading(libraries):
    """Extras still on their way, from the dashboard's per-library state."""
    from . import translate, wiki
    labels = {"voice": "Voice messages", **{k: v["label"] for k, v in wiki.RESOURCES.items()},
              **{k: v["label"] for k, v in translate.LANGUAGES.items()}}
    return [{"id": name, "label": labels.get(name, name), "percent": entry.get("percent")}
            for name, entry in libraries.items() if entry.get("state") in ("downloading", "waiting")]


def respond(state, query):
    if not re.fullmatch("[0-9a-f]{88}", query):
        return None
    cfg = json.loads((state / "config.json").read_text())
    secret = cfg.get("monitor_key", "")
    found = challenge(secret, query)
    if found is None:
        return None
    current = dashboard.snapshot(state)
    payload = {k: current[k] for k in ("name", "provider", "model", "model_name", "stats", "sampled_at", "link")}
    payload.update(challenge=found.hex(), claimed=bool(cfg["access"]["owners"]) and cfg["access"]["mode"] != "later",
                   phase="ready", downloading=downloading(current["libraries"]))
    return seal(secret, payload)
