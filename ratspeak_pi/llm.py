"""The model side: fetch llama.cpp and a model, run llama-server, and measure its speed."""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

from .ladder import LADDER

PORT = 8089
BENCH_PROMPT = "Explain in plain words why the sky is blue."
BENCH_TOKENS = 48

CHAT_PROMPT = (
    "You are an AI assistant answering messages through Ratspeak. Reply in the user's language and use plain text. "
    "Keep replies brief unless the user asks for detail. Say when you're unsure or need more information. "
    "Users can send /help to see available commands."
)
ATTACHMENT_PROMPT = ("Use the supplied attachment text to answer. If asked what you can read, give a detail from "
                     "the text when available and say if the requested content is missing.")

# Match whole stock prompts so migration preserves custom prompts with similar openings.
LEGACY_CHAT_PROMPTS = (
    "You are {name}, an assistant reached through Ratspeak. Answer in plain text. "
    "Keep replies brief unless asked for detail. You cannot browse or run commands.",
    "You are {name}, a small assistant running on a Raspberry Pi that your owner set up. People message you through "
    "Ratspeak, and each reply is sent whole, so answer in plain text without Markdown and keep it short: two to four "
    "sentences unless they ask for more. You cannot browse the internet, run commands, or see files. You are a small "
    "model: if you are not sure of something, say so.",
    "You are Ratspeak Agent, an AI assistant. A display name is a contact label.\n"
    "Reply in the user's language. Keep replies brief and in plain text unless asked for detail. "
    "Say when information is missing or uncertain.\n"
    "The /help command lists available commands.",
)


def chat_prompt(system: str, name: str) -> str:
    """Bring installed defaults up to date while preserving custom instructions."""
    saved = system.strip()
    if saved in ("", CHAT_PROMPT) or any(saved == template.format(name=name) for template in LEGACY_CHAT_PROMPTS):
        return CHAT_PROMPT
    return system


def sources(url: str) -> list[str]:
    """Kiwix sends each location to one mirror, which can crawl. Its metalink lists them all: time a short
    sample from each and try the fastest first. The pinned digest still decides what is kept."""
    if not url.startswith("https://download.kiwix.org/"):
        return [url]
    try:
        meta = subprocess.run(["curl", "--fail", "--silent", "--location", "--max-time", "20", "--max-filesize", "4000000",
                               url + ".meta4"], capture_output=True, check=True).stdout
        found = ET.fromstring(meta).findall("m:file/m:url", {"m": "urn:ietf:params:xml:ns:metalink"})
        mirrors = [u.text.strip() for u in sorted(found, key=lambda u: int(u.get("priority", "999")))
                   if u.text and u.text.strip().startswith("https://")][:8]
    except (subprocess.CalledProcessError, ET.ParseError, ValueError):
        return [url]

    def speed(mirror):
        probe = subprocess.run(["curl", "--fail", "--silent", "--range", "0-4194303", "--max-time", "6",
                                "--output", os.devnull, "--write-out", "%{speed_download}", mirror], capture_output=True, text=True)
        try:
            return float(probe.stdout or 0)
        except ValueError:
            return 0.0
    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(len(mirrors) or 1) as pool:
        speeds = dict(zip(mirrors, pool.map(speed, mirrors)))
    return sorted(mirrors, key=speeds.get, reverse=True) + [url]


def fetch_complete(url: str, path: Path, expected: str, say=print) -> Path:
    """fetch(), but the file appears under its own name only once it is whole and verified."""
    if path.exists() and sha256(path) == expected:
        return path
    if os.environ.get("RATSPEAK_OFFLINE_DIR"):
        return fetch(url, path, expected, say)   # the card copy lands whole through its .part file already
    partial = path.with_name(path.name + ".part")
    fetch(url, partial, expected, say)
    partial.replace(path)
    return path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


CURL_FAILURES = {6: "its server’s name couldn’t be found", 7: "its server couldn’t be reached", 22: "its server returned an error",
                 28: "the download timed out", 35: "the secure connection failed", 56: "the connection dropped",
                 60: "its server’s certificate couldn’t be verified. Check the Pi’s date and time"}


def curl(command: list[str], path: Path, progress=None) -> int:
    if progress is None:
        return subprocess.run(command).returncode
    with subprocess.Popen(command) as running:
        while True:
            try:
                return running.wait(timeout=1)
            except subprocess.TimeoutExpired:
                progress(path.stat().st_size if path.exists() else 0)


def fetch(url: str, path: Path, expected: str, say=print, progress=None) -> Path:
    """Download with resume, then verify. A file that already matches is kept; one that does not is refetched once.
    progress(bytes), when given, hears how much has arrived about once a second."""
    path.parent.mkdir(parents=True, exist_ok=True)
    offline = os.environ.get("RATSPEAK_OFFLINE_DIR")
    if offline:
        if path.is_file() and sha256(path) == expected:
            return path
        source = Path(offline) / path.name
        if not source.is_file():
            raise RuntimeError(f"The offline card is missing {path.name}")
        pending = path.with_name(path.name + ".part")   # where the dashboard looks for a library's progress
        try:
            with source.open("rb") as copy, pending.open("wb") as out:
                copied = 0
                while block := copy.read(1 << 22):
                    out.write(block)
                    copied += len(block)
                    if progress:
                        progress(copied)
            if sha256(pending) != expected:
                raise RuntimeError(f"The offline copy of {path.name} failed verification")
            pending.replace(path)
            return path
        finally:
            pending.unlink(missing_ok=True)
    for attempt in (1, 2):
        if path.exists() and sha256(path) == expected:
            return path
        if attempt == 2:
            path.unlink(missing_ok=True)
        say(f"Downloading {path.name}")
        candidates = sources(url)
        for number, source in enumerate(candidates, 1):
            # A source that stalls below 50 KB/s for a minute hands over, resuming where it stopped.
            # The last has no floor, so a slow connection still finishes.
            floor = [] if number == len(candidates) else ["--speed-limit", "51200", "--speed-time", "60"]
            command = ["curl", "--fail", "--location", "--continue-at", "-", "--retry", "5", "--retry-delay", "2",
                       *floor, "--progress-bar", "--output", str(path), source]
            code = curl(command, path, progress)
            if code == 0:
                break
        else:
            reason = CURL_FAILURES.get(code, f"curl error {code}")
            raise RuntimeError(f"Couldn’t download {path.name.removesuffix('.part')}: {reason}.")
    if sha256(path) != expected:
        path.unlink()
        raise RuntimeError(f"{path.name} did not match its published SHA-256 after two downloads; nothing was kept")
    return path


def _unpack(archive: Path, target: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive) as tar:
        if hasattr(tarfile, "data_filter"):
            tar.extractall(target, filter="data")
        else:  # Python 3.11.2 (Bookworm) predates extraction filters: refuse anything that leaves the folder
            for member in tar.getmembers():
                if member.name.startswith("/") or ".." in Path(member.name).parts or member.isdev():
                    raise RuntimeError(f"Unsafe path in the llama.cpp archive: {member.name}")
            tar.extractall(target)


def install_llama(prefix: Path, cache: Path, say=print, progress=None, cuda=False) -> Path:
    """llama-server from the pinned llama.cpp release, unpacked under prefix/llama (prefix/llama-cuda for the
    NVIDIA build, whose CUDA runtime goes beside the binary, where its RPATH looks). Returns the binary."""
    pin = LADDER["llama_cuda" if cuda else "llama_cpp"]
    home = prefix / ("llama-cuda" if cuda else "llama")
    marker = home / pin["tag"]
    if not marker.exists():
        _unpack(fetch(pin["url"], cache / Path(pin["url"]).name if cuda else cache / f"llama-{pin['tag']}.tar.gz",
                      pin["sha256"], say, progress), home)
        if cuda:
            runtime = pin["runtime"]
            staged = home / "runtime"
            _unpack(fetch(runtime["url"], cache / Path(runtime["url"]).name, runtime["sha256"], say, progress), staged)
            server = next(home.rglob("llama-server"), None)
            if server is None:
                raise RuntimeError("The llama.cpp archive holds no llama-server")
            for library in staged.rglob("*.so*"):
                library.replace(server.parent / library.name)
            shutil.rmtree(staged)
        marker.write_text(pin["sha256"] + "\n")
    found = sorted(home.rglob("llama-server"))
    if not found:
        raise RuntimeError("The llama.cpp archive holds no llama-server")
    check = subprocess.run([str(found[0]), "--version"], capture_output=True, text=True)
    if check.returncode:
        raise RuntimeError("llama-server does not run on this system: " + (check.stderr or check.stdout).strip()[-300:])
    return found[0]


def cuda_devices(binary: Path) -> list[str]:
    """The NVIDIA cards llama.cpp can reach: none when the container wasn't given one or its driver is too old."""
    listing = subprocess.run([str(binary), "--list-devices"], capture_output=True, text=True, timeout=120)
    return [line.strip() for line in (listing.stdout + listing.stderr).splitlines() if line.strip().startswith("CUDA")]


def threads(cores: int) -> int:
    """One thread per core, up to the four a Pi has. More threads than fast cores makes llama.cpp far slower:
    16 threads measured 4 tokens per second where 8 gave 139 on the same machine."""
    return max(1, min(cores, 4))


def server_command(binary: Path, model: Path, context: int, threads: int, thinking: bool) -> list[str]:
    command = [str(binary), "--model", str(model), "--alias", "local", "--host", "127.0.0.1", "--port", str(PORT),
               "--ctx-size", str(context), "--threads", str(threads), "--parallel", "1", "--no-webui"]
    if thinking:
        command += ["--reasoning-budget", "0"]  # answers only: thinking tokens cost seconds the reader never sees
    return command


def _post(path: str, body: dict, timeout: float) -> dict:
    request = urllib.request.Request(f"http://127.0.0.1:{PORT}{path}", json.dumps(body).encode(),
                                     {"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read())


def healthy(timeout: float = 2) -> bool:
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{PORT}/health", timeout=timeout) as response:
            return response.status == 200
    except (OSError, urllib.error.URLError):
        return False


def bench(command: list[str], load_timeout: float = 600) -> float:
    """Tokens per second for a short answer, with the server started and stopped around the test."""
    server = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + load_timeout
        while not healthy():
            if server.poll() is not None:
                raise RuntimeError("llama-server stopped while loading: " + (server.stderr.read() or "")[-400:])
            if time.monotonic() > deadline:
                raise RuntimeError("The model took too long to load")
            time.sleep(1)
        # The chat endpoint, as the answer loop uses it: a raw completion can end after one token.
        # Best of two runs: the first also pays for warming the model into memory.
        best = 0.0
        for _ in range(2):
            result = _post("/v1/chat/completions", {"model": "local", "max_tokens": BENCH_TOKENS, "cache_prompt": False,
                                                    "messages": [{"role": "user", "content": BENCH_PROMPT}]}, 900)
            timings = result.get("timings") or {}
            if timings.get("predicted_n", 0) < 8 or not timings.get("predicted_per_second"):
                raise RuntimeError(f"The speed test produced too little text to measure: {json.dumps(timings)[:200]}")
            best = max(best, float(timings["predicted_per_second"]))
        return best
    finally:
        server.terminate()
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()
