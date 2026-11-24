"""Private profile and bounded local-file utilities for Agent Rat."""
from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

VERSION = "0.7.0"
RETICULUM_REV = "5e35c9f9d9f40d44846aaa5e11009e03490511d3"
LXMF_REV = "70fa2242119857c63c2132ec0e36d731dd364d39"
LXST_REV = "b39e23cbec76fe51cf7f7b58bd4514b0c6b4ea0d"     # rsLXST v0.4.0: voice note decoding only
# Profiles from these releases open here; the 1.5 stores import 1.4.0 files on first start.
UPGRADABLE_REVS = {
    ("14241a383910e6ee7665303eff1e7c8d98957cc0", "00f439df7e6d14d29e5ca75efc7c694ab4990b3d"),  # v1.4.0
    ("5976f6bccef81851d3186e889e5924665283f21c", "b21732352bcf72b7ca955bbe4cab9ea95d64c6fc"),  # v1.5.0
}


def private_directory(path: Path) -> Path:
    path = path.expanduser().absolute()
    if path.is_symlink():
        raise ValueError("A profile directory cannot be a symbolic link")
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    info = path.stat()
    if info.st_uid != os.getuid() or not stat.S_ISDIR(info.st_mode):
        raise ValueError("Profile must be a directory owned by the current user")
    os.chmod(path, 0o700)
    return path


def read_json(path: Path, maximum: int = 65536):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError(f"Expected an owned regular file: {path.name}")
    if info.st_size > maximum:
        raise ValueError(f"File is too large: {path.name}")
    with path.open("r", encoding="utf-8") as source:
        def unique(pairs):
            result = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError(f"Duplicate JSON field: {key}")
                result[key] = value
            return result
        return json.load(source, object_pairs_hook=unique)


def atomic_json(path: Path, value):
    if path.is_symlink():
        raise ValueError("Refusing to replace a symbolic link")
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    with os.fdopen(fd, "w", encoding="utf-8") as target:
        os.fchmod(target.fileno(), 0o600)
        json.dump(value, target, ensure_ascii=False, indent=2)
        target.write("\n")
        target.flush()
        os.fsync(target.fileno())
    os.replace(temporary, path)


def atomic_text(path: Path, value: str):
    if path.is_symlink():
        raise ValueError("Refusing to replace a symbolic link")
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    with os.fdopen(fd, "w", encoding="utf-8") as target:
        os.fchmod(target.fileno(), 0o600)
        target.write(value)
        target.flush()
        os.fsync(target.fileno())
    os.replace(temporary, path)


def validate_label(value: str) -> str:
    value = value.strip()
    if not value or len(value) > 40 or any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ValueError("Choose a label of 1–40 characters without control characters")
    return value


def profile_manifest(profile: Path):
    result = read_json(profile / "manifest.json")
    if result.get("schema") != 1 or result.get("product") != "agent-rat":
        raise ValueError("Unrecognized profile: select an existing maintained-helper profile or prepare a new directory; existing files were preserved")
    revisions = (result.get("reticulum_rev"), result.get("lxmf_rev"))
    if revisions != (RETICULUM_REV, LXMF_REV) and revisions not in UPGRADABLE_REVS:
        raise ValueError("Profile source versions differ; an explicit supported migration is required")
    return result


def upgrade_profile(profile: Path):
    """Record the current sources before the receiver starts, so older helpers refuse the migrated stores."""
    manifest = profile_manifest(profile)
    if (manifest["reticulum_rev"], manifest["lxmf_rev"]) != (RETICULUM_REV, LXMF_REV):
        manifest.update(reticulum_rev=RETICULUM_REV, lxmf_rev=LXMF_REV)
        atomic_json(profile / "manifest.json", manifest)
    return manifest


def native_identity(binary: str, identity: Path, create: bool = False):
    command = [binary, "identity", "--identity", str(identity)]
    if create:
        command.append("--create")
    process = subprocess.run(command, capture_output=True, text=True, timeout=20)
    if process.returncode:
        raise RuntimeError(f"Native identity check failed: {process.stderr.strip()[:500]}")
    value = json.loads(process.stdout)
    if not re.fullmatch(r"[0-9a-f]{32}", value.get("address", "")) or not re.fullmatch(r"[0-9a-f]{128}", value.get("public_key", "")):
        raise ValueError("Native identity command returned invalid public metadata")
    return value


def prepare_profile(profile: Path, name: str, binary: str):
    name = validate_label(name)
    binary_path = Path(binary).expanduser().resolve(strict=True)
    if not binary_path.is_file() or not os.access(binary_path, os.X_OK):
        raise ValueError("Native helper must be an executable file")
    if profile.exists() and not (profile / "manifest.json").exists() and any(item.name != ".controller.lock" for item in profile.iterdir()):
        raise ValueError("This directory already has unrelated or legacy data. Choose a new directory; nothing was overwritten")
    profile = private_directory(profile)
    manifest_path = profile / "manifest.json"
    if manifest_path.exists():
        manifest = profile_manifest(profile)
        if manifest.get("retired"):
            raise ValueError("This profile is retired; it must not silently resume")
        return manifest
    private_directory(profile / "reticulum")
    private_directory(profile / "invitations")
    identity = native_identity(str(binary_path), profile / "identity", create=True)
    manifest = {"schema": 1, "product": "agent-rat", "version": VERSION,
                "profile_id": str(uuid.uuid4()), "label": name, "created_at": time.time(),
                "reticulum_rev": RETICULUM_REV, "lxmf_rev": LXMF_REV,
                "native": str(binary_path), "helper": str(Path(__file__).with_name("agent_rat.py").resolve()),
                "address": identity["address"], "public_key": identity["public_key"],
                "paused": True, "retired": False, "interfaces": []}
    atomic_text(profile / "reticulum" / "config", "[reticulum]\nshare_instance = no\nenable_transport = no\n\n[interfaces]\n")
    atomic_json(manifest_path, manifest)
    return manifest


def public_card(manifest, state="Prepared; not connected"):
    return {"name": manifest["label"], "address": manifest["address"],
            "public_key": manifest["public_key"], "status": state}
