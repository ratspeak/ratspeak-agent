"""Local management operations shared by Setup and the answering process."""
import fcntl
import json
import re
import sys
from contextlib import contextmanager
from pathlib import Path


@contextmanager
def access_lock(state):
    with (state / ".access.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def change(state, request, rpc, save):
    with access_lock(state):
        cfg = json.loads((state / "config.json").read_text())
        if cfg.get("link") != request.get("link") or cfg["access"] != request.get("expected"):
            raise ValueError("Access changed. Check the current settings and try again.")
        if cfg.get("provider") == "hermes":
            raise ValueError("This Hermes connection is owner-only.")
        access = cfg["access"]
        action = request["action"]
        if access["mode"] == "later" or not access["owners"]:
            raise ValueError("Claim your agent in Ratspeak first.")
        if action == "add":
            address = request.get("address", "")
            if not re.fullmatch("[0-9a-f]{32}", address) or address == cfg.get("address"):
                raise ValueError("Enter a different 32-character Ratspeak address.")
            if address not in access["owners"]:
                rpc("allow", address=address, name=None, reinstate=True)
                access["owners"].append(address)
        elif action == "remove":
            address = request.get("address", "")
            if address not in access["owners"][1:]:
                raise ValueError("That user no longer has access.")
            access["owners"].remove(address)
            save(cfg)
            rpc("revoke", address=address)   # after saving, so the receiver's refreshed sender list leaves it out
            return
        elif action == "private" or (action == "public" and request.get("confirmed") is True):
            rpc("open_inbox", enabled=action == "public")
            access["mode"] = "anyone" if action == "public" else "owners"
        else:
            raise ValueError("Confirm public access before enabling it.")
        save(cfg)


if __name__ == "__main__":
    from ratspeak_pi import bridge, units
    import agent_rat
    change(units.STATE, json.load(sys.stdin), lambda op, **v: agent_rat.rpc(units.PROFILE, op, **v), bridge.save)
