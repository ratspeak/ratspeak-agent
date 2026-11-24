"""systemd units: the model server, the Ratspeak answer loop, and the setup page. All run as the `ratspeak` user."""
from __future__ import annotations

import shlex
from pathlib import Path

USER = "ratspeak"
PREFIX = Path("/opt/ratspeak-pi")
STATE = Path("/var/lib/ratspeak-pi")
PROFILE = STATE / "profile"
HELPER = PREFIX / "runtime" / "agent_rat.py"
PAGE_PORT = 8080


def _unit(description: str, command: list[str], after: str = "network-online.target", extra: str = "") -> str:
    return f"""[Unit]
Description={description}
After={after}
Wants={after}

[Service]
User={USER}
WorkingDirectory={STATE}
ExecStart={shlex.join(command)}
Restart=always
RestartSec=5
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths={STATE}
ProtectHome=yes
PrivateTmp=yes
{extra}
[Install]
WantedBy=multi-user.target
"""


def units(server_command: list[str], llm_port: int) -> dict[str, str]:
    module = ["/usr/bin/python3", "-m", "ratspeak_pi"]
    env = f"Environment=PYTHONPATH={PREFIX}\n"
    apply = f"""[Unit]
Description=Ratspeak Pi: apply settings left on the card by the setup app
After=NetworkManager.service
Before=ratspeak-pi-llm.service ratspeak-pi-answer.service
ConditionPathExists=|/boot/firmware/ratspeak-pi/apply.json
ConditionPathExists=|/boot/firmware/ratspeak-pi/erase-chats.json
ConditionPathExists=|/boot/firmware/ratspeak-pi/new-identity.json

[Service]
Type=oneshot
WorkingDirectory={PREFIX}
ExecStart={shlex.join(module + ["apply-pending"])}

[Install]
WantedBy=multi-user.target
"""
    # The answer loop runs unprivileged and cannot write the card's boot partition, so when it changes the
    # config (a /setup claim, /allow) this pair copies the result to status.json for the setup app to read.
    sync_path = f"""[Unit]
Description=Ratspeak Pi: watch for settings changed by message

[Path]
PathChanged={STATE}/config.json
PathChanged={STATE}/venice-model

[Install]
WantedBy=multi-user.target
"""
    sync = f"""[Unit]
Description=Ratspeak Pi: copy settings to the card's status file

[Service]
Type=oneshot
WorkingDirectory={PREFIX}
ExecStart={shlex.join(module + ["sync-status"])}
"""
    made = {
        "ratspeak-pi-activity.service": sync.replace("copy settings to the card's status file", "save agent activity on the card").replace("sync-status", "save-activity"),
        "ratspeak-pi-activity.timer": """[Unit]
Description=Ratspeak Pi: save the latest activity for SD card management

[Timer]
OnBootSec=10min
OnUnitActiveSec=10min

[Install]
WantedBy=timers.target
""",
        "ratspeak-pi-sync.path": sync_path,
        "ratspeak-pi-sync.service": sync,
        # Nice=5: answering yields to the radio stack and SSH, so the Pi stays reachable under load.
        "ratspeak-pi-llm.service": _unit("Ratspeak Pi model server (llama.cpp)", server_command, extra="Nice=5\n"),
        "ratspeak-pi-answer.service": _unit("Ratspeak Pi: answer Ratspeak messages", module + ["bridge"],
                                            after="ratspeak-pi-llm.service" if server_command else "network-online.target", extra=env),
        "ratspeak-pi-page.service": _unit("Ratspeak Pi setup page", module + ["page"], extra=env),
        "ratspeak-pi-apply.service": apply,
    }
    if not server_command:
        del made["ratspeak-pi-llm.service"]
    return made
