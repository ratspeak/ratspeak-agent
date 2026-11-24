"""Assemble GitHub release assets from the CI platform builds."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import tomllib
from urllib.parse import quote


def main():
    version = tomllib.loads(Path("app/src-tauri/Cargo.toml").read_text())["package"]["version"]
    tag = f"v{version}"
    if os.environ["GITHUB_REF_TYPE"] == "tag" and os.environ["GITHUB_REF_NAME"] != tag:
        raise ValueError(f"Release tag does not match {tag}")
    signed = os.environ["SIGN_UPDATES"] == "true"
    artifacts = Path("artifacts")
    release = Path("release")
    release.mkdir()

    def collect(pattern):
        files = list(artifacts.rglob(pattern))
        if len(files) != 1:
            raise ValueError(f"Expected one artifact matching {pattern}, found {len(files)}")
        source = files[0]
        destination = release / source.name.replace(" ", "-")
        shutil.copyfile(source, destination)
        return source, destination

    collect("*.dmg")
    _, pi = collect(f"ratspeak-pi-{version}-aarch64.tar.gz")
    packages = {
        "windows-x86_64-nsis": collect("*.exe"),
        "linux-x86_64-deb": collect("*.deb"),
        "linux-x86_64-rpm": collect("*.rpm"),
    }
    if signed:
        packages["darwin-aarch64-app"] = collect("*.app.tar.gz")
    signatures = [source.with_name(f"{source.name}.sig").is_file() for source, _ in packages.values()]
    if any(signatures) and not all(signatures):
        raise ValueError("Updater manifest requires signatures for every platform")
    if signed and all(signatures):
        base = f"https://github.com/{os.environ['GITHUB_REPOSITORY']}/releases/download/{tag}"
        platforms = {}
        for target, (source, destination) in packages.items():
            signature = source.with_name(f"{source.name}.sig").read_text().strip()
            if not signature:
                raise ValueError(f"Empty updater signature for {source.name}")
            (release / f"{destination.name}.sig").write_text(f"{signature}\n")
            platforms[target] = {"url": f"{base}/{quote(destination.name)}", "signature": signature}
        (release / "latest.json").write_text(json.dumps({"version": version, "platforms": platforms}, indent=2) + "\n")

    pi.with_name(f"{pi.name}.sha256").write_text(checksum(pi))
    (release / "SHA256SUMS").write_text("".join(checksum(path) for path in sorted(release.iterdir())))


def checksum(path):
    with path.open("rb") as payload:
        digest = hashlib.file_digest(payload, "sha256").hexdigest()
    return f"{digest}  {path.name}\n"


if __name__ == "__main__":
    main()
