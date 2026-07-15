#!/usr/bin/env python3
"""Reproduce the bounded WP-10A-C external-reader capability probe."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[3]
PROBE_ROOT = Path(__file__).resolve().parent
PRODUCER = PROBE_ROOT / "producer.py"
READER = PROBE_ROOT / "reader.py"
LOCK = PROBE_ROOT / "requirements-linux-x86_64-py312.lock"
CHECKED_REPORT = PROBE_ROOT / "report.json"
EVIDENCE_ROOT = ROOT / "target" / "mirante4d" / "wp10a-c"
WORK_ROOT = EVIDENCE_ROOT / "reader-probe"
PYTHON = Path("/usr/bin/python3.12")
ZSTD = Path("/usr/bin/zstd")
EXPECTED_PYTHON_SHA256 = "1643dacd9feaedc58f3cc581e4d22577dfe25c09b10282936186ccf0f2e61118"
EXPECTED_ZSTD_SHA256 = "7c5468b370f7c47eda07281e3437fafc568f95d10420051e3aa522709f9342c5"
EXPECTED_UV_SHA256 = "13b335cfb84d5ec0a649ce071d6eb7c1e81496412caf9646f75434049da9d85c"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def run_text(command: list[str], *, environment: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        command,
        cwd=ROOT,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
        timeout=120,
    )
    return result.stdout.strip()


def run_json(command: list[str]) -> dict[str, object]:
    value = json.loads(run_text(command))
    if not isinstance(value, dict):
        raise ValueError("probe command did not return one JSON object")
    return value


def verify_tools() -> tuple[Path, dict[str, object]]:
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("reader probe is locked to Linux x86_64")
    uv_name = shutil.which("uv")
    if uv_name is None:
        raise ValueError("uv is required to install the hash-locked reader")
    uv = Path(uv_name).resolve()
    tools = {
        "platform": "Linux x86_64",
        "python": {
            "version": run_text([str(PYTHON), "--version"]),
            "sha256": sha256_file(PYTHON),
        },
        "zstd": {
            "version": run_text([str(ZSTD), "--version"]),
            "sha256": sha256_file(ZSTD),
        },
        "uv": {
            "version": run_text([str(uv), "--version"]),
            "sha256": sha256_file(uv),
        },
    }
    if tools["python"] != {"version": "Python 3.12.3", "sha256": EXPECTED_PYTHON_SHA256}:
        raise ValueError("Python tool does not match the accepted probe environment")
    zstd = tools["zstd"]
    if not isinstance(zstd, dict) or "v1.5.5" not in str(zstd["version"]):
        raise ValueError("zstd version does not match the accepted probe environment")
    if zstd["sha256"] != EXPECTED_ZSTD_SHA256:
        raise ValueError("zstd binary does not match the accepted probe environment")
    if tools["uv"] != {"version": "uv 0.9.27", "sha256": EXPECTED_UV_SHA256}:
        raise ValueError("uv tool does not match the accepted probe environment")
    return uv, tools


def reproduce(work_root: Path) -> dict[str, object]:
    uv, tools = verify_tools()
    shutil.rmtree(work_root, ignore_errors=True)
    work_root.mkdir(parents=True)
    run_a = work_root / "run-a.zarr"
    run_b = work_root / "run-b.zarr"
    producer_a = run_json([str(PYTHON), str(PRODUCER), str(run_a)])
    producer_b = run_json([str(PYTHON), str(PRODUCER), str(run_b)])
    if producer_a != producer_b:
        raise ValueError("two clean producer runs did not yield identical bytes")
    expected_producer = {
        "metadata_bytes": 629,
        "metadata_sha256": "cb28e21165ee80e5fdd9bed0f806837e97f4fe04babfcf7b7758f3fbbdd9b8c4",
        "shard_bytes": 306,
        "shard_sha256": "0afe3fd3c86fc01d25a755bfeec85e986fda2380ac3fc73b802fc4e092460f25",
        "tree_sha256": "6a773ec88f1ea70ca30020bcbca6fa78855ce41ffde7a50cdaf84af7f3d9a4c0",
    }
    if producer_a != expected_producer:
        raise ValueError(f"producer output drifted: {producer_a!r}")

    environment = dict(os.environ)
    environment["UV_NO_CACHE"] = "1"
    venv = work_root / "reader-venv"
    run_text([str(uv), "venv", "--clear", "--python", str(PYTHON), str(venv)], environment=environment)
    reader_python = venv / "bin" / "python"
    run_text(
        [
            str(uv),
            "pip",
            "install",
            "--python",
            str(reader_python),
            "--require-hashes",
            "-r",
            str(LOCK),
        ],
        environment=environment,
    )
    reader_a = run_json([str(reader_python), str(READER), str(run_a)])
    reader_b = run_json([str(reader_python), str(READER), str(run_b)])
    if reader_a != reader_b or reader_a.get("result") != "PASS":
        raise ValueError("external reader observations were not deterministic passes")

    return {
        "schema": "mirante4d-wp10a-c-reader-probe",
        "schema_version": 1,
        "status": "diagnostic-pass",
        "case": "hand-built-2d-uint8-selected-zarr-shard-subset",
        "standards_manifest_sha256": sha256_file(
            ROOT / "architecture" / "wp10a-normative-standards.json"
        ),
        "sources": {
            "producer_sha256": sha256_file(PRODUCER),
            "reader_sha256": sha256_file(READER),
            "reproducer_sha256": sha256_file(Path(__file__)),
            "requirements_lock_sha256": sha256_file(LOCK),
        },
        "reader": {
            "name": "zarr-python",
            "version": "3.2.1",
            "source_tag": "v3.2.1",
            "source_commit": "85890b3bb404fd1d401267c508a2694f5734559e",
            "resolved_versions": [
                "donfig 0.8.1.post1",
                "google-crc32c 1.8.0",
                "numcodecs 0.16.5",
                "numpy 2.5.1",
                "packaging 26.2",
                "PyYAML 6.0.3",
                "typing-extensions 4.16.0",
                "zarr 3.2.1",
            ],
        },
        "tools": tools,
        "producer": producer_a,
        "observed": reader_a,
        "result": "PASS",
        "non_claims": [
            "not a T1 fixture or authority promotion",
            "not OME metadata or official-schema evidence",
            "not a complete M4D package, IO-3, identity, product, or performance claim",
            "not evidence for other data types, validity arrays, transforms, or multiple outer shards",
        ],
    }


def checked_work_root() -> Path:
    for path in [
        ROOT / "target",
        ROOT / "target" / "mirante4d",
        EVIDENCE_ROOT,
        WORK_ROOT,
    ]:
        if path.is_symlink():
            raise ValueError(f"probe work path cannot be a symlink: {path.relative_to(ROOT)}")
    resolved_root = EVIDENCE_ROOT.resolve()
    resolved_work = WORK_ROOT.resolve()
    if resolved_work.parent != resolved_root or resolved_work.name != "reader-probe":
        raise ValueError("probe work directory escaped its single owned evidence root")
    return resolved_work


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write-report", action="store_true")
    arguments = parser.parse_args()
    report = reproduce(checked_work_root())
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if arguments.write_report:
        CHECKED_REPORT.write_text(encoded, encoding="utf-8")
    elif not CHECKED_REPORT.is_file() or CHECKED_REPORT.read_text(encoding="utf-8") != encoded:
        raise ValueError("checked reader-probe report is absent or stale")
    print("WP-10A-C external-reader probe: deterministic PASS")


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        ValueError,
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
        json.JSONDecodeError,
    ) as error:
        print(f"reader probe failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
