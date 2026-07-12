#!/usr/bin/env python3
"""Reproduce the independent WP-10B project fixture twice and compare bytes."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[2]
TOOL_ROOT = ROOT / "tools/project-fixtures"
DEFAULT_WORK_ROOT = ROOT / "target/mirante4d/fixture-candidates/project-store-v1"


class ReproductionError(RuntimeError):
    pass


def run(command: list[str]) -> None:
    result = subprocess.run(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=120,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise ReproductionError(f"command failed: {' '.join(command)}\n{detail}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--work-root", type=Path, default=DEFAULT_WORK_ROOT)
    args = parser.parse_args()
    work_root = args.work_root.resolve()
    work_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="reproduce-", dir=work_root) as temporary:
        temporary_root = Path(temporary)
        candidates = [temporary_root / "first", temporary_root / "second"]
        for candidate in candidates:
            run([sys.executable, str(TOOL_ROOT / "produce.py"), "--output", str(candidate)])
            run(
                [
                    sys.executable,
                    str(TOOL_ROOT / "validate.py"),
                    "--manifest",
                    str(candidate / "manifest.json"),
                ]
            )
        names = ["manifest.json", "project-store-v1.tar.gz"]
        for name in names:
            first = (candidates[0] / name).read_bytes()
            second = (candidates[1] / name).read_bytes()
            if first != second:
                raise ReproductionError(f"two-run reproduction differs: {name}")
        print(
            json.dumps(
                {
                    "archive_bytes": (candidates[0] / names[1]).stat().st_size,
                    "manifest_bytes": (candidates[0] / names[0]).stat().st_size,
                    "result": "byte-identical",
                    "runs": 2,
                },
                sort_keys=True,
                separators=(",", ":"),
            )
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
