"""Portable cleanup commands used by the root Makefile."""

import argparse
from pathlib import Path
import shutil
import subprocess


def clean() -> None:
    root = Path(__file__).resolve().parent.parent
    for relative in ("talker/.venv", "console/node_modules", "console/dist"):
        target = root / relative
        # Refuse symlink/junction escapes before recursively deleting build output.
        if not target.resolve().is_relative_to(root):
            raise ValueError(f"cleanup target is outside the repository: {target}")
        if target.exists():
            shutil.rmtree(target)


def sandbox_clean() -> None:
    result = subprocess.run(
        ["docker", "ps", "-aq", "--filter", "label=kaguya.sandbox=1"],
        check=True,
        capture_output=True,
        text=True,
    )
    containers = result.stdout.split()
    if containers:
        subprocess.run(["docker", "rm", "-f", *containers], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("clean", "sandbox-clean"))
    args = parser.parse_args()
    {"clean": clean, "sandbox-clean": sandbox_clean}[args.command]()
