"""What board this is: model name, CPU generation, memory, cores. Reads /proc and the device tree only."""
from __future__ import annotations

import os
import platform
import re
from dataclasses import dataclass
from pathlib import Path

# CPU part numbers from /proc/cpuinfo ("CPU part"), mapped to how fast the core is for inference.
# 3 = Cortex-A76 (Pi 5 class), 2 = Cortex-A72 (Pi 4 class), 1 = Cortex-A53 (Pi 3, Zero 2 W).
CPU_PARTS = {"0xd0b": 3, "0xd08": 2, "0xd03": 1}


@dataclass(frozen=True)
class Board:
    model: str        # "Raspberry Pi 5 Model B Rev 1.0", or "" off a Pi
    generation: int   # 1-3, see CPU_PARTS; 0 = unknown CPU
    ram_mb: int       # MemTotal, which is a little under the board's nominal size
    cores: int
    arch: str         # "aarch64" is the only one supported

    @property
    def supported(self) -> bool:
        return self.arch == "aarch64" and self.ram_mb > 0


def parse_meminfo(text: str) -> int:
    match = re.search(r"^MemTotal:\s+(\d+)\s+kB", text, re.M)
    return int(match.group(1)) // 1024 if match else 0


def parse_generation(cpuinfo: str) -> int:
    parts = re.findall(r"^CPU part\s*:\s*(0x[0-9a-f]+)", cpuinfo, re.M)
    return max((CPU_PARTS.get(part, 0) for part in parts), default=0)


def detect(root: Path = Path("/")) -> Board:
    def read(path: str) -> str:
        try:
            return (root / path).read_text(errors="replace")
        except OSError:
            return ""

    return Board(model=read("proc/device-tree/model").strip("\x00\n "),
                 generation=parse_generation(read("proc/cpuinfo")),
                 ram_mb=parse_meminfo(read("proc/meminfo")),
                 cores=os.cpu_count() or 1,
                 arch=platform.machine())
