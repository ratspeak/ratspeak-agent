"""Check the installer and agent against the tagged sources used by the release build."""
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "runtime"))
import common
import policy


def load(path):
    with (ROOT / path).open("rb") as source:
        return tomllib.load(source)


def check():
    native = load("runtime/native/Cargo.lock")["package"]
    desktop = load("app/src-tauri/Cargo.lock")["package"]
    dependencies = load("app/src-tauri/Cargo.toml")["dependencies"]
    for repo, key, prefix, required in (
        ("rsReticulum", "RETICULUM_REV", "rns-", "rns-runtime"),
        ("rsLXMF", "LXMF_REV", "lxmf-", "lxmf-core"),
    ):
        rev = getattr(common, key)
        if not re.fullmatch(r"[0-9a-f]{40}", rev) or getattr(policy, key) != rev:
            raise ValueError(f"{repo}: profile and invitation revisions must match")
        packages = {p["name"]: p["version"] for p in native if p["name"].startswith(prefix)}
        version = packages[required]
        tag = f"v{version}"
        released = subprocess.check_output(
            ["git", "-C", str(ROOT.parent / repo), "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
            text=True,
        ).strip()
        if released != rev:
            raise ValueError(f"{repo}: the agent pins {rev}, but {tag} points to {released}")

        url = f"https://github.com/ratspeak/{repo}"
        for name, spec in dependencies.items():
            if name.startswith(prefix) and not (
                isinstance(spec, dict) and spec.get("git") == url and spec.get("rev") == rev
            ):
                raise ValueError(f"{name}: installer manifest must pin {repo} at {rev}")
        for package in desktop:
            name = package["name"]
            if name.startswith(prefix) and (
                package.get("source") != f"git+{url}?rev={rev}#{rev}"
                or package["version"] != packages.get(name)
            ):
                raise ValueError(f"{name}: installer and agent lockfiles disagree")
        print(f"{repo} {tag} {rev}")
    # rsLXST only decodes voice notes in the helper; nothing else pins it.
    version = {p["name"]: p["version"] for p in native}["lxst-core"]
    released = subprocess.check_output(
        ["git", "-C", str(ROOT.parent / "rsLXST"), "rev-parse", "--verify", f"refs/tags/v{version}^{{commit}}"], text=True).strip()
    if released != common.LXST_REV:
        raise ValueError(f"rsLXST: the agent pins {common.LXST_REV}, but v{version} points to {released}")
    print(f"rsLXST v{version} {released}")


if __name__ == "__main__":
    try:
        check()
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"Upstream pin check failed: {error}")
