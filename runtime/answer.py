"""Reply to messages with a model behind an OpenAI-compatible chat endpoint (Ollama, llama.cpp, LM Studio, hosted APIs)."""
from __future__ import annotations

import ipaddress
import json
import os
import plistlib
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from common import atomic_text

DEFAULT_URL = "http://127.0.0.1:11434/v1"
CONTEXT_CHARS = 6000
REPLY_CHARS = 4000
MAX_RESPONSE = 1 << 20
OFFLINE_NOTE = "Automatic note: the model on this computer is not answering right now. Please try again later."


def endpoint(url):
    """Base URL of the chat API. Plain HTTP is only for this computer, so message text never crosses a network in the clear."""
    parts = urllib.parse.urlsplit(url.strip())
    if parts.scheme not in ("http", "https") or not parts.hostname or parts.username or parts.password or parts.query or parts.fragment:
        raise ValueError("Model URL must look like http://127.0.0.1:11434/v1 or https://host/v1")
    if parts.scheme == "http" and not is_local(parts.hostname):
        raise ValueError("A model on another computer must be reached over https")
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, parts.path.rstrip("/"), "", ""))


def is_local(host):
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def default_system():
    return ("You are an AI assistant running on a computer your owner controls. People message you through Ratspeak, "
            "sometimes over slow radio links, so reply in plain text without Markdown and keep it short: a few sentences "
            "unless they ask for more. You cannot browse the internet, run commands, or see files. "
            "If you do not know something, say so.")


def conversation(history, system, budget=CONTEXT_CHARS):
    """Chat turns and inbox ids for one conversation, or (None, []) when nothing is waiting."""
    waiting = [entry for entry in history if entry["direction"] == "in" and entry.get("unread") and entry["text"].strip()]
    if not waiting:
        return None, []
    question = "\n\n".join(entry["text"].strip() for entry in waiting)[-budget:]
    budget -= len(question)
    turns = []
    for entry in reversed([entry for entry in history if not entry.get("unread")]):
        text = entry["text"].strip()
        if not text:
            continue
        if len(text) > budget:
            break
        budget -= len(text)
        turns.append(("user" if entry["direction"] == "in" else "assistant", text))
    turns.reverse()
    turns.append(("user", question))
    while turns[0][0] == "assistant":
        turns.pop(0)
    # Several chat templates reject two turns in a row from the same side.
    merged = []
    for role, text in turns:
        if merged and merged[-1]["role"] == role:
            merged[-1]["content"] += "\n\n" + text
        else:
            merged.append({"role": role, "content": text})
    today = time.strftime("%A, %B %d, %Y")
    return [{"role": "system", "content": f"{system.strip()}\nToday is {today}."}, *merged], [entry["id"] for entry in waiting]


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_):
        return None


def complete(url, model, messages, key=None, effort=None, timeout=300, on_usage=None, grammar=None):
    """One chat completion. Returns the reply text, trimmed for a message."""
    body = {"model": model, "messages": messages, "stream": False, "max_tokens": 700}
    if grammar:
        body["grammar"] = grammar
    if effort:
        body["reasoning_effort"] = effort
    headers = {"Content-Type": "application/json"}
    if key:
        headers["Authorization"] = f"Bearer {key}"
    request = urllib.request.Request(f"{url}/chat/completions", data=json.dumps(body).encode(), headers=headers)
    handlers = [_NoRedirect]
    if is_local(urllib.parse.urlsplit(url).hostname):
        handlers.append(urllib.request.ProxyHandler({}))
    try:
        with urllib.request.build_opener(*handlers).open(request, timeout=timeout) as response:
            raw = response.read(MAX_RESPONSE + 1)
    except urllib.error.HTTPError as error:
        detail = error.read(300).decode("utf-8", "replace").strip()
        raise RuntimeError(f"Model endpoint returned HTTP {error.code}: {detail}") from None
    except (urllib.error.URLError, OSError) as error:
        raise RuntimeError(f"Model endpoint is not reachable: {getattr(error, 'reason', error)}") from None
    if len(raw) > MAX_RESPONSE:
        raise RuntimeError("Model response is too large")
    try:
        result = json.loads(raw)
        text = result["choices"][0]["message"]["content"]
    except (ValueError, LookupError, TypeError):
        raise RuntimeError("Model response is not a chat completion") from None
    if on_usage is not None:
        on_usage(result.get("usage"))
    if not isinstance(text, str):
        raise RuntimeError("Model reply is not text")
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    if not text:
        raise RuntimeError("Model reply is empty")
    return text if len(text) <= REPLY_CHARS else text[:REPLY_CHARS - 1].rstrip() + "…"


def service_file(manifest, command, log, platform=None, home=None):
    """Name, path, and text of the login service that keeps `command` running."""
    platform, home = platform or sys.platform, home or Path.home()
    if platform == "darwin":
        name = f"org.ratspeak.agent-rat.{manifest['profile_id']}"
        text = plistlib.dumps({"Label": name, "ProgramArguments": command, "RunAtLoad": True, "KeepAlive": True,
            "ProcessType": "Background", "ThrottleInterval": 30, "StandardOutPath": str(log), "StandardErrorPath": str(log)}).decode()
        return name, home / "Library/LaunchAgents" / f"{name}.plist", text
    if platform.startswith("linux"):
        name = f"agent-rat-{manifest['profile_id']}.service"
        quoted = " ".join('"' + part.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$") + '"' for part in command)
        text = (f"[Unit]\nDescription=Agent Rat answering as {json.dumps(manifest['label'])}\n\n"
                f"[Service]\nExecStart={quoted}\nRestart=always\nRestartSec=30\n"
                f"StandardOutput=append:{log}\nStandardError=append:{log}\n\n[Install]\nWantedBy=default.target\n")
        return name, home / ".config/systemd/user" / name, text
    raise ValueError("Background answering needs macOS or Linux; run answer in a terminal instead")


def _run(*command, check=True):
    process = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if check and process.returncode:
        raise RuntimeError(f"{' '.join(command[:3])} failed: {(process.stderr or process.stdout).strip()[:300]}")
    return process


def install_service(manifest, command, log):
    name, path, text = service_file(manifest, command, log)
    path.parent.mkdir(parents=True, exist_ok=True)
    result = {"background": True, "service": name, "service_file": str(path), "log": str(log), "starts_at_login": True}
    if sys.platform == "darwin":
        domain = f"gui/{os.getuid()}"
        _run("launchctl", "bootout", f"{domain}/{name}", check=False)
        atomic_text(path, text)
        for attempt in range(10):
            process = _run("launchctl", "bootstrap", domain, str(path), check=attempt == 9)
            if not process.returncode:
                break
            time.sleep(.5)
        return result
    if _run("systemctl", "--user", "show-environment", check=False).returncode:
        raise RuntimeError("This system has no user service manager. Run answer under your own supervisor, or from cron with --once")
    atomic_text(path, text)
    _run("systemctl", "--user", "daemon-reload")
    _run("systemctl", "--user", "enable", name)
    _run("systemctl", "--user", "restart", name)
    # Without lingering, a user service stops at logout and waits for the next login.
    user = os.environ.get("USER") or str(os.getuid())
    _run("loginctl", "enable-linger", user, check=False)
    linger = _run("loginctl", "show-user", user, "--property=Linger", check=False).stdout.strip()
    result["runs_while_logged_out"] = linger == "Linger=yes"
    return result


def remove_service(manifest):
    name, path, _ = service_file(manifest, [], Path(os.devnull))
    if sys.platform == "darwin":
        _run("launchctl", "bootout", f"gui/{os.getuid()}/{name}", check=False)
    else:
        _run("systemctl", "--user", "disable", "--now", name, check=False)
    existed = path.exists()
    path.unlink(missing_ok=True)
    if sys.platform != "darwin":
        _run("systemctl", "--user", "daemon-reload", check=False)
    return {"background": False, "removed": existed}
