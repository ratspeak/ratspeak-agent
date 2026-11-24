"""Owner-only messaging with the Hermes API on the installer's desktop host."""
import hashlib
import json
import select
import socket
import sys
import time

from . import units

LIMIT = 256 * 1024


def exchange(state, request):
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(195)
        # The login helper may still be attaching after a container restart.
        deadline = time.monotonic() + 6
        while True:
            try:
                conn.connect(str(state / "hermes.sock"))
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.2)
        conn.sendall(json.dumps(request).encode() + b"\n")
        with conn.makefile("rb") as wire:
            result = json.loads(wire.readline(LIMIT))
    if not isinstance(result.get("reply"), str) or not result["reply"].strip():
        raise ValueError("Hermes did not return a reply")
    return result


def answer(state, cfg, peer, message):
    if cfg["access"] != {"mode": "owners", "owners": [peer]}:
        raise ValueError("Only the paired owner can use Hermes.")
    session = "ratspeak-" + hashlib.sha256(f"{cfg['address']}:{peer}".encode()).hexdigest()[:32]
    turn = hashlib.sha256(json.dumps([session, message["id"]]).encode()).hexdigest()
    return exchange(state, {"session": session, "turn": turn, "text": message["text"]})


def relay(state):
    """A private Docker exec pipe connects the container to the host. No TCP listener.
    The host adds the Hermes port and key it recorded; the container never holds them."""
    path = state / "hermes.sock"
    with socket.socket(socket.AF_UNIX) as listener:
        path.unlink(missing_ok=True)
        listener.bind(str(path))
        path.chmod(0o600)
        listener.listen(1)
        while True:
            ready, _, _ = select.select([listener, sys.stdin.buffer], [], [])
            if sys.stdin.buffer in ready:
                return  # The supervisor closed its end.
            conn, _ = listener.accept()
            with conn, conn.makefile("rb") as reader:
                conn.settimeout(5)
                try:
                    request = json.loads(reader.readline(LIMIT))
                    print(json.dumps(request), flush=True)
                    result = sys.stdin.buffer.readline(LIMIT)
                    if not result:
                        return
                    conn.sendall(result)
                except (OSError, ValueError):
                    continue


if __name__ == "__main__":
    relay(units.STATE)
