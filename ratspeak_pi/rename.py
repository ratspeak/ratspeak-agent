"""Change an agent's announced label while retaining its LXMF identity."""
import fcntl
import json
import re
import sys
from pathlib import Path


def rename(state, link, name):
    from common import atomic_json, atomic_text
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 ._-]{0,31}", name):
        raise ValueError("Invalid agent name")
    with (state / ".access.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        config = state / "config.json"
        cfg = json.loads(config.read_text())
        manifest = state / "profile" / "manifest.json"
        profile = json.loads(manifest.read_text())
        if cfg["link"] != link or f"lxma://{profile['address']}:{profile['public_key']}" != link:
            raise ValueError("Agent identity changed")
        prompt = state / "system.txt"
        if prompt.is_file():
            text = prompt.read_text()
            old = "You are " + cfg["name"] + ","
            if text.startswith(old):
                atomic_text(prompt, "You are " + name + "," + text[len(old):])
        profile["label"] = name
        atomic_json(manifest, profile)
        cfg["name"] = name
        atomic_json(config, cfg)


def announce_saved_name(profile, link, name):
    from agent_rat import rpc, setup_lock, start, stop
    try:
        result = rpc(profile, "announce_saved_name")
    except RuntimeError as error:
        if str(error) != "Unknown local operation":
            raise
        # Older transports only read the label at launch. Reconnect their mesh
        # receiver under its normal controller lock, keeping inference running.
        with setup_lock(profile):
            card = rpc(profile, "status")["card"]
            if f"lxma://{card['address']}:{card['public_key']}" != link:
                raise ValueError("Agent identity changed")
            if card["name"] != name:
                stop(profile)
                card = start(profile)["card"]
                if card["name"] != name:
                    raise RuntimeError("Receiver did not load the saved name")
            rpc(profile, "announce")
        return
    if result.get("label") != name:
        raise RuntimeError("Receiver did not announce the saved name")


if __name__ == "__main__":
    sys.path.insert(0, "/opt/ratspeak-pi/runtime")
    state = Path("/var/lib/ratspeak-pi")
    link, name = sys.argv[1:3]
    if sys.argv[3:] == ["--announce"]:
        announce_saved_name(state / "profile", link, name)
    else:
        rename(state, link, name)
