"""ratspeak-pi: setup | apply | status | allow | page (bridge, apply-pending, sync-status, and install-status are run by
systemd)"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys

from . import VERSION, ladder, llm, page, setup, units


def main() -> int:
    parser = argparse.ArgumentParser(prog="ratspeak-pi", description=__doc__)
    parser.add_argument("--version", action="version", version=VERSION)
    sub = parser.add_subparsers(dest="command", required=True)
    s = sub.add_parser("setup", help="Choose a model for this board and start answering Ratspeak messages")
    s.add_argument("--users", choices=list(ladder.USER_STEP), help=argparse.SUPPRESS)
    s.add_argument("--smarter", action="store_true", help=argparse.SUPPRESS)
    s.add_argument("--name", help="Name contacts see")
    s.add_argument("--owner", action="append", help="A Ratspeak address allowed to message it (repeatable)")
    s.add_argument("--access", choices=["owners", "anyone", "later"],
                   help="Who may message it; 'later' lets the first person to send the setup code claim it")
    s.add_argument("--hub", action="append", help="Use this hub, host:port, in place of Ratspeak's public hubs (repeatable)")
    s.add_argument("--local-only", action="store_true", help="No hubs: reachable on its own network only")
    s.add_argument("--feature", action="append", choices=setup.DOWNLOADS, help="Add an optional library or translation language (repeatable)")
    s.add_argument("--settings", help="Read every answer from this JSON file (written by the Mac setup app)")
    s.add_argument("--model", choices=[r["id"] for r in ladder.RUNGS], help="Use this model, skipping the pick")
    s.add_argument("--yes", action="store_true", help="Take the default for every question not given as a flag")
    s.add_argument("--no-services", action="store_true", help="Set up files only (containers, tests)")
    s.add_argument("--force", action="store_true", help="Run on a board that isn't a 64-bit Pi")
    ap = sub.add_parser("apply", help="Change an installed Pi from a settings file")
    ap.add_argument("file")
    ap.add_argument("--no-services", action="store_true")
    sub.add_parser("apply-pending", help="Apply the change file on the card, if any (run by systemd at boot)")
    sub.add_parser("extras", help="Fetch or remove optional libraries after a settings change (run by systemd)")
    sub.add_parser("save-activity", help="Save an activity reading to the card")
    sub.add_parser("sync-status", help="Copy the settings to the card's status file (run by systemd)")
    sub.add_parser("bridge", help="Answer Ratspeak messages (run by systemd)")
    sub.add_parser("status", help="What it is running and whether it is answering")
    a = sub.add_parser("allow", help="Let one more Ratspeak address message it")
    a.add_argument("address")
    p = sub.add_parser("page", help="Serve the setup page (run by systemd)")
    p.add_argument("--port", type=int, default=units.PAGE_PORT)
    sub.add_parser("pairing-code", help="Show the current owner setup code on this Pi")
    sub.add_parser("install-status", help="Serve install progress to the setup app during first boot (run by systemd)")
    args = parser.parse_args()
    if args.command == "pairing-code":
        from .claim import current
        cfg = json.loads((units.STATE / "config.json").read_text())
        pairing = current(cfg)
        print(f"{pairing['claim_code'][:3]} {pairing['claim_code'][3:]}" if pairing else "This agent is already paired.")
        return 0
    if args.command == "setup":
        return setup.run(args)
    if args.command == "install-status":
        from . import install_status
        return install_status.serve()
    if args.command == "apply":
        setup.apply(args.file, services=not args.no_services)
        return 0
    if args.command == "apply-pending":
        return setup.apply_pending()
    if args.command == "extras":
        return setup.extras()
    if args.command == "save-activity":
        from .setup import save_activity
        return save_activity()
    if args.command == "sync-status":
        setup.sync_status()
        return 0
    if args.command == "bridge":
        from . import bridge
        bridge.run(llm.PORT)
    if args.command == "page":
        page.serve(args.port)
    if args.command == "allow":
        address = args.address.lower()
        if not setup.ADDRESS.fullmatch(address):
            raise SystemExit("A Ratspeak address is 32 hexadecimal characters")
        from .access_control import access_lock
        with access_lock(units.STATE):
            cfg = page.read(units.STATE / "config.json")
            if cfg.get("provider") == "hermes":
                raise SystemExit("This Hermes connection is owner-only.")
            setup.helper("allow", "--peer", address, "--reinstate", user=units.USER)
            if cfg:
                changed = False
                if address not in cfg["access"]["owners"]:
                    cfg["access"]["owners"].append(address)
                    changed = True
                if cfg["access"]["mode"] == "later":
                    cfg["access"]["mode"] = "owners"
                    cfg.pop("claim_secret", None)
                    cfg.pop("claim_attempts", None)
                    setup.helper("allow", "--nobody", user=units.USER)
                    changed = True
                if changed:
                    setup.finish(cfg)
        print(f"{address} may now message it")
    if args.command == "status":
        cfg = page.read(units.STATE / "config.json")
        if not cfg:
            raise SystemExit("Not set up yet: run sudo ratspeak-pi setup")
        if cfg.get("provider") == "venice":
            from . import venice
            cfg["model_name"] = venice.selected(units.STATE, cfg["model_name"])
        state = page.read(units.PROFILE / "answer.json")
        active = {u: subprocess.run(["systemctl", "is-active", u], capture_output=True, text=True).stdout.strip()
                  for u in units.units(cfg.get("command") or [], llm.PORT)}
        print(json.dumps({"name": cfg["name"], "address": cfg["address"], "model": cfg["model_name"],
                          "provider": cfg.get("provider", "local"), "tokens_per_second": cfg["speed"],
                          "model_server_up": None if cfg.get("provider") == "venice" else llm.healthy(),
                          "replies": state.get("replies", 0), "last_error": state.get("last_error"),
                          "services": active}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
