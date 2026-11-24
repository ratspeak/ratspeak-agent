"""Install progress for the computer that prepared this card, sealed like the setup page's live status.

firstboot.sh starts this from the unpacked release, before it waits for the network, and setup stops it just before
the setup page takes the same port. It reads status.json on the card for every request.
"""
from __future__ import annotations

import json
import re
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from . import hardware, ladder, live_status, units

BOOT = Path("/boot/firmware/ratspeak-pi")
UNIT = "ratspeak-pi-install-status.service"
# Install facts kept in status.json while installing; a ready Pi drops them.
KEYS = ("stage", "stage_index", "stages", "done", "total", "percent", "elapsed", "started", "model_name")
# Rough seconds per stage on a Pi 4 or 5, plus bytes at each stage's rate: downloads (and their check) at 5 MB/s,
# card copies (read, write, re-read to check) at 15 MB/s, and the speed test loading and running the model at 30 MB/s.
SECONDS = {"unpack": 3, "network": 15, "packages": 60, "llama": 5, "model": 0, "venice": 5, "speed": 10, "identity": 15,
           "starting": 8}
CARD_PACKAGES = 15   # dpkg from the card; no apt-get update
DOWNLOAD, CARD, LOAD = 5e6, 15e6, 30e6


def read(path: Path) -> dict:
    try:
        value = json.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def stages(local: bool, from_card: bool) -> list[str]:
    """This install's stages, in order. firstboot waits for the network unless a local model comes on the card."""
    return ["unpack", *([] if local and from_card else ["network"]), "packages",
            *(["llama", "model", "speed"] if local else ["venice"]), "identity", "starting"]


def model_bytes(status: dict, settings: dict) -> int:
    """The chosen model's size once setup names it; before that, the one setup will pick for this board."""
    named = {rung["name"]: rung for rung in ladder.RUNGS}.get(status.get("model_name"))
    if named:
        return named["download_bytes"]
    if settings.get("model") in ladder.BY_ID:
        return ladder.RUNGS[ladder.BY_ID[settings["model"]]]["download_bytes"]
    board = hardware.detect()
    try:
        return ladder.RUNGS[ladder.pick(board.ram_mb, board.generation, settings.get("users") or "1",
                                        bool(settings.get("smarter")))]["download_bytes"]
    except (KeyError, ValueError):
        return 0


def uptime() -> float | None:
    """Seconds since boot: the wall clock jumps when the network sets it."""
    try:
        return float(Path("/proc/uptime").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return None


def fields(status: dict, settings: dict) -> dict:
    """stage_index, stages, percent and elapsed for a status.json written during install."""
    local = settings.get("provider", "local") != "venice"
    from_card = status.get("from_card") is True
    path = stages(local, from_card)
    size = model_bytes(status, settings) if local else 0
    rate = CARD if from_card else DOWNLOAD
    sized = {"llama": ladder.LADDER["llama_cpp"]["size"] / rate, "model": size / rate, "speed": size / LOAD}
    fixed = {**SECONDS, "packages": CARD_PACKAGES if from_card else SECONDS["packages"]}
    weight = {name: fixed[name] + sized.get(name, 0) for name in path}
    stage, done, total = status.get("stage"), status.get("done"), status.get("total")
    progress = 0.0
    if stage in path:
        progress = sum(weight[name] for name in path[:path.index(stage)])
        if isinstance(done, int) and isinstance(total, int) and total > 0:
            progress += sized.get(stage, 0) * min(1.0, done / total)
    stored = status.get("percent")
    now, started = uptime(), status.get("started")
    return {"stage_index": path.index(stage) + 1 if stage in path else None, "stages": len(path),
            "percent": max(min(99, int(100 * progress / sum(weight.values()))), stored if isinstance(stored, int) else 0),
            "elapsed": max(0, int(now - started)) if now is not None and isinstance(started, (int, float)) else None}


def payload(status: dict, settings: dict, challenge: bytes) -> dict:
    failed = status.get("state") == "failed"
    stage = status.get("stage")
    counted = stage in ("llama", "model")
    return {"phase": "failed" if failed else "installing", "name": str(settings.get("name") or "Pi").strip(),
            "link": settings.get("pairing_link"), "challenge": challenge.hex(), "sampled_at": int(time.time()),
            "claimed": False, "stage": stage, **fields(status, settings),
            "done": status.get("done") if counted else None, "total": status.get("total") if counted else None,
            "from_card": status.get("from_card") is True, "model_name": status.get("model_name"),
            "detail": status.get("detail") if failed else None}


def respond(boot: Path, query: str) -> bytes | None:
    settings = read(boot / "settings.json")
    key = settings.get("monitor_key", "")
    found = live_status.challenge(key, query)
    if found is None:
        return None
    return live_status.seal(key, payload(read(boot / "status.json"), settings, found))


class Handler(BaseHTTPRequestHandler):
    boot = BOOT

    def do_GET(self):
        body = None
        if self.path.startswith("/agent-status/"):
            try:
                body = respond(self.boot, self.path[len("/agent-status/"):])
            except (OSError, ValueError, KeyError, TypeError):
                body = None
        if body is None:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_other(self):
        self.send_error(404)

    do_HEAD = do_POST = do_PUT = do_DELETE = do_PATCH = do_OPTIONS = do_other

    def log_message(self, *args):
        pass


def serve(boot: Path = BOOT, port: int = units.PAGE_PORT) -> int:
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: F401
    except ImportError:
        print("No encryption library: install progress is on the card only.")
        return 0
    if not re.fullmatch("[0-9a-f]{64}", str(read(boot / "settings.json").get("monitor_key", ""))):
        print("No monitor key on the card: install progress is on the card only.")
        return 0
    ThreadingHTTPServer(("0.0.0.0", port), type("CardHandler", (Handler,), {"boot": boot})).serve_forever()
    return 0
