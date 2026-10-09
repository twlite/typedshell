#!/usr/bin/env python3
"""Override the local package and lockfile versions for a release build."""

import re
import sys
from pathlib import Path


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: set-version.py VERSION")
    version = sys.argv[1]
    number = r"(?:0|[1-9][0-9]*)"
    prerelease_identifier = rf"(?:{number}|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
    version_pattern = re.compile(
        rf"{number}\.{number}\.{number}"
        rf"(?:-{prerelease_identifier}(?:\.{prerelease_identifier})*)?"
    )
    if not version_pattern.fullmatch(version):
        raise SystemExit("version must be valid SemVer without build metadata")

    manifest_path = Path("Cargo.toml")
    manifest_lines = manifest_path.read_text(encoding="utf-8").splitlines()
    section = None
    manifest_updated = False
    for index, line in enumerate(manifest_lines):
        if line.startswith("[") and line.endswith("]"):
            section = line
        if section == "[package]" and re.match(r"^version\s*=", line):
            manifest_lines[index] = f'version = "{version}"'
            manifest_updated = True
            break
    if not manifest_updated:
        raise SystemExit("could not find [package].version in Cargo.toml")
    manifest_path.write_text("\n".join(manifest_lines) + "\n", encoding="utf-8")

    lock_path = Path("Cargo.lock")
    lock_text = lock_path.read_text(encoding="utf-8")
    package_version = re.compile(
        r'(?m)(^\[\[package\]\]\nname = "typedshell"\nversion = )"[^"]+"'
    )
    lock_text, replacements = package_version.subn(
        lambda match: f'{match.group(1)}"{version}"', lock_text
    )
    if replacements != 1:
        raise SystemExit("could not update the typedshell entry in Cargo.lock")
    lock_path.write_text(lock_text, encoding="utf-8")


if __name__ == "__main__":
    main()
