"""Lifecycle inside the managed Linux container. Docker owns the persistent state volume."""
import contextlib
import fcntl
import json
import os
import platform
import secrets
import shutil
import subprocess
import sys
import threading
import time
import zipfile
from pathlib import Path

from . import bridge, llm, setup, translate, units, venice, voice, wiki
from .access_control import access_lock
from common import atomic_json, atomic_text, prepare_profile
from agent_rat import network_config, running
from policy import Policy

STATE = units.STATE
MODELS = json.loads(Path(__file__).with_name("desktop_models.json").read_text())
BINARY = units.PREFIX / "bin/ratspeak-pi-native"
CONFIG = STATE / "config.json"
READY = Path("/tmp/ratspeak-agent-ready")
FAILURE = Path("/tmp/ratspeak-agent-error")


def status():
    if not CONFIG.exists():
        return None
    cfg = json.loads(CONFIG.read_text())
    if cfg.get("provider") == "venice":
        cfg["model"] = venice.selected(STATE, cfg["model"])
    return {k: cfg.get(k) for k in ("name", "provider", "model", "link")}


def progress(text: str, done: int | None = None, total: int | None = None) -> None:
    """One line of install progress for Setup, which reads this command's output."""
    print(json.dumps({"text": text, "done": done, "total": total}), flush=True)


def watched(text: str, path: Path | list[Path], total: int | None, work):
    """Run a download, reporting the files' size twice a second; then say it is being checked."""
    stop = threading.Event()
    paths = path if isinstance(path, list) else [path]

    def report():
        while not stop.wait(0.5):
            done = sum(p.stat().st_size for p in paths if p.exists())
            if total and done >= total:
                progress("Checking the download…")
            else:
                progress(text, done, total)

    reporter = threading.Thread(target=report, daemon=True)
    reporter.start()
    try:
        return work()
    finally:
        stop.set()
        reporter.join()


def need_space(model: dict, cuda: bool) -> None:
    """Refuse before downloading what Docker's storage can't hold: a full disk can leave it read-only."""
    cache = STATE / "models"
    have = (cache / model["file"]).stat().st_size >> 20 if (cache / model["file"]).exists() else 0
    need = max(0, model["size_mb"] - have) + (800 if cuda else 30) + 1024
    free = shutil.disk_usage(STATE).free >> 20
    if free < need:
        raise ValueError(f"{model['name']} needs about {-(-need // 1024)} GB free in Docker’s storage, which has "
                         f"{free // 1024} GB. Free some space or give Docker a larger disk, then retry.")


def configure(settings):
    if CONFIG.exists():
        raise ValueError("An agent is already configured on this computer.")
    mid = settings.get("model")
    is_hermes = settings.get("provider") == "hermes"
    clean = setup.validate({**settings, "model": None, "provider": "local" if is_hermes else settings.get("provider", "local")})
    if is_hermes:
        if clean["access"] != {"mode": "later", "owners": []}:
            raise ValueError("Hermes must be claimed by one owner.")
        clean["provider"] = "hermes"
    cloud = clean["provider"] == "venice"
    command = []
    if is_hermes:
        mid = name = settings["hermes"]["model"]
    elif cloud:
        mid = clean["venice_model"]
        venice.test_model(mid, settings["venice_api_key"])
        venice.store(STATE, mid, settings["venice_api_key"])
        name = mid
    else:
        model = next((m for m in MODELS if m["id"] == mid), None)
        if not model:
            raise ValueError("Choose a desktop model.")
        cuda = settings.get("gpu") is True
        need_space(model, cuda)
        if platform.machine() == "x86_64":
            old = llm.LADDER["llama_cpp"]
            pin = dict(old, url=old["url"].replace("ubuntu-arm64", "ubuntu-x64"),
                       sha256="deb1188ec217e3b4934819fbedbbbbc9623efc9cb2e00d5cd1eddac6870d2eb5")
            llm.LADDER["llama_cpp"] = pin
        elif cuda:
            raise ValueError("Graphics cards are used only on x86-64 Docker engines.")
        cache = STATE / "models"
        if cuda:
            pin = llm.LADDER["llama_cuda"]
            binary = watched("Downloading the graphics card runner",
                             [cache / Path(pin["url"]).name, cache / Path(pin["runtime"]["url"]).name],
                             pin["size"] + pin["runtime"]["size"],
                             lambda: llm.install_llama(STATE, cache, say=lambda _: None, cuda=True))
            if not llm.cuda_devices(binary):
                raise ValueError("The agent can’t reach the graphics card. Update the NVIDIA driver, then retry.")
        else:
            pin = llm.LADDER["llama_cpp"]
            binary = watched("Downloading the model runner", cache / f"llama-{pin['tag']}.tar.gz",
                             pin.get("size") if platform.machine() != "x86_64" else None,
                             lambda: llm.install_llama(STATE, cache, say=lambda _: None))
        file = watched(f"Downloading {model['name']}", cache / model["file"], model["size_mb"] * 1_000_000,
                       lambda: llm.fetch(model["url"], cache / model["file"], model["sha256"], say=lambda _: None))
        command = llm.server_command(binary, file, model["context"], min(len(os.sched_getaffinity(0)), 12), model["thinking"])
        name = model["name"]
    profile = units.PROFILE
    manifest = prepare_profile(profile, clean["name"], str(BINARY))
    interfaces, net = network_config(clean["hubs"], True)
    # Docker's bridge cannot carry multicast onto the host LAN. Publish an actual RNS TCP listener.
    net += "\n[[Desktop LAN]]\ntype = TCPServerInterface\nenabled = yes\nlisten_ip = 0.0.0.0\nlisten_port = 4243\n"
    atomic_text(profile / "reticulum/config", net)
    manifest.update(interfaces=interfaces, label=clean["name"])
    atomic_json(profile / "manifest.json", manifest)
    policy = Policy(profile / "state.sqlite3", manifest["address"], manifest["public_key"], clean["name"])
    try:
        policy.set_open_inbox(clean["access"]["mode"] != "owners")
        for owner in clean["access"]["owners"]:
            policy.allow(owner)
    finally:
        policy.close()
    cfg = dict(name=clean["name"], access=clean["access"], features=clean["features"], hubs=clean["hubs"],
               provider=clean["provider"], model=mid, model_name=name, speed=None, context=4096, command=command,
               address=manifest["address"], link=f"lxma://{manifest['address']}:{manifest['public_key']}")
    if clean["access"]["mode"] == "later":
        cfg["claim_secret"] = secrets.token_hex(32)
    atomic_text(STATE / "system.txt", llm.CHAT_PROMPT)
    atomic_json(CONFIG, cfg)


LIBRARIES = STATE / "resources" / "status.json"


def sync_libraries(stop: threading.Event, wait: float = 5) -> None:
    """Download the libraries the owner chose and remove the others, one at a time, while the agent answers.
    Setup shows the progress from the status file and the size of the part already downloaded."""
    folder, done, status = wiki.FOLDER, None, {}
    folder.mkdir(parents=True, exist_ok=True)
    while not stop.is_set():
        try:
            features = json.loads(CONFIG.read_text()).get("features", {})
        except (OSError, ValueError):
            features = {}
        wanted = {name for name in [*wiki.RESOURCES, *translate.LANGUAGES, "voice"] if features.get(name, name == "voice")}
        failed = any(entry.get("state") == "failed" for entry in status.values())
        if wanted != done or failed and time.monotonic() - status.get("_at", {}).get("t", 0) > 600:
            engine = voice.choose(True, 0)

            def present(name):
                if name == "voice":
                    return voice.installed() == engine
                if name in translate.LANGUAGES:
                    return translate.ready(name)
                return (folder / wiki.RESOURCES[name]["file"]).exists()

            # Everything already here shows as ready while the rest download one at a time.
            status = {name: {"state": "ready" if present(name) else "waiting"} for name in wanted}
            atomic_json(LIBRARIES, status)
            if "voice" not in wanted:
                voice.remove()
            elif voice.installed() == engine:
                status["voice"] = {"state": "ready"}
            else:
                status["voice"] = {"state": "downloading"}
                atomic_json(LIBRARIES, status)
                try:
                    voice.install(engine, lambda url, path, sha: llm.fetch_complete(url, path, sha, lambda _: None))
                    status["voice"] = {"state": "ready"}
                except (OSError, RuntimeError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
                    status["voice"] = {"state": "failed", "error": str(error)[:200]}
            if stop.is_set():
                return
            for name, library in wiki.RESOURCES.items():
                path = folder / library["file"]
                if name not in wanted:
                    path.unlink(missing_ok=True)
                    path.with_name(path.name + ".part").unlink(missing_ok=True)
                    continue
                if path.exists():
                    status[name] = {"state": "ready"}
                    continue
                status[name] = {"state": "downloading"}
                atomic_json(LIBRARIES, status)
                try:
                    free = shutil.disk_usage(folder).free
                    if free < library["download_bytes"] + (500 << 20):
                        raise RuntimeError(f"Docker has {free >> 20} MB free and this needs {library['size_mb'] + 500} MB")
                    llm.fetch_complete(library["url"], path, library["sha256"], lambda _: None)
                    status[name] = {"state": "ready"}
                except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
                    status[name] = {"state": "failed", "error": str(error)[:200]}
                if stop.is_set():
                    return
            for name, language in translate.LANGUAGES.items():
                if name not in wanted:
                    translate.remove(name)
                    continue
                if translate.ready(name):
                    status[name] = {"state": "ready"}
                    continue
                status[name] = {"state": "downloading"}
                atomic_json(LIBRARIES, status)
                try:
                    free = shutil.disk_usage(folder).free
                    if free < language["size_mb"] * 10**6 + (600 << 20):
                        raise RuntimeError(f"Docker has {free >> 20} MB free and this needs {language['size_mb'] + 600} MB")
                    translate.install(name, lambda url, path, sha: llm.fetch_complete(url, path, sha, lambda _: None))
                    status[name] = {"state": "ready"}
                except (OSError, RuntimeError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
                    status[name] = {"state": "failed", "error": str(error)[:200]}
                if stop.is_set():
                    return
            status["_at"] = {"t": time.monotonic()}
            atomic_json(LIBRARIES, {k: v for k, v in status.items() if not k.startswith("_")})
            done = wanted
        stop.wait(wait)


def choose_libraries(values: dict) -> None:
    """Record which libraries and tools the owner wants; the running agent downloads or removes them."""
    features = values.get("features")
    if not isinstance(features, dict):
        raise ValueError("Choose the extras.")
    with access_lock(STATE):   # the answer loop writes the same file for /setup and /allow
        cfg = json.loads(CONFIG.read_text())
        if cfg.get("provider", "local") != "local":
            raise ValueError("Extras are for agents with a local model.")
        for name in [*wiki.RESOURCES, *translate.LANGUAGES, "voice"]:
            if name in features:
                cfg.setdefault("features", {})[name] = bool(features[name])
        atomic_json(CONFIG, cfg)


def serve():
    READY.unlink(missing_ok=True)
    (STATE / "hermes.json").unlink(missing_ok=True)  # Earlier receivers kept the Hermes key here.
    FAILURE.unlink(missing_ok=True)
    while not CONFIG.exists():
        time.sleep(.25)
    cfg = json.loads(CONFIG.read_text())
    with contextlib.ExitStack() as stack:
        if cfg.get("command"):
            server = subprocess.Popen(cfg["command"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            stack.callback(server.terminate)
            deadline = time.monotonic() + 240
            while not llm.healthy():
                if server.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("The local model could not start. Check available container memory.")
                time.sleep(.5)
        loop = subprocess.Popen([sys.executable, "-m", "ratspeak_pi", "bridge"])
        if cfg.get("provider", "local") == "local":
            stop = threading.Event()
            stack.callback(stop.set)
            threading.Thread(target=sync_libraries, args=(stop,), daemon=True).start()
        stack.callback(loop.terminate)
        while loop.poll() is None and (not cfg.get("command") or server.poll() is None):
            if not READY.exists() and running(units.PROFILE):
                READY.touch()
            time.sleep(1)
        raise RuntimeError("The agent stopped unexpectedly.")


def main():
    op = sys.argv[1]
    if op == "serve":
        try:
            serve()
        except Exception:
            FAILURE.write_text("The agent could not start. Check available container memory and restart the agent.")
            raise
    elif op == "libraries":
        with (STATE / ".configure.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            choose_libraries(json.load(sys.stdin))
    elif op == "status":
        print(json.dumps(status()))
    elif op in ("configure", "rotate"):
        with (STATE / ".configure.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            values = json.load(sys.stdin)
            try:
                if op == "configure":
                    configure(values)
                else:
                    cfg = json.loads(CONFIG.read_text())
                    if cfg.get("provider") != "venice":
                        raise ValueError("This agent uses a local model.")
                    venice.test_model(values["model"], values["key"])
                    venice.store(STATE, values["model"], values["key"])
            except venice.ModelTestFailed:
                raise
            except (ValueError, RuntimeError, OSError) as error:
                # The app shows this line instead of a general failure.
                print(json.dumps({"error": str(error)}), flush=True)
                raise SystemExit(1) from error
    else:
        raise ValueError("Unknown container operation.")


if __name__ == "__main__":
    try:
        main()
    except venice.ModelTestFailed:
        sys.exit(42)
