#!/usr/bin/env python3
"""Verify that a release tag exactly matches Cargo's package version."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys
import tomllib

TAG_PATTERN = re.compile(r"v(?P<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)")


def package_version(manifest: Path) -> str:
    with manifest.open("rb") as file:
        data = tomllib.load(file)
    version = data.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        raise ValueError(f"{manifest} has no [workspace.package].version")
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True, help="release tag, for example v0.4.2")
    parser.add_argument("--manifest", type=Path, default=Path("Cargo.toml"))
    args = parser.parse_args()

    match = TAG_PATTERN.fullmatch(args.tag)
    if match is None:
        print(f"release tag must be v<semver>, got {args.tag!r}", file=sys.stderr)
        return 1

    try:
        cargo_version = package_version(args.manifest)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"cannot read package version: {error}", file=sys.stderr)
        return 1

    tag_version = match.group("version")
    if tag_version != cargo_version:
        print(
            f"tag {args.tag!r} does not match {args.manifest}'s version {cargo_version!r}",
            file=sys.stderr,
        )
        return 1

    print(f"tag {args.tag!r} matches {args.manifest}'s version {cargo_version!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
