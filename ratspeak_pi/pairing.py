"""Install the identity printed in desktop Setup before the receiver starts."""
import os
import shutil
import sys
import tempfile
from pathlib import Path

from . import units


def provision(boot: Path, expected: str, name: str, services: bool, replacement: bytes | None = None) -> None:
    sys.path.insert(0, str(units.PREFIX / "runtime"))
    from common import atomic_json, native_identity, prepare_profile

    profile = units.PROFILE
    seed = boot / "identity.seed"
    binary = str(units.PREFIX / "bin" / "ratspeak-pi-native")

    def verify(path):
        meta = native_identity(binary, path)
        if f"lxma://{meta['address']}:{meta['public_key']}" != expected:
            raise RuntimeError("The card identity does not match the pairing QR.")
        return meta

    if (profile / "manifest.json").is_file():
        verify(profile / "identity")
    else:
        material = seed.read_bytes() if replacement is None else replacement
        if len(material) != 64:
            raise RuntimeError("The pairing key on the card is incomplete.")
        # Assemble an unpublished helper profile. Only its final identity ever reaches the inbox database.
        with tempfile.TemporaryDirectory(dir=profile.parent) as parent:
            staged = Path(parent) / "profile"
            manifest = prepare_profile(staged, name, binary)
            key = staged / "identity"
            key.write_bytes(material)
            os.chmod(key, 0o600)
            manifest.update(verify(key))
            atomic_json(staged / "manifest.json", manifest)
            if services:
                for path in [staged, *staged.rglob("*")]:
                    shutil.chown(path, user=units.USER, group=units.USER)
            staged.rename(profile)
    seed.unlink(missing_ok=True)
