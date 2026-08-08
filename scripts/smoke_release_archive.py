#!/usr/bin/env python3
"""Unpack a release archive and verify the packaged binary's --version output."""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()

    if not args.archive.is_file():
        print(f"archive does not exist: {args.archive}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="bingo-release-smoke-") as temp:
        root = Path(temp)
        try:
            shutil.unpack_archive(args.archive, root)
        except (OSError, shutil.ReadError, ValueError) as error:
            print(f"cannot unpack {args.archive}: {error}", file=sys.stderr)
            return 1

        names = {"bingo", "bingo.exe"}
        binaries = [path for path in root.rglob("*") if path.is_file() and path.name in names]
        if len(binaries) != 1:
            found = ", ".join(str(path.relative_to(root)) for path in binaries) or "none"
            print(f"archive must contain exactly one bingo binary; found: {found}", file=sys.stderr)
            return 1

        binary = binaries[0]
        try:
            completed = subprocess.run(
                [str(binary), "--version"],
                check=False,
                capture_output=True,
                text=True,
                timeout=30,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"cannot execute unpacked binary: {error}", file=sys.stderr)
            return 1

        expected = f"bingo {args.version}"
        actual = completed.stdout.strip()
        if completed.returncode != 0 or actual != expected or completed.stderr:
            print(
                "archive smoke failed: "
                f"exit={completed.returncode}, stdout={actual!r}, stderr={completed.stderr!r}; "
                f"expected stdout {expected!r}",
                file=sys.stderr,
            )
            return 1

        print(f"archive smoke verified: {args.archive} -> {expected}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
