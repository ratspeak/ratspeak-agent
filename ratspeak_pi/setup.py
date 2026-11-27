"""Install and reconfigure. One settings dictionary drives both, wherever it comes from:

  the terminal wizard        `sudo ratspeak-pi setup`
  flags                      `sudo ratspeak-pi setup --yes --owner <address>`
  a file from the Mac app    `ratspeak-pi setup --settings F` on first boot; `ratspeak-pi apply F` for later changes

settings = {"name", "users": "1"|"2-3"|"4+", "smarter": bool, "access": {"mode": "owners"|"anyone"|"later", "owners": [...]},
            "hubs": ["host:port", ...], "features": {"wikipedia": bool}, "wifi": {"ssid", "password", "country"} (apply only)}
The local network interface is always on, so the Pi is reachable from its own Wi-Fi with no hub at all.
"""
from __future__ import annotations

import json
import os
import pwd
import re
import shutil
import subprocess
import sys
import secrets
import time
import tempfile
import zipfile
from pathlib import Path

from . import hardware, identity_reset, install_status, ladder, llm, maintenance, translate, units, venice, voice
from .access_control import access_lock

ADDRESS = re.compile(r"[0-9a-f]{32}")
HUB = re.compile(r"[A-Za-z0-9.-]{1,253}:\d{1,5}")
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9 _.-]{0,31}")
# Ratspeak's public hubs, on by default so the Pi is reachable from anywhere. The Pi is never a transport node.
PUBLIC_HUBS = ["1.ratspeak.org:4141", "2.ratspeak.org:4242", "3.ratspeak.org:4343"]
RESOURCES = json.loads((Path(__file__).parent / "resources.json").read_text())
DOWNLOADS = [*RESOURCES, *translate.LANGUAGES, "voice"]      # extras fetched after the agent is answering
BOOT = Path("/boot/firmware/ratspeak-pi")   # on the card's FAT partition: the Mac app reads and writes here


def validate(settings: dict) -> dict:
    """A clean copy of the settings, or SystemExit naming what is wrong."""
    name = str(settings.get("name") or "Pi").strip()
    if not NAME.fullmatch(name):
        raise SystemExit("The name can use letters, digits, spaces, and . _ - (32 characters at most)")
    users = settings.get("users") or "1"
    if users not in ladder.USER_STEP:
        raise SystemExit("users must be 1, 2-3 or 4+")
    access = dict(settings.get("access") or {})
    owners = [str(o).strip().lower() for o in access.get("owners") or []]
    for owner in owners:
        if not ADDRESS.fullmatch(owner):
            raise SystemExit(f"{owner} is not a Ratspeak address (32 hexadecimal characters)")
    mode = access.get("mode") or ("owners" if owners else "later")
    if mode not in ("owners", "anyone", "later") or (mode == "owners" and not owners):
        raise SystemExit("access.mode must be owners (with at least one address), anyone, or later")
    hubs = [str(h).strip() for h in settings.get("hubs") or []]
    for hub in hubs:
        if not HUB.fullmatch(hub):
            raise SystemExit(f"{hub} is not a hub address (host:port)")
    requested = settings.get("features") or {}
    # Voice messages are on unless chosen off; only local models use extras.
    local = settings.get("provider", "local") == "local"
    features = {key: bool(requested.get(key, key == "voice" and local)) for key in DOWNLOADS}
    clean = {"name": name, "users": users, "smarter": bool(settings.get("smarter")), "features": features, "access": {"mode": mode, "owners": list(dict.fromkeys(owners))}, "hubs": hubs}
    if settings.get("offline") is True:
        clean["offline"] = True   # no network at all: the card brought every download
    provider = settings.get("provider", "local")
    if provider not in ("local", "venice"):
        raise SystemExit("Unknown inference provider")
    clean["provider"] = provider
    if provider == "venice":
        cloud = str(settings.get("venice_model") or "")
        if not venice.MODEL.fullmatch(cloud) or settings.get("offline") or settings.get("model"):
            raise SystemExit("Venice needs a cloud model and an internet connection.")
        clean["venice_model"] = cloud
    if settings.get("model"):
        if settings["model"] not in {r["id"] for r in ladder.RUNGS}:
            raise SystemExit("Unknown model in setup settings")
        clean["model"] = settings["model"]
    if "monitor_key" in settings:
        if not re.fullmatch("[0-9a-f]{64}", str(settings["monitor_key"])):
            raise SystemExit("Invalid installer monitor key")
        clean["monitor_key"] = settings["monitor_key"]
    if settings.get("pairing_link"):
        if not re.fullmatch(r"lxma://[0-9a-f]{32}:[0-9a-f]{128}", str(settings["pairing_link"])):
            raise SystemExit("Invalid pairing link on card")
        clean["pairing_link"] = settings["pairing_link"]
    if mode == "later" and settings.get("claim_secret") is not None:
        if not re.fullmatch(r"[0-9a-f]{64}", str(settings["claim_secret"])):
            raise SystemExit("Invalid setup code on card")
        clean["claim_secret"] = settings["claim_secret"]
    wifi = settings.get("wifi")
    if wifi and wifi.get("ssid"):
        clean["wifi"] = {"ssid": str(wifi["ssid"]), "password": str(wifi.get("password") or ""),
                         "country": str(wifi.get("country") or "").upper()[:2]}
    return clean


def ask(question: str, default: str, valid=lambda value: True) -> str:
    while True:
        value = input(f"{question} [{default}]: ").strip() or default
        if valid(value):
            return value
        print("  That isn't one of the choices.")


def from_args(args) -> dict:
    """Settings from flags, asking in the terminal for whatever was not given (unless --yes)."""
    if args.settings:
        return validate(json.loads(Path(args.settings).read_text()))
    interactive = sys.stdin.isatty() and not args.yes
    users = args.users or "1"  # Older scripts can still supply an explicit preference.
    name = args.name or (ask("A name for it, as contacts will see it", "Pi") if interactive else "Pi")
    owners, mode = list(args.owner or []), args.access
    hubs = [] if args.local_only else list(args.hub or PUBLIC_HUBS[2:])   # Diamond, the quieter hub
    if interactive and not args.hub and not args.local_only:
        if ask("Reach it from anywhere through Ratspeak's public hubs? (y/n)", "y", lambda v: v in ("y", "n")) == "n":
            hubs = []
    features = {name: True for name in args.feature or []}
    return validate({"name": name, "users": users, "hubs": hubs, "smarter": args.smarter, "features": features,
                     "access": {"mode": mode or ("owners" if owners else "later"), "owners": owners}})


def choose_model(board, users: str, prefix: Path, models: Path, say=print, force: str | None = None,
                 smarter: bool = False, preferred: str | None = None, step=lambda stage, **facts: None) -> dict:
    """Measure the selected model without substituting another after a slow result.
    step(stage, done=, total=, model_name=) hears each install stage and the bytes of the two large files."""
    selected = force or preferred
    index = ladder.BY_ID[selected] if selected else ladder.pick(board.ram_mb, board.generation, users, smarter)
    rung = ladder.RUNGS[index]
    if preferred and not force and not ladder.eligible(index, board.ram_mb, board.generation):
        raise ValueError(f"{rung['name']} exceeds this Pi's processor or memory profile. Choose a different model in Ratspeak Agent.")
    step("llama", model_name=rung["name"])
    llama = ladder.LADDER["llama_cpp"]["size"]
    binary = llm.install_llama(prefix, models / "cache", say, lambda done: step("llama", done=done, total=llama))
    size = rung["download_bytes"]
    step("model", done=0, total=size)
    copying = bool(os.environ.get("RATSPEAK_OFFLINE_DIR"))
    say(f"{'Copying' if copying else 'Downloading'} {rung['name']} ({rung['size_mb']} MB){' from the card' if copying else ''}")
    model = llm.fetch(rung["url"], models / rung["file"], rung["sha256"], lambda _: None,
                      lambda done: step("model", done=done, total=size))
    context = ladder.context(index, board.ram_mb)
    command = llm.server_command(binary, model, context, llm.threads(board.cores), rung["thinking"])
    step("speed")
    say(f"Testing {rung['name']}")
    speed = llm.bench(command)
    say(f"{rung['name']}: {speed:.1f} tokens per second")
    return {"rung": rung["id"], "name": rung["name"], "context": context, "speed": round(speed, 1),
            "command": command}


def helper(*command: str, user: str | None) -> dict:
    """Run the vendored Ratspeak helper against this Pi's profile and return its JSON reply."""
    full = ["python3", str(units.HELPER), "--profile", str(units.PROFILE), *command]
    if user:
        full = ["runuser", "-u", user, "--", *full]
    done = subprocess.run(full, capture_output=True, text=True)
    if done.returncode:
        raise RuntimeError(f"Ratspeak helper `{command[0]}` failed: {(done.stderr or done.stdout).strip()[-400:]}")
    try:
        return json.loads(done.stdout)
    except ValueError:
        return {}


def hand_over(user: str | None) -> None:
    """Stop the receiver setup started, so the answer service runs it inside its own sandbox."""
    try:
        helper("stop", user=user)
    except RuntimeError:
        pass   # the service still reaches it through the profile's socket


def report(state: str, detail: str | None = None, **more) -> None:
    """Progress for the Mac app, written to the card's boot partition; a new detail is printed too.
    With no detail the last one stays, for progress counts."""
    if detail is not None:
        print(detail or state, flush=True)
    if not BOOT.is_dir():
        return
    current = install_status.read(BOOT / "status.json")
    current.update(state=state, updated=int(time.time()), **more)
    current["detail"] = current.get("detail", "") if detail is None else detail
    if state == "ready":
        for key in install_status.KEYS:
            current.pop(key, None)
    elif state in ("installing", "failed") and current.get("stage"):
        current.update(install_status.fields(current, install_status.read(BOOT / "settings.json")))
    # Replaced whole: the install status server reads it while setup writes.
    with tempfile.NamedTemporaryFile("w", dir=BOOT, prefix=".status-", delete=False) as output:
        output.write(json.dumps(current, indent=2) + "\n")
    Path(output.name).replace(BOOT / "status.json")


def apply_access(access: dict, previous: dict, user: str | None) -> None:
    """Make the helper's contact list match: 'later' and 'anyone' open the inbox; owners are allowed; removed
    owners are revoked."""
    helper("allow", "--anyone" if access["mode"] in ("anyone", "later") else "--nobody", user=user)
    for owner in access["owners"]:
        helper("allow", "--peer", owner, "--reinstate", user=user)
    for gone in set(previous.get("owners") or []) - set(access["owners"]):
        helper("revoke", "--peer", gone, user=user)


def apply_network(hubs: list[str], user: str | None) -> None:
    helper("connect", "--lan", *[part for hub in hubs for part in ("--tcp", hub)], user=user)


def apply_wifi(wifi: dict) -> None:
    """Replace the Wi-Fi network NetworkManager joins. The old profile is kept until the new one is saved."""
    if wifi.get("country"):
        subprocess.run(["raspi-config", "nonint", "do_wifi_country", wifi["country"]], check=False)
    subprocess.run(["nmcli", "connection", "delete", "ratspeak-wifi"], capture_output=True)
    add = ["nmcli", "connection", "add", "type", "wifi", "ifname", "wlan0", "con-name", "ratspeak-wifi",
           "ssid", wifi["ssid"], "connection.autoconnect", "yes", "connection.autoconnect-priority", "10"]
    if wifi["password"]:
        add += ["wifi-sec.key-mgmt", "wpa-psk", "wifi-sec.psk", wifi["password"]]
    if subprocess.run(add, capture_output=True).returncode:
        # Not CalledProcessError: its text carries the command line, password included, into status.json.
        raise RuntimeError(f"The Wi-Fi network {wifi['ssid']} could not be saved.")
    subprocess.run(["nmcli", "connection", "up", "ratspeak-wifi"], capture_output=True)


def stay_up() -> None:
    """Two settings that keep a headless Pi answering: Wi-Fi power saving off (it drops idle connections), and
    the board's hardware watchdog, which restarts the Pi if the system freezes. Both take effect at the next start."""
    Path("/etc/NetworkManager/conf.d").mkdir(parents=True, exist_ok=True)
    Path("/etc/NetworkManager/conf.d/ratspeak-pi.conf").write_text("[connection]\nwifi.powersave = 2\n")
    Path("/etc/systemd/system.conf.d").mkdir(parents=True, exist_ok=True)
    # 14 s: the Pi's watchdog cannot be set longer than 15.
    Path("/etc/systemd/system.conf.d/ratspeak-pi.conf").write_text("[Manager]\nRuntimeWatchdogSec=14\nRebootWatchdogSec=2min\n")
    subprocess.run(["nmcli", "general", "reload"], capture_output=True)


def save_extras(progress: dict) -> None:
    """What the extras are doing, for the setup app's status: the config lists only what is installed."""
    path = units.STATE / "resources" / "status.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(".status.json.new")
    temp.write_text(json.dumps(progress) + "\n")
    temp.replace(path)


def apply_features(wanted: dict, cfg: dict, services: bool) -> None:
    """Add or remove the optional libraries. A failure here is reported and leaves the Pi answering without it."""
    folder = units.STATE / "resources"
    cfg.pop("note", None)
    cfg.pop("features_wanted", None)
    # A Pi with 512 MB has no room for voice beside the model; it stays off there without a note.
    want = bool(wanted.get("voice", True)) and cfg.get("provider", "local") == "local"
    engine = voice.choose(False, hardware.detect().ram_mb) if want else None
    progress = {name: {"state": "ready" if (folder / resource["file"]).exists() else "waiting"}
                for name, resource in RESOURCES.items() if wanted.get(name)}
    progress.update({name: {"state": "ready" if translate.ready(name) else "waiting"}
                     for name in translate.LANGUAGES if wanted.get(name)})
    if engine:
        progress["voice"] = {"state": "ready" if voice.installed() == engine else "waiting", "engine": engine}
    save_extras(progress)

    def mark(name, state, error=None):
        if name in progress:
            progress[name] = {**progress[name], "state": state, **({"error": str(error)[:200]} if error else {})}
            save_extras(progress)

    for name, resource in RESOURCES.items():
        path = folder / resource["file"]
        try:
            if wanted.get(name) and not path.exists():
                free = shutil.disk_usage(units.STATE).free // (1 << 20)
                if free < resource["size_mb"] + 500:
                    raise RuntimeError(f"the card has {free} MB free and it needs {resource['size_mb'] + 500}")
                mark(name, "downloading")
                report("ready", f"Downloading {resource['name']} ({resource['size_mb']} MB)")
                if subprocess.run(["python3", "-c", "import libzim"], capture_output=True).returncode:
                    if os.environ.get("RATSPEAK_OFFLINE_DIR"):
                        raise RuntimeError("The offline Wikipedia reader is missing")
                    subprocess.run(["apt-get", "install", "-y", "-qq", "python3-libzim"], check=True, capture_output=True,
                                   env={**os.environ, "DEBIAN_FRONTEND": "noninteractive"})
                for old in folder.glob("*.zim"):          # an earlier edition of the same library
                    if old.name.split("_20")[0] == resource["file"].split("_20")[0]:
                        old.unlink()
                llm.fetch_complete(resource["url"], path, resource["sha256"], lambda _: None)
                mark(name, "ready")
            elif not wanted.get(name) and path.exists():
                path.unlink()
            cfg["features"][name] = bool(wanted.get(name))
        except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
            cfg["features"][name] = False
            mark(name, "failed", error)
            cfg["note"] = f"{resource['name']} was not added: {str(error)[:200]}"
    for name, language in translate.LANGUAGES.items():
        try:
            if wanted.get(name) and not translate.ready(name):
                free = shutil.disk_usage(units.STATE).free // (1 << 20)
                if free < language["size_mb"] + 600:
                    raise RuntimeError(f"the card has {free} MB free and it needs {language['size_mb'] + 600}")
                mark(name, "downloading")
                report("ready", f"Downloading {language['label']} translation ({language['size_mb']} MB)")
                translate.install(name, lambda url, path, sha: llm.fetch_complete(url, path, sha, lambda _: None))
                mark(name, "ready")
            elif not wanted.get(name):
                translate.remove(name)
            cfg["features"][name] = bool(wanted.get(name))
        except (RuntimeError, OSError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
            cfg["features"][name] = False
            mark(name, "failed", error)
            cfg["note"] = f"{language['label']} translation was not added: {str(error)[:200]}"
    try:
        if engine and voice.installed() != engine:
            mark("voice", "downloading")
            report("ready", f"Downloading voice messages ({voice.ENGINES[engine]['size_mb']} MB)")
            voice.install(engine, lambda url, path, sha: llm.fetch_complete(url, path, sha, lambda _: None))
            mark("voice", "ready")
        elif not engine and voice.installed():
            voice.remove()
        cfg["features"]["voice"] = bool(engine)
    except (RuntimeError, OSError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        cfg["features"]["voice"] = False
        mark("voice", "failed", error)
        cfg["note"] = f"Voice messages were not added: {str(error)[:200]}"
    with access_lock(units.STATE):
        # A /setup claim or /allow can land during a long download: keep them and change only the extras.
        saved = json.loads((units.STATE / "config.json").read_text())
        for key in ("features", "note", "features_wanted"):
            saved.pop(key, None)
        saved.update({key: cfg[key] for key in ("features", "note") if key in cfg})
        cfg.clear()
        cfg.update(saved)
        if services:
            chown()   # also hands the lock file to the service user when this created it
        finish(cfg)


def write_units(command: list[str]) -> list[str]:
    made = units.units(command, llm.PORT)
    for name, text in made.items():
        Path("/etc/systemd/system", name).write_text(text)
    subprocess.run(["systemctl", "daemon-reload"], check=True)
    return list(made)


def chown() -> None:
    subprocess.run(["chown", "-R", f"{units.USER}:{units.USER}", str(units.STATE)], check=True)


def finish(cfg: dict) -> None:
    save_config(cfg)
    sync_status(cfg)


def save_config(cfg: dict) -> None:
    path = units.STATE / "config.json"
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as handle:
        json.dump(cfg, handle, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    if path.exists():
        owner = path.stat()
        if os.geteuid() == 0:
            os.chown(handle.name, owner.st_uid, owner.st_gid)
    Path(handle.name).replace(path)


def save_activity() -> None:
    """Store a sample for later SD-card management while leaving the setup/repair status alone."""
    from .dashboard import snapshot
    if not BOOT.is_dir():
        return
    sample = snapshot(units.STATE)
    if not sample["link"]:
        return
    with tempfile.NamedTemporaryFile("w", dir=BOOT, delete=False) as output:
        json.dump({key: sample[key] for key in ("link", "stats", "sampled_at")}, output)
    Path(output.name).replace(BOOT / "activity.json")


def sync_status(cfg: dict | None = None) -> None:
    cfg = cfg or json.loads((units.STATE / "config.json").read_text())
    cloud = cfg.get("provider") == "venice"
    if cloud:
        cfg = {**cfg, "model": venice.selected(units.STATE, cfg["model"])}
        cfg["model_name"] = cfg["model"]
    detail = f"{cfg['name']} is ready, using {cfg['model_name']} through Venice." if cloud else (
        f"{cfg['name']} is ready, answering with {cfg['model_name']} at about {cfg['speed']} tokens per second.")
    from .dashboard import snapshot
    activity = snapshot(units.STATE)
    report("ready", detail
           + (" " + cfg["note"] if cfg.get("note") else ""),
           stats=activity["stats"], sampled_at=activity["sampled_at"], name=cfg["name"], address=cfg["address"], link=cfg["link"], model=cfg["model_name"], speed=cfg["speed"],
           provider=cfg.get("provider", "local"), venice_model=cfg["model"] if cloud else None, model_id=cfg["model"], model_options=cfg.get("model_options", []), offline=cfg.get("offline", False),
           access=cfg["access"], hubs=cfg["hubs"], users=cfg["users"], smarter=cfg.get("smarter", False), ram_mb=cfg.get("ram_mb"),
           features=cfg.get("features", {}), can_clear_chats=True, can_rename=True, can_reset_identity=True, version=__import__("ratspeak_pi").VERSION)


def install(settings: dict, services: bool, force_board: bool, model: str | None) -> dict:
    board = hardware.detect()
    if not board.supported and not force_board:
        raise SystemExit(f"This needs 64-bit Raspberry Pi OS (found {board.arch or 'an unknown system'}).")
    report("installing", f"{board.model or 'This computer'}: {board.ram_mb} MB of memory, {board.cores} cores",
           from_card=bool(os.environ.get("RATSPEAK_OFFLINE_DIR")))
    last_stage, last_write = None, 0.0

    def step(stage, detail=None, done=None, total=None, **facts):
        # A new stage is written at once; byte counts at most every 2 s, each being a write to the card's FAT partition.
        nonlocal last_stage, last_write
        if stage == last_stage and done is not None and done < (total or 0) and time.monotonic() - last_write < 2:
            return
        last_stage, last_write = stage, time.monotonic()
        report("installing", detail, stage=stage, done=done, total=total, **facts)

    user = units.USER if services else None
    if services:
        if os.geteuid():
            raise SystemExit("Run setup with sudo: it installs system services.")
        try:
            pwd.getpwnam(units.USER)
        except KeyError:
            subprocess.run(["useradd", "--system", "--home-dir", str(units.STATE), "--shell", "/usr/sbin/nologin",
                            units.USER], check=True)
    units.STATE.mkdir(parents=True, exist_ok=True)
    cloud = settings.get("provider") == "venice"
    if cloud:
        if settings.get("offline"):
            raise ValueError("Venice needs an internet connection.")
        model_id = venice.import_card(BOOT, units.STATE, settings["venice_model"])
        step("venice", "Testing the Venice model", model_name=model_id)
        credentials = json.loads((units.STATE / "venice.json").read_text())
        venice.test_model(model_id, credentials["key"])
        picked = {"rung": model_id, "name": model_id, "context": 4096, "speed": None, "command": []}
    else:
        picked = choose_model(board, settings["users"], units.PREFIX, units.STATE / "models",
                              say=lambda text: report("installing", text), force=model, smarter=settings["smarter"],
                              preferred=settings.get("model"), step=step)
    (units.STATE / "system.txt").write_text(llm.CHAT_PROMPT + "\n")
    if services:
        chown()
    step("identity", "Creating its Ratspeak identity")
    seed = None
    pending = BOOT / "new-identity.json"
    if settings.get("pairing_link"):
        from .pairing import provision
        if pending.exists():
            request = json.loads(pending.read_text())
            if request["old"] != settings["pairing_link"] or not identity_reset.LINK.fullmatch(request["new"]):
                raise ValueError("Identity request belongs to another card.")
            settings = {**settings, "pairing_link": request["new"]}
            seed = bytes.fromhex(request["seed"])
        provision(BOOT, settings["pairing_link"], settings["name"], services, seed)
    card = helper("prepare", "--name", settings["name"], "--native", str(units.PREFIX / "bin" / "ratspeak-pi-native"),
                  user=user).get("card", {})
    apply_network(settings["hubs"], user)
    apply_access(settings["access"], {}, user)
    card = helper("card", user=user) or card
    offline = settings.get("offline") is True   # files from the card alone do not mean the Pi has no network
    options = [ladder.RUNGS[i]["id"] for i in ladder.choices(board.ram_mb, board.generation)]
    cfg = {**{k: settings[k] for k in ("name", "users", "smarter", "access", "hubs")},
           "features": {},
           "model": picked["rung"], "model_name": picked["name"], "context": picked["context"], "speed": picked["speed"],
           "provider": "venice" if cloud else "local", "command": picked["command"], "address": card.get("address", ""),
           "link": f"lxma://{card.get('address', '')}:{card.get('public_key', '')}", "board": board.model,
           "ram_mb": board.ram_mb, "offline": offline, "model_options": [] if cloud else [picked["rung"]] if offline else options}
    if settings.get("pairing_link") and cfg["link"] != settings["pairing_link"]:
        raise RuntimeError("The installed identity does not match the installer's QR code.")
    if settings.get("monitor_key"):
        cfg["monitor_key"] = settings["monitor_key"]
    if settings["access"]["mode"] == "later":
        cfg["claim_secret"] = settings.get("claim_secret") or secrets.token_hex(32)
    save_config(cfg)
    step("starting", "Starting its services")
    if services:
        hand_over(user)
        stay_up()
        names = [n for n in write_units(picked["command"]) if n not in ("ratspeak-pi-sync.service", "ratspeak-pi-activity.service")]
        chown()
        subprocess.run(["systemctl", "enable", *names], check=True)
        subprocess.run(["systemctl", "stop", install_status.UNIT], capture_output=True)   # it holds the page's port
        # Started without waiting: the apply unit is ordered before the others and only runs at boot.
        subprocess.run(["systemctl", "--no-block", "start", *[n for n in names if n != "ratspeak-pi-apply.service"]],
                       check=True)
    sync_status(cfg)   # saved above; writing it again here could undo a claim made since the service started
    apply_features(settings["features"], cfg, services)   # after it is answering: these are large downloads
    if seed is not None:
        pending.unlink(missing_ok=True)
    return cfg


def apply(path: Path, services: bool = True) -> dict:
    """Change an installed Pi to match new settings: Wi-Fi, who may use it, hubs, and the user count."""
    cfg = json.loads((units.STATE / "config.json").read_text())
    cfg.setdefault("smarter", False)
    cfg.setdefault("features", {})
    changes = json.loads(Path(path).read_text())
    provider = cfg.get("provider", "local")
    cloud = provider == "venice"
    if "provider" in changes and changes["provider"] != provider:
        raise ValueError("Changing the inference provider requires setting up the card again.")
    base = {k: cfg[k] for k in ("name", "users", "smarter", "access", "hubs")}
    base["provider"] = provider
    if cloud:
        base["venice_model"] = venice.selected(units.STATE, cfg["model"])
    wanted = {**cfg["features"], **cfg.get("features_wanted", {}), **changes.get("features", {})}
    new = validate({**base, **changes, "provider": provider, "features": wanted})
    user = units.USER if services else None
    if "venice_api_key" in changes or "venice_model" in changes:
        if not cloud:
            raise ValueError("This agent uses a local model.")
        model_id = new["venice_model"]
        key = changes.get("venice_api_key") or json.loads((units.STATE / "venice.json").read_text())["key"]
        venice.store(units.STATE, model_id, key)
        cfg.update(model=model_id, model_name=model_id)
    selected = new.get("model")
    if cloud and selected:
        raise ValueError("This agent uses Venice.")
    if selected and cfg.get("model_options") and selected not in cfg["model_options"]:
        raise ValueError("Choose a model offered for this Pi.")
    report("applying", "Applying new settings")
    if "name" in changes and cfg["name"] != new["name"]:
        sys.path.insert(0, str(units.PREFIX / "runtime"))
        from .rename import rename
        rename(units.STATE, cfg["link"], new["name"])
        cfg["name"] = new["name"]
    if new.get("wifi"):
        apply_wifi(new["wifi"])
    if new["hubs"] != cfg["hubs"]:
        apply_network(new["hubs"], user)
    if new["access"] != cfg["access"]:
        apply_access(new["access"], cfg["access"], user)
    legacy_change = (new["users"], new["smarter"]) != (cfg["users"], cfg["smarter"])
    if (selected and selected != cfg["model"]) or (not cloud and not selected and legacy_change):
        if services:
            subprocess.run(["systemctl", "--no-block", "stop", "ratspeak-pi-answer.service", "ratspeak-pi-llm.service"],
                           check=False)
            deadline = time.monotonic() + 30
            while llm.healthy() and time.monotonic() < deadline:   # the speed test needs the port and the memory
                time.sleep(1)
        try:
            picked = choose_model(hardware.detect(), new["users"], units.PREFIX, units.STATE / "models",
                                  say=lambda text: report("applying", text), smarter=new["smarter"], preferred=selected)
        except (Exception, SystemExit):
            if services:
                subprocess.run(["systemctl", "--no-block", "start", "ratspeak-pi-llm.service", "ratspeak-pi-answer.service"],
                               check=False)
            raise
        cfg.update(model=picked["rung"], model_name=picked["name"], context=picked["context"], speed=picked["speed"],
                   command=picked["command"])
        if services:
            write_units(picked["command"])
    cfg.update({k: new[k] for k in ("users", "smarter", "access", "hubs")})
    if services:
        chown()
        hand_over(user)
        # No waiting: at boot this runs inside a unit ordered before the two it restarts.
        subprocess.run(["systemctl", "--no-block", "restart", *([] if cloud else ["ratspeak-pi-llm.service"]), "ratspeak-pi-answer.service"],
                       check=False)
    if services and any(new["features"][key] != cfg["features"].get(key, False) for key in DOWNLOADS):
        # In its own unit: at boot this runs before the network is up and before the Pi starts answering,
        # and neither should wait for a download of a gigabyte.
        cfg["features_wanted"] = new["features"]
        finish(cfg)
        subprocess.run(["systemd-run", "--no-block", "--unit=ratspeak-pi-extras", f"--working-directory={units.PREFIX}",
                        "python3", "-m", "ratspeak_pi", "extras"], check=False)
    else:
        finish(cfg)
        apply_features(new["features"], cfg, services)
    return cfg


def extras() -> int:
    """Fetch or remove the optional libraries an earlier `apply` asked for, once the internet is reachable."""
    cfg = json.loads((units.STATE / "config.json").read_text())
    wanted = cfg.get("features_wanted")
    if wanted is None:
        return 0
    for _ in range(60):
        if subprocess.run(["curl", "-fsS", "--max-time", "10", "-o", "/dev/null", "https://download.kiwix.org/"],
                          capture_output=True).returncode == 0:
            break
        time.sleep(10)
    apply_features(wanted, cfg, True)
    return 0


def apply_pending() -> int:
    """Run at every boot: apply the change file the Mac app left on the card, once."""
    pending = BOOT / "apply.json"
    if not (units.STATE / "config.json").exists():
        return 0
    chats_ok = maintenance.apply_chat_request(BOOT)
    if not identity_reset.apply_request(BOOT):
        return 1
    if not pending.exists():
        return 0 if chats_ok else 1
    def receipt(name):
        # Preserve the usual receipt while excluding the key from either outcome.
        try:
            safe = json.loads(pending.read_text())
            safe.pop("venice_api_key", None)
            if isinstance(safe.get("wifi"), dict):
                safe["wifi"].pop("password", None)
        except (ValueError, AttributeError):
            safe = {}
        (BOOT / name).write_text(json.dumps(safe) + "\n")
        pending.unlink()
    try:
        apply(pending)
        receipt("applied.json")
    except (SystemExit, Exception) as error:  # noqa: BLE001 - whatever went wrong must reach the status file
        report("failed", f"The new settings were not applied: {error}")
        receipt("apply-failed.json")
        return 1
    return 0 if chats_ok else 1


def run(args) -> int:
    settings = from_args(args)
    try:
        cfg = install(settings, not args.no_services, args.force, args.model)
    except (RuntimeError, subprocess.CalledProcessError) as error:
        report("failed", f"Setup stopped: {error}")
        return 1
    print(f"Its Ratspeak address: {cfg['address']}")
    if shutil.which("qrencode"):
        subprocess.run(["qrencode", "-t", "ANSIUTF8", "-m", "1", cfg["link"]])
        print("Scan this in the Ratspeak app (Contacts, Add, Scan), then send it a message.")
    if cfg["access"]["mode"] == "later":
        if not args.settings:
            from .claim import current
            code = current(cfg)["claim_code"]
            print(f"To claim this Pi, send {code[:3]} {code[3:]}. Run sudo ratspeak-pi pairing-code for a fresh code.")
    if not args.no_services:
        print(f"The contact QR code is on http://{os.uname().nodename}.local:{units.PAGE_PORT} from this network.")
    return 0
