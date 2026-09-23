from __future__ import annotations

import argparse
import re
from decimal import Decimal
from pathlib import Path


SCRIPT_PATH = Path("main.py")
VERSION_PATTERN = re.compile(r'^SCRIPT_VERSION = "(\d+\.\d)"$', re.MULTILINE)
VALID_VERSION = re.compile(r"\d+\.\d")


def read_version(path: Path = SCRIPT_PATH) -> str:
    matches = VERSION_PATTERN.findall(path.read_text(encoding="utf-8"))
    if len(matches) != 1:
        raise ValueError(f"Expected exactly one numeric SCRIPT_VERSION in {path}")
    return matches[0]


def next_version(latest_release: str = "", path: Path = SCRIPT_PATH) -> str:
    current = read_version(path)
    if latest_release and not VALID_VERSION.fullmatch(latest_release):
        raise ValueError(f"Latest release tag is not numeric: {latest_release!r}")
    baseline = max(Decimal(current), Decimal(latest_release or current))
    return f"{baseline + Decimal('0.1'):.1f}"


def write_version(version: str, path: Path = SCRIPT_PATH) -> None:
    if not VALID_VERSION.fullmatch(version):
        raise ValueError(f"Release version is not numeric: {version!r}")
    source = path.read_text(encoding="utf-8")
    updated, count = VERSION_PATTERN.subn(f'SCRIPT_VERSION = "{version}"', source)
    if count != 1:
        raise ValueError(f"Expected exactly one numeric SCRIPT_VERSION in {path}")
    path.write_text(updated, encoding="utf-8", newline="\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("current", "next", "set"))
    parser.add_argument("value", nargs="?", default="")
    args = parser.parse_args()
    if args.command == "current":
        print(read_version())
    elif args.command == "next":
        print(next_version(args.value))
    else:
        if not args.value:
            parser.error("set requires a version")
        write_version(args.value)


if __name__ == "__main__":
    main()
