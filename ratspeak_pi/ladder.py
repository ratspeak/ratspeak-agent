"""Model preferences shared by the desktop installer and the Pi."""
from __future__ import annotations

import json
from pathlib import Path

LADDER = json.loads((Path(__file__).parent / "models.json").read_text())
RUNGS = LADDER["rungs"]
OS_MB = LADDER["os_mb"]                        # Raspberry Pi OS Lite, idle, plus the helper
BY_ID = {rung["id"]: i for i, rung in enumerate(RUNGS)}
USER_STEP = {"1": 0, "2-3": 1, "4+": 2}


def fits(rung: dict, ram_mb: int) -> bool:
    return rung["size_mb"] + rung["overhead_mb"] <= ram_mb - OS_MB


def eligible(index: int, ram_mb: int, generation: int) -> bool:
    rung = RUNGS[index]
    # The memory-mapped floor is also used on 512 MB Pis with a larger firmware reservation.
    return rung.get("min_generation", 1) <= max(1, generation) and (fits(rung, ram_mb) or rung["id"] == "smollm2-360m")


def choices(ram_mb: int, generation: int) -> list[int]:
    for menu in LADDER["menus"]:
        if menu.get("min_generation", 1) > max(generation, 1):
            continue
        indices = [BY_ID[model] for model in menu["models"]]
        if eligible(indices[0], ram_mb, generation):
            return [i for i in indices if eligible(i, ram_mb, generation)]
    raise ValueError("No model is available for this Pi.")


def pick(ram_mb: int, generation: int, users: str = "1", smarter: bool = False) -> int:
    menu = choices(ram_mb, generation)
    return menu[min(max(0, USER_STEP[users] - int(smarter)), len(menu) - 1)]


def context(index: int, ram_mb: int) -> int:
    """Tokens of conversation the server keeps. A board with spare memory doubles it."""
    rung = RUNGS[index]
    spare = ram_mb - OS_MB - rung["size_mb"] - rung["overhead_mb"]
    return rung["context"] * 2 if spare > 2 * rung["overhead_mb"] + 1024 else rung["context"]
