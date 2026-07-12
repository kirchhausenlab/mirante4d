#!/usr/bin/env python3
"""Fetch once and verify the exact offline WP-10A standards artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import urllib.request


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "architecture" / "wp10a-normative-standards.json"
STANDARDS_ROOT = ROOT / "verification" / "standards"
MAX_ARTIFACTS = 64
MAX_ARTIFACT_BYTES = 2 * 1024 * 1024


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def require_relative_standard_path(value: object) -> Path:
    if not isinstance(value, str):
        raise ValueError("artifact local_path must be a string")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"unsafe artifact path: {value!r}")
    if relative.parts[:2] != ("verification", "standards"):
        raise ValueError(f"artifact is outside verification/standards: {value!r}")
    return relative


def load_manifest(path: Path) -> tuple[dict[str, object], list[dict[str, object]]]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict):
        raise ValueError("standards manifest must be an object")
    if document.get("schema") != "mirante4d-wp10a-normative-standards":
        raise ValueError("unexpected standards manifest schema")
    if document.get("schema_version") != 1 or document.get("status") != "accepted-off-product":
        raise ValueError("standards manifest version or status is not accepted")

    entry = document.get("entry")
    if not isinstance(entry, dict):
        raise ValueError("standards manifest entry binding is absent")
    entry_path = ROOT / str(entry.get("path", ""))
    if not entry_path.is_file() or sha256_file(entry_path) != entry.get("sha256"):
        raise ValueError("WP-10A-C entry binding does not match")

    sources = document.get("sources")
    if not isinstance(sources, list) or not sources:
        raise ValueError("standards manifest has no sources")
    source_ids: set[str] = set()
    artifacts: list[dict[str, object]] = []
    for source in sources:
        if not isinstance(source, dict):
            raise ValueError("standards source must be an object")
        source_id = source.get("id")
        commit = source.get("commit")
        if not isinstance(source_id, str) or source_id in source_ids:
            raise ValueError("standards source id is empty or duplicated")
        if not isinstance(commit, str) or len(commit) != 40:
            raise ValueError(f"standards source {source_id!r} lacks an exact commit")
        source_ids.add(source_id)
        rows = source.get("artifacts")
        if not isinstance(rows, list) or not rows:
            raise ValueError(f"standards source {source_id!r} has no artifacts")
        for artifact in rows:
            if not isinstance(artifact, dict):
                raise ValueError("standards artifact must be an object")
            url = artifact.get("url")
            if not isinstance(url, str) or commit not in url:
                raise ValueError(f"artifact URL is not bound to {source_id!r} commit")
            artifacts.append(artifact)

    if len(artifacts) > MAX_ARTIFACTS:
        raise ValueError("standards artifact count exceeds the fixed bound")
    return document, artifacts


def fetch_artifact(artifact: dict[str, object]) -> None:
    relative = require_relative_standard_path(artifact.get("local_path"))
    destination = ROOT / relative
    expected_bytes = artifact.get("bytes")
    expected_sha256 = artifact.get("sha256")
    url = artifact.get("url")
    if not isinstance(expected_bytes, int) or not 0 < expected_bytes <= MAX_ARTIFACT_BYTES:
        raise ValueError(f"invalid byte bound for {relative}")
    if not isinstance(expected_sha256, str) or len(expected_sha256) != 64:
        raise ValueError(f"invalid SHA-256 for {relative}")
    if not isinstance(url, str):
        raise ValueError(f"invalid URL for {relative}")

    request = urllib.request.Request(url, headers={"User-Agent": "mirante4d-standards-fetch/1"})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read(expected_bytes + 1)
    if len(data) != expected_bytes or sha256_bytes(data) != expected_sha256:
        raise ValueError(f"downloaded bytes do not match the manifest for {relative}")

    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
            temporary.write(data)
            temporary.flush()
            os.fsync(temporary.fileno())
            temporary_name = temporary.name
        os.chmod(temporary_name, 0o644)
        os.replace(temporary_name, destination)
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def verify_artifacts(artifacts: list[dict[str, object]]) -> None:
    expected_paths: set[Path] = set()
    total_bytes = 0
    for artifact in artifacts:
        relative = require_relative_standard_path(artifact.get("local_path"))
        if relative in expected_paths:
            raise ValueError(f"duplicate standards artifact path: {relative}")
        expected_paths.add(relative)
        path = ROOT / relative
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError(f"standards artifact is not one regular unlinked file: {relative}")
        expected_bytes = artifact.get("bytes")
        expected_sha256 = artifact.get("sha256")
        if metadata.st_size != expected_bytes or sha256_file(path) != expected_sha256:
            raise ValueError(f"standards artifact does not match its manifest: {relative}")
        total_bytes += metadata.st_size

    actual_paths = {
        path.relative_to(ROOT)
        for path in STANDARDS_ROOT.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if actual_paths != expected_paths:
        missing = sorted(map(str, expected_paths - actual_paths))
        extra = sorted(map(str, actual_paths - expected_paths))
        raise ValueError(f"standards inventory mismatch: missing={missing}, extra={extra}")
    if total_bytes > MAX_ARTIFACT_BYTES:
        raise ValueError("combined standards bytes exceed the fixed bound")
    print(f"WP-10A standards: {len(expected_paths)} artifacts, {total_bytes} bytes, exact digests passed")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--fetch", action="store_true")
    arguments = parser.parse_args()
    manifest = arguments.manifest.resolve()
    if not manifest.is_relative_to(ROOT):
        raise ValueError("manifest must remain inside the repository")
    _, artifacts = load_manifest(manifest)
    if arguments.fetch:
        for artifact in artifacts:
            fetch_artifact(artifact)
    verify_artifacts(artifacts)


if __name__ == "__main__":
    main()
