"""The setup page: this Pi's name, Ratspeak address and QR code, and whether it is answering.

Read-only and served to the local network, for a Pi with no screen. Nothing on it is secret: the address
and public key are what the Pi announces anyway.
"""
from __future__ import annotations

import base64
import hashlib
import html
import json
import re
import subprocess
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import llm, units, venice, live_status


def read(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def contact_card_payload(link: str, name: str) -> str:
    match = re.fullmatch(r"lxma://([0-9a-fA-F]{32}):([0-9a-fA-F]{128})", link)
    if not match:
        return ""
    address, key_hex = match.groups()
    key = bytes.fromhex(key_hex)
    identity = hashlib.sha256(key).digest()[:16]
    service = hashlib.sha256(b"lxmf.delivery").digest()[:10]
    if address.lower() != hashlib.sha256(service + identity).hexdigest()[:32]:
        return ""
    name = name.strip().encode()[:40].decode("utf-8", errors="ignore").rstrip()
    def token(value):
        return base64.urlsafe_b64encode(value).rstrip(b"=").decode("ascii")
    return f"RSCP2:person:{token(name.encode())}:{address.lower()}:{identity.hex()}:{token(key)}:"


def qr_svg(text: str) -> str:
    if not text:
        return ""
    try:
        done = subprocess.run(["qrencode", "-t", "SVG", "-m", "4", "-o", "-", text], capture_output=True, text=True,
                              timeout=10)
        return done.stdout[done.stdout.find("<svg"):] if done.returncode == 0 else ""
    except (OSError, subprocess.TimeoutExpired):
        return ""


def render() -> str:
    cfg, state = read(units.STATE / "config.json"), read(units.PROFILE / "answer.json")
    if not cfg:
        return '<!doctype html><meta name="color-scheme" content="dark"><title>Ratspeak Pi</title><p>Setup has not finished. Run <code>sudo ratspeak-pi setup</code>.</p>'
    cloud = cfg.get("provider") == "venice"
    up = bool(state.get("started_at")) if cloud else llm.healthy()
    model = f"{venice.selected(units.STATE, cfg.get('model_name'))} · Venice" if cloud else f"{cfg.get('model_name')} (about {cfg.get('speed')} tokens per second)"
    last = state.get("last_reply_at")
    status = "Answering" if up and not state.get("last_error") else "Model is starting or stopped" if not up \
        else f"Waiting: {state['last_error']}"
    rows = [("Status", status), ("Model", model),
            ("Replies sent", state.get("replies", 0)),
            ("Last reply", time.strftime("%Y-%m-%d %H:%M", time.localtime(last)) if last else "none yet"),
            ("Who may message it", {"anyone": "anyone with the address", "later": "awaiting the owner's setup code"}
             .get(cfg.get("access", {}).get("mode"), f"{len(cfg.get('access', {}).get('owners', []))} allowed contact(s)")),
            ("Board", f"{cfg.get('board')}, {cfg.get('ram_mb')} MB")]
    table = "".join(f"<tr><th>{html.escape(str(k))}</th><td>{html.escape(str(v))}</td></tr>" for k, v in rows)
    return f"""<!doctype html><html lang="en"><meta charset="utf-8"><meta name="color-scheme" content="dark"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(cfg['name'])} · Ratspeak Pi</title>
<style>
:root{{color-scheme:dark;--bg:#26303d;--ink:#eef3f6;--card:#2d3948;--line:#465361;--accent:#a2cdd7}}
body{{margin:0;background:var(--bg);color:var(--ink);font:16px/1.5 system-ui,sans-serif}}
main{{max-width:520px;margin:0 auto;padding:24px 16px}}
h1{{font-size:28px;margin:0 0 4px}} p{{margin:8px 0}}
.qr{{background:#fff;border:1px solid var(--line);border-radius:16px;padding:12px;margin:16px 0}} .qr svg{{display:block;width:100%;height:auto}}
code{{word-break:break-all;color:var(--accent)}}
table{{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--line);border-radius:12px;overflow:hidden}}
th,td{{text-align:left;padding:10px 12px;border-top:1px solid var(--line);vertical-align:top}} tr:first-child th,tr:first-child td{{border-top:0}}
th{{font-weight:600;width:42%}}
</style>
<main>
<h1>{html.escape(cfg['name'])}</h1>
<p>{"This Pi answers Ratspeak messages using Venice." if cloud else "This Pi answers Ratspeak messages with a local model."}</p>
<div class="qr">{qr_svg(contact_card_payload(cfg.get('link', ''), cfg['name']))}</div>
<p>Contacts → Add contact → From QR code</p>
<p>Agent address <code>{html.escape(cfg.get('address', ''))}</code></p>
{f'<table>{table}</table>'}
</main></html>"""


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith("/agent-status/"):
            try:
                body = live_status.respond(units.STATE, self.path[len("/agent-status/"):])
            except (ImportError, OSError, ValueError, KeyError):
                body = None
            if body is None:
                self.send_error(404)
                return
            content_type = "text/plain"
        elif self.path in ("/", "/index.html"):
            body = render().encode()
            content_type = "text/html; charset=utf-8"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def serve(port: int = units.PAGE_PORT):
    ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
