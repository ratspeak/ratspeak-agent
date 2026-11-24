"""Read-only agent activity. Also runnable inside older desktop containers by Setup."""
import json
import sqlite3
import time
from contextlib import closing
from pathlib import Path

STATE = Path("/var/lib/ratspeak-pi")


def usage(state, counts, now=None):
    if not isinstance(counts, dict):
        return
    total = counts.get("total_tokens")
    if type(total) is not int:
        parts = [counts.get(k) for k in ("prompt_tokens", "completion_tokens")]
        if not all(type(n) is int and n >= 0 for n in parts):
            return
        total = sum(parts)
    if not 0 <= total < 2**40:
        return
    now = int(time.time() if now is None else now)
    with closing(sqlite3.connect(state / "usage.sqlite3")) as db, db:
        db.execute("CREATE TABLE IF NOT EXISTS usage (id INTEGER PRIMARY KEY, tokens INTEGER NOT NULL)")
        db.execute("CREATE TABLE IF NOT EXISTS usage_seconds (at INTEGER PRIMARY KEY, tokens INTEGER NOT NULL)")
        db.execute("DELETE FROM usage_seconds WHERE at <= ?", (now - 86400,))
        db.execute("INSERT INTO usage_seconds VALUES (?,?) ON CONFLICT(at) DO UPDATE SET tokens=tokens+excluded.tokens", (now, total))
        db.execute("INSERT INTO usage VALUES (1,?) ON CONFLICT(id) DO UPDATE SET tokens=tokens+excluded.tokens", (total,))


def device_memory(path=Path("/proc/meminfo")):
    try:
        values = {line.split(":")[0]: int(line.split()[1]) * 1024 for line in path.read_text().splitlines()
                  if line.startswith(("MemTotal:", "MemAvailable:"))}
        total, available = values["MemTotal"], values["MemAvailable"]
        if 0 <= available <= total and total:
            return {"used": total - available, "total": total}
    except (OSError, KeyError, ValueError, IndexError):
        pass
    return None


def model_resident_bytes(command, proc=Path("/proc")):
    """Resident CPU memory of this agent's configured llama-server, including mapped weights."""
    if not command or not isinstance(command, list) or not all(isinstance(arg, str) for arg in command):
        return None
    if Path(command[0]).name != "llama-server":
        return None
    argv = b"\0".join(arg.encode() for arg in command) + b"\0"
    for process in proc.glob("[0-9]*"):
        try:
            if (process / "comm").read_bytes().strip() != b"llama-server" or (process / "cmdline").read_bytes() != argv:
                continue
            for line in (process / "status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1024
        except (OSError, ValueError, IndexError):
            continue
    return None


def snapshot(state=STATE, container=False):
    cfg = json.loads((state / "config.json").read_text())
    model = cfg.get("model")
    if cfg.get("provider") == "venice":
        try:
            model = (state / "venice-model").read_text().strip() or model
        except OSError:
            pass
    now = int(time.time())
    stats = {"received": None, "sent": None, "tokens": None, "tokens_24h": None, "device_memory": None}
    try:
        with closing(sqlite3.connect((state / "profile/state.sqlite3").as_uri()+"?mode=ro", uri=True, timeout=.5)) as db:
            stats["received"] = db.execute("SELECT COUNT(*) FROM inbox WHERE suppressed=0").fetchone()[0]
            stats["sent"] = db.execute("""
                SELECT COUNT(*) FROM jobs
                WHERE kind IN ('plain','message') AND status IN ('sent','received')""").fetchone()[0]
    except sqlite3.Error:
        pass
    try:
        with closing(sqlite3.connect((state / "contacts.sqlite3").as_uri()+"?mode=ro", uri=True, timeout=.5)) as db:
            received, sent = db.execute("""SELECT
                COUNT(CASE WHEN side='in' OR stage='returned' THEN 1 END),
                COUNT(CASE WHEN side='in' AND stage='answered' THEN 1 END)
                FROM friend_mail""").fetchone()
            if stats["received"] is not None:
                stats["received"] += received
            if stats["sent"] is not None:
                stats["sent"] += sent
    except sqlite3.Error:
        pass
    try:
        with closing(sqlite3.connect((state / "usage.sqlite3").as_uri()+"?mode=ro", uri=True, timeout=.5)) as db:
            stats["tokens"] = db.execute("SELECT tokens FROM usage WHERE id=1").fetchone()[0]
            stats["tokens_24h"] = db.execute("SELECT COALESCE(SUM(tokens), 0) FROM usage_seconds WHERE at > ? AND at <= ?", (now - 86400, now)).fetchone()[0]
    except (sqlite3.Error, TypeError):
        pass
    if not container and cfg.get("provider") != "venice":
        stats["device_memory"] = device_memory()
    if cfg.get("provider", "local") == "local":
        stats["model_resident_bytes"] = model_resident_bytes(cfg.get("command"))
    pairing = {}
    if cfg.get("claim_secret") and cfg["access"]["mode"] == "later":
        from ratspeak_pi.claim import current
        pairing = current(cfg)
    libraries = {}
    if cfg.get("provider", "local") == "local":
        from ratspeak_pi import wiki
        try:
            status = json.loads((state / "resources" / "status.json").read_text())
        except (OSError, ValueError):
            status = {}
        features = cfg.get("features", {})

        def listed(name, default=False):
            # A Pi's config lists what is installed; its installer records the extras it is adding.
            return name in status if not container else features.get(name, default)
        for name, library in wiki.RESOURCES.items():
            if not listed(name):
                continue
            entry = dict(status.get(name) or {"state": "waiting"})
            part = state / "resources" / (library["file"] + ".part")
            if entry["state"] == "downloading" and part.exists():
                entry["percent"] = min(99, part.stat().st_size * 100 // library["download_bytes"])
            libraries[name] = entry
        from ratspeak_pi import translate, voice
        if listed("voice", True):
            entry = dict(status.get("voice") or {"state": "waiting"})
            if entry["state"] == "downloading":
                files = voice.ENGINES[entry.get("engine") or voice.choose(True, 0)]["files"]
                done = sum(f["bytes"] if (state / "voice" / f["file"]).is_file() else
                           (state / "voice" / (f["file"] + ".part")).stat().st_size if (state / "voice" / (f["file"] + ".part")).exists() else 0
                           for f in files)
                entry["percent"] = min(99, done * 100 // sum(f["bytes"] for f in files))
            libraries["voice"] = entry
        for name, language in translate.LANGUAGES.items():
            if not listed(name):
                continue
            entry = dict(status.get(name) or {"state": "waiting"})
            if entry["state"] == "downloading":
                done = 0
                for pack in language["packs"]:
                    home = state / "translate" / pack["file"].removesuffix(".argosmodel")
                    part = state / "translate" / (pack["file"] + ".part")
                    done += pack["bytes"] if (home / "model/model.bin").is_file() else part.stat().st_size if part.exists() else 0
                entry["percent"] = min(99, done * 100 // sum(p["bytes"] for p in language["packs"]))
            libraries[name] = entry
    return {"name": cfg.get("name"), "provider": cfg.get("provider", "local"), "model": model,
            "features": cfg.get("features", {}), "libraries": libraries,
            "model_name": model if cfg.get("provider") == "venice" else cfg.get("model_name", model),
            "access": cfg["access"], "link": cfg.get("link"), **pairing,
            "stats": stats, "sampled_at": now}


if __name__ == "__main__":
    result = snapshot(container=True)
    result["state"] = "running" if Path("/tmp/ratspeak-agent-ready").exists() else "starting"
    print(json.dumps(result))
