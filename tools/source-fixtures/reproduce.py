#!/usr/bin/env python3
"""Reproduce the approved public source-fixture archive twice."""

from __future__ import annotations

import argparse
import ast
import csv
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path
from typing import Any


PYTHON = Path("/usr/bin/python3.12")
PYTHON_SHA256 = "1643dacd9feaedc58f3cc581e4d22577dfe25c09b10282936186ccf0f2e61118"
RUSTC_SHA256 = "4a84e05991ad6f2a84c1361c29b52b38c390365bd1fc269b1936c88d482a8928"
RUSTC_RELEASE_SHA256 = "3545a0efad2355ecb0a3b9ac02efee96e27f1f9d24b7ce2fc3f279b2efb0d923"
SPEC_IDS = [f"SRC-TIFF-SPEC-{number:03d}" for number in range(1, 5)]
ARCHIVE_NAME = "mirante4d-source-tiff-fixtures-v1.tar"


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run(command: list[str], *, env: dict[str, str] | None = None) -> None:
    subprocess.run(command, check=True, env=env)


def verify_runtime() -> None:
    if sys.version_info[:3] != (3, 12, 3) or Path(sys.executable).resolve() != PYTHON:
        raise SystemExit(f"run this command with {PYTHON}")
    if sha256_file(PYTHON) != PYTHON_SHA256:
        raise SystemExit("the approved CPython binary hash does not match")
    rustc_command = shutil.which("rustc")
    rustup = shutil.which("rustup")
    if rustc_command is None or rustup is None:
        raise SystemExit("the approved Rust 1.96.1 toolchain is required")
    rustc = subprocess.check_output([rustup, "which", "rustc"], text=True).strip()
    if sha256_file(Path(rustc)) != RUSTC_SHA256:
        raise SystemExit("the approved Rust 1.96.1 rustc executable is required")
    version = subprocess.check_output([rustc, "--version"], text=True).strip()
    if version != "rustc 1.96.1 (31fca3adb 2026-06-26)":
        raise SystemExit(f"unexpected Rust identity: {version}")


def fetch(url: str, expected_sha256: str, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(url, headers={"User-Agent": "Mirante4D-fixture-reproducer/1"})
    with urllib.request.urlopen(request, timeout=60) as response:
        data = response.read()
    if sha256_bytes(data) != expected_sha256:
        raise RuntimeError(f"download hash mismatch for {url}")
    destination.write_bytes(data)


def safe_extract_wheel(wheel: Path, destination: Path) -> str:
    entries: list[dict[str, Any]] = []
    with zipfile.ZipFile(wheel) as archive:
        for info in sorted(archive.infolist(), key=lambda item: item.filename):
            parts = Path(info.filename).parts
            if info.is_dir():
                continue
            if not parts or info.filename.startswith("/") or ".." in parts or "\\" in info.filename:
                raise RuntimeError(f"unsafe wheel member {info.filename!r}")
            data = archive.read(info)
            target = destination.joinpath(*parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            entries.append({"path": info.filename, "bytes": len(data), "sha256": sha256_bytes(data)})
    return sha256_bytes(canonical_json(entries))


def load_lock(path: Path) -> dict[str, Any]:
    lock = json.loads(path.read_text(encoding="utf-8"))
    if lock.get("schema") != "mirante4d-source-fixture-python-wheel-lock":
        raise RuntimeError(f"unexpected lock schema: {path}")
    if lock["python"] != {
        "version": "3.12.3",
        "ubuntu_revision": "3.12.3-1ubuntu0.15",
        "binary": "/usr/bin/python3.12",
        "binary_sha256": PYTHON_SHA256,
    }:
        raise RuntimeError(f"unapproved Python lock identity: {path}")
    return lock


def prepare_python_lineage(lock_path: Path, scratch: Path) -> tuple[Path, dict[str, str]]:
    lock = load_lock(lock_path)
    site = scratch / "site"
    installed: dict[str, str] = {}
    for wheel in lock["wheels"]:
        if wheel["name"] == "OME-XML-XSD":
            continue
        downloaded = scratch / "downloads" / wheel["filename"]
        fetch(wheel["url"], wheel["sha256"], downloaded)
        installed[wheel["name"]] = safe_extract_wheel(downloaded, site)
    return site, installed


def python_env(site: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "LC_ALL": "C.UTF-8",
            "TZ": "UTC",
            "PYTHONPATH": str(site),
            "PYTHONNOUSERSITE": "1",
            "PYTHONDONTWRITEBYTECODE": "1",
        }
    )
    return env


def parse_spec_paths(spec: Path) -> list[str]:
    with spec.open(newline="", encoding="utf-8") as source:
        rows = list(csv.DictReader(source, delimiter="|"))
    paths = sorted(row["path"] for row in rows if row["kind"] == "file")
    if len(paths) != 16 or len(paths) != len(set(paths)):
        raise RuntimeError("approved specification must name exactly 16 unique files")
    return paths


def first_ifd_and_strip(encoded: bytes) -> tuple[int, int, int]:
    if encoded[:4] != b"II*\0":
        raise RuntimeError("mutation base is not little-endian classic TIFF")
    ifd = struct.unpack_from("<I", encoded, 4)[0]
    count = struct.unpack_from("<H", encoded, ifd)[0]
    tags: dict[int, tuple[int, int, int]] = {}
    for index in range(count):
        tag, kind, length, value = struct.unpack_from("<HHII", encoded, ifd + 2 + index * 12)
        tags[tag] = (kind, length, value)
    if 273 not in tags or 279 not in tags:
        raise RuntimeError("mutation base lacks strip tags")
    offset = tags[273][2]
    byte_count = tags[279][2]
    return ifd, offset, byte_count


def bind_mutations(root: Path, recipe_path: Path) -> dict[str, Any]:
    source = json.loads(recipe_path.read_text(encoding="utf-8"))
    expected_paths = sorted(
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in {".tif", ".tiff"}
    )
    bound = []
    for recipe in source["recipes"]:
        result = dict(recipe)
        base = (root / recipe["base_path"]).read_bytes()
        operation = recipe["operation"]
        if operation == "replace_bytes":
            result.update(byte_offset=0, original_hex=base[:2].hex())
            mutated = bytes.fromhex(result["replacement_hex"]) + base[2:]
        elif operation == "truncate_header":
            result["truncate_at"] = 4
            mutated = base[:4]
        elif operation == "truncate_ifd":
            ifd, _, _ = first_ifd_and_strip(base)
            result["truncate_at"] = ifd + 1
            mutated = base[: ifd + 1]
        elif operation == "truncate_strip_data":
            _, offset, byte_count = first_ifd_and_strip(base)
            result["truncate_at"] = offset + byte_count - 1
            mutated = base[: result["truncate_at"]]
        elif operation == "replace_ome_sizez":
            original = b'SizeZ="2"'
            replacement = b'SizeZ="3"'
            if base.count(original) != 1:
                raise RuntimeError("OME SizeZ mutation target is not unique")
            offset = base.index(original)
            result.update(
                byte_offset=offset,
                original_hex=original.hex(),
                replacement_hex=replacement.hex(),
            )
            mutated = base[:offset] + replacement + base[offset + len(original) :]
        elif operation in {"remove_group_member", "duplicate_group_member"}:
            paths = expected_paths.copy()
            if operation == "remove_group_member":
                paths.remove(recipe["base_path"])
            else:
                paths.append(recipe["base_path"])
            mutated = canonical_json({"paths": paths})
        elif operation == "replace_with_multipage_file":
            mutated = (root / recipe["replacement_path"]).read_bytes()
        else:
            raise RuntimeError(f"unsupported mutation operation {operation!r}")
        result["mutated_bytes"] = len(mutated)
        result["mutated_sha256"] = sha256_bytes(mutated)
        bound.append(result)
    return {
        "schema": "mirante4d-source-fixture-bound-mutations",
        "schema_version": 1,
        "source_recipe_sha256": sha256_file(recipe_path),
        "recipes": bound,
    }


def compare_facts(facts_path: Path, reader_path: Path) -> None:
    facts = json.loads(facts_path.read_text(encoding="utf-8"))
    reader = json.loads(reader_path.read_text(encoding="utf-8"))
    expected = {
        item["path"]: item
        for family in facts["specifications"]
        for item in family["files"]
    }
    observed = {item["path"]: item for item in reader["files"]}
    if set(expected) != set(observed):
        raise RuntimeError("fact oracle and reader path sets differ")
    fields = ("dtype", "ifd_count", "width", "height", "logical_bytes", "logical_value_sha256")
    for path in sorted(expected):
        for field in fields:
            if expected[path][field] != observed[path][field]:
                raise RuntimeError(f"fact/reader mismatch for {path}: {field}")
        for field in ("minimum", "maximum"):
            wanted = expected[path][field]
            actual = observed[path][field]
            if wanted is None:
                if actual is not None:
                    raise RuntimeError(f"fact/reader mismatch for {path}: {field}")
            elif float(wanted) != float(actual):
                raise RuntimeError(f"fact/reader mismatch for {path}: {field}")
    if facts["logical_value_sha256"] != reader["logical_value_sha256"]:
        raise RuntimeError("fact oracle and reader global logical hashes differ")
    if facts["logical_voxel_bytes"] != reader["logical_voxel_bytes"]:
        raise RuntimeError("fact oracle and reader logical byte totals differ")


def member_inventory(root: Path) -> tuple[list[str], list[str]]:
    files = sorted(path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file())
    directories = sorted(path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_dir())
    if len(files) != 20 or len(directories) != 7:
        raise RuntimeError(f"approved archive must have 20 files and 7 directories, got {len(files)}/{len(directories)}")
    return files, directories


def generated_tree_sha256(root: Path, files: list[str], directories: list[str]) -> str:
    entries: list[dict[str, Any]] = [{"path": path, "type": "directory"} for path in directories]
    entries.extend(
        {
            "path": path,
            "type": "file",
            "bytes": (root / path).stat().st_size,
            "sha256": sha256_file(root / path),
        }
        for path in files
    )
    entries.sort(key=lambda item: item["path"])
    return sha256_bytes(canonical_json(entries))


def write_ustar(root: Path, destination: Path, files: list[str], directories: list[str]) -> None:
    with tarfile.open(destination, "w", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(directories):
            info = tarfile.TarInfo(path)
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ""
            archive.addfile(info)
        for path in sorted(files):
            data = (root / path).read_bytes()
            info = tarfile.TarInfo(path)
            info.size = len(data)
            info.mode = 0o644
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ""
            archive.addfile(info, __import__("io").BytesIO(data))


def independence_audit(repo: Path) -> dict[str, Any]:
    sources = {
        "producer": repo / "tools/source-fixtures/producer/produce.py",
        "reader": repo / "tools/source-fixtures/reader/reader.py",
    }
    imports: dict[str, list[str]] = {}
    for name, path in sources.items():
        found = set()
        for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
            if isinstance(node, ast.Import):
                found.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                found.add(node.module.split(".")[0])
        imports[name] = sorted(found)
    producer_external = sorted(set(imports["producer"]) & {"numpy", "tifffile", "PIL", "lxml", "mirante4d"})
    reader_external = sorted(set(imports["reader"]) & {"numpy", "tifffile", "PIL", "lxml", "mirante4d"})
    cargo = (repo / "tools/source-fixtures/fact-oracle/Cargo.toml").read_text(encoding="utf-8")
    rust = (repo / "tools/source-fixtures/fact-oracle/src/main.rs").read_text(encoding="utf-8")
    if producer_external != ["numpy", "tifffile"] or reader_external != ["PIL", "lxml"]:
        raise RuntimeError("Python lineage import boundary failed")
    if "[dependencies]" in cargo or any(term in rust.lower() for term in ("use mirante4d", "tifffile", "numpy", "pillow", "lxml", "extern crate")):
        raise RuntimeError("fact-oracle dependency boundary failed")
    return {
        "schema": "mirante4d-source-fixture-independence-audit",
        "schema_version": 1,
        "status": "passed",
        "producer_imports": imports["producer"],
        "producer_external": producer_external,
        "reader_imports": imports["reader"],
        "reader_external": reader_external,
        "fact_oracle_dependencies": [],
        "production_imports": [],
    }


def tool_artifact(
    name: str,
    version: str,
    revision: str,
    url: str,
    release_sha: str,
    installed_sha: str,
    license_name: str,
) -> dict[str, str]:
    return {
        "name": name,
        "version": version,
        "source_revision": revision,
        "source_url": url,
        "release_artifact_sha256": release_sha,
        "installed_artifact_sha256": installed_sha,
        "license": license_name,
    }


def python_runtime() -> dict[str, str]:
    return tool_artifact(
        "CPython",
        "3.12.3",
        "3.12.3-1ubuntu0.15",
        "https://packages.ubuntu.com/noble-updates/python3.12",
        PYTHON_SHA256,
        PYTHON_SHA256,
        "Python-2.0",
    )


def build_manifest(
    repo: Path,
    archive_path: Path,
    root: Path,
    files: list[str],
    directories: list[str],
    tree_sha: str,
    reader_report_sha: str,
    run_evidence: list[str],
    installed: dict[str, dict[str, str]],
) -> dict[str, Any]:
    producer_lock_path = repo / "tools/source-fixtures/producer/requirements.lock.json"
    reader_lock_path = repo / "tools/source-fixtures/reader/requirements.lock.json"
    fact_lock_path = repo / "tools/source-fixtures/fact-oracle/Cargo.lock"
    producer_lock = load_lock(producer_lock_path)
    reader_lock = load_lock(reader_lock_path)

    def wheel_artifacts(lock: dict[str, Any], lineage: str) -> list[dict[str, str]]:
        return [
            tool_artifact(
                wheel["name"],
                wheel["version"],
                wheel["version"],
                wheel["url"],
                wheel["sha256"],
                installed[lineage][wheel["name"]],
                wheel["license"],
            )
            for wheel in lock["wheels"]
            if wheel["name"] != "OME-XML-XSD"
        ]

    reader_dependencies = wheel_artifacts(reader_lock, "reader")
    xsd = next(item for item in reader_lock["wheels"] if item["name"] == "OME-XML-XSD")
    reader_dependencies.append(
        tool_artifact(
            xsd["name"], xsd["version"], xsd["version"], xsd["url"], xsd["sha256"], xsd["sha256"], xsd["license"]
        )
    )
    independence = independence_audit(repo)
    facts = json.loads((root / "records/expected-facts.json").read_text(encoding="utf-8"))
    file_manifest = {}
    for path in files:
        role, media_type = (
            ("source_tiff", "image/tiff")
            if path.endswith((".tif", ".tiff"))
            else (
                {
                    "records/expected-facts.json": "expected_facts",
                    "records/grouping.json": "source_grouping",
                    "records/mutations.json": "mutation_manifest",
                    "records/provenance-license.json": "provenance_license",
                }[path],
                "application/json",
            )
        )
        file_manifest[path] = {
            "media_type": media_type,
            "bytes": (root / path).stat().st_size,
            "sha256": sha256_file(root / path),
            "role": role,
        }

    children: dict[str, int] = {}
    for path in files + directories:
        parent = str(Path(path).parent).replace(".", "")
        children[parent] = children.get(parent, 0) + 1
    regular_bytes = sum((root / path).stat().st_size for path in files)
    return {
        "$schema": "../../docs/plans/active/foundation-refactor/schemas/foundation-source-fixture-manifest-v1.schema.json",
        "schema": "mirante4d-foundation-source-fixture-manifest",
        "schema_version": 1,
        "specification_ids": SPEC_IDS,
        "specification_version": 1,
        "status": "independently_validated",
        "publication_class": "public_safe",
        "license": "MIT",
        "producer": {
            "lineage_id": "SRC-PRODUCER-001",
            "lineage_class": "byte_producer",
            "implementation_source": "tools/source-fixtures/producer/produce.py",
            "implementation_sha256": sha256_file(repo / "tools/source-fixtures/producer/produce.py"),
            "runtime": python_runtime(),
            "dependencies": wheel_artifacts(producer_lock, "producer"),
            "lock_manifest_path": "tools/source-fixtures/producer/requirements.lock.json",
            "lock_manifest_sha256": sha256_file(producer_lock_path),
            "reproduction_command_id": "SRC-CMD-001",
            "independence_statement": "Writes TIFF bytes from the declarative specification; it does not author facts or validation results and imports no Mirante4D code.",
        },
        "expected_fact_authority": {
            "lineage_id": "SRC-FACT-001",
            "lineage_class": "fact_oracle",
            "implementation_source": "tools/source-fixtures/fact-oracle/src/main.rs",
            "implementation_sha256": sha256_file(repo / "tools/source-fixtures/fact-oracle/src/main.rs"),
            "runtime": tool_artifact(
                "Rust",
                "1.96.1",
                "31fca3adb283cc9dfd56b49cdee9a96eb9c96ffd",
                "https://static.rust-lang.org/dist/2026-06-25/rustc-1.96.1-x86_64-unknown-linux-gnu.tar.xz",
                RUSTC_RELEASE_SHA256,
                RUSTC_SHA256,
                "MIT OR Apache-2.0",
            ),
            "dependencies": [],
            "lock_manifest_path": "tools/source-fixtures/fact-oracle/Cargo.lock",
            "lock_manifest_sha256": sha256_file(fact_lock_path),
            "reproduction_command_id": "SRC-CMD-002",
            "independence_statement": "Computes facts only from the declarative specification, never opens TIFF, and has no external crate dependency.",
        },
        "independent_reader": {
            "lineage_id": "SRC-READER-001",
            "lineage_class": "independent_reader",
            "implementation_source": "tools/source-fixtures/reader/reader.py",
            "implementation_sha256": sha256_file(repo / "tools/source-fixtures/reader/reader.py"),
            "runtime": python_runtime(),
            "dependencies": reader_dependencies,
            "lock_manifest_path": "tools/source-fixtures/reader/requirements.lock.json",
            "lock_manifest_sha256": sha256_file(reader_lock_path),
            "reproduction_command_id": "SRC-CMD-003",
            "independence_statement": "Observes TIFF/OME facts with Pillow/lxml only; it imports neither producer libraries nor Mirante4D code.",
        },
        "independence_audit_sha256": sha256_bytes(canonical_json(independence)),
        "archive": {
            "path": f"fixtures/source/{ARCHIVE_NAME}",
            "format": "ustar",
            "compression": "none",
            "sha256": sha256_file(archive_path),
            "archive_bytes": archive_path.stat().st_size,
            "regular_file_bytes": regular_bytes,
            "logical_voxel_bytes": facts["logical_voxel_bytes"],
            "directories": directories,
            "max_depth": max(len(Path(path).parts) for path in files),
            "max_fan_out": max(children.values()),
            "max_path_bytes": max(len(path.encode("ascii")) for path in files + directories),
            "max_file_bytes": max((root / path).stat().st_size for path in files),
            "compression_ratio": 1,
        },
        "files": file_manifest,
        "expected_facts": {
            "path": "records/expected-facts.json",
            "schema_id": "mirante4d-source-fixture-expected-facts",
            "schema_version": 1,
            "sha256": sha256_file(root / "records/expected-facts.json"),
            "logical_value_digest_algorithm": "sha256",
            "logical_value_sha256": facts["logical_value_sha256"],
            "independent_reader_report_sha256": reader_report_sha,
        },
        "reproduction": {
            "command_id": "SRC-CMD-004",
            "run_evidence_sha256": run_evidence,
            "generated_tree_sha256": tree_sha,
        },
        "approvals": {
            "project_original_and_license": {
                "state": "approved",
                "role": "repository owner",
                "approved_by": "Mirante4D repository owner",
                "approved_on": "2026-07-10",
                "reference": "OA-001",
            },
            "scientific_and_layout_facts": {
                "state": "approved",
                "role": "repository owner",
                "approved_by": "Mirante4D repository owner",
                "approved_on": "2026-07-10",
                "reference": "OA-001",
            },
        },
    }


def reproduce_once(
    repo: Path,
    work: Path,
    producer_site: Path,
    reader_site: Path,
    xsd: Path,
    oracle: Path,
    run_id: str,
) -> dict[str, Any]:
    root = work / "root"
    root.mkdir(parents=True)
    spec = repo / "tools/source-fixtures/specification/v1.tsv"
    ome = repo / "tools/source-fixtures/specification/ome-2016-06.xml"
    run(
        [str(PYTHON), "-S", str(repo / "tools/source-fixtures/producer/produce.py"), "--spec", str(spec), "--ome-xml", str(ome), "--output", str(root)],
        env=python_env(producer_site),
    )
    records = root / "records"
    records.mkdir()
    facts = records / "expected-facts.json"
    run([str(oracle), str(spec), str(facts)])
    shutil.copyfile(repo / "tools/source-fixtures/specification/grouping-v1.json", records / "grouping.json")
    shutil.copyfile(repo / "tools/source-fixtures/specification/provenance-license-v1.json", records / "provenance-license.json")
    mutations = bind_mutations(root, repo / "tools/source-fixtures/specification/mutation-recipes-v1.json")
    (records / "mutations.json").write_bytes(canonical_json(mutations))
    reader_report = work / "reader-report.json"
    run(
        [
            str(PYTHON), "-S", str(repo / "tools/source-fixtures/reader/reader.py"), "observe",
            "--root", str(root), "--spec", str(spec), "--ome-xml", str(ome), "--xsd", str(xsd),
            "--mutations", str(records / "mutations.json"), "--report", str(reader_report),
        ],
        env=python_env(reader_site),
    )
    compare_facts(facts, reader_report)
    files, directories = member_inventory(root)
    tree_sha = generated_tree_sha256(root, files, directories)
    archive = work / ARCHIVE_NAME
    write_ustar(root, archive, files, directories)
    evidence = {
        "schema": "mirante4d-source-fixture-reproduction-run",
        "schema_version": 1,
        "run_id": run_id,
        "status": "passed",
        "archive_sha256": sha256_file(archive),
        "generated_tree_sha256": tree_sha,
        "expected_facts_sha256": sha256_file(facts),
        "independent_reader_report_sha256": sha256_file(reader_report),
        "mutation_manifest_sha256": sha256_file(records / "mutations.json"),
    }
    evidence_path = work / "run-evidence.json"
    evidence_path.write_bytes(canonical_json(evidence))
    return {
        "root": root,
        "archive": archive,
        "reader_report": reader_report,
        "files": files,
        "directories": directories,
        "tree_sha": tree_sha,
        "evidence_sha": sha256_file(evidence_path),
        "stable": {key: value for key, value in evidence.items() if key != "run_id"},
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--write-repository", action="store_true")
    parser.add_argument("--work-directory", type=Path)
    args = parser.parse_args()
    repo = args.repository.resolve()
    verify_runtime()

    temporary: tempfile.TemporaryDirectory[str] | None = None
    if args.work_directory:
        work = args.work_directory.resolve()
        if work.exists():
            shutil.rmtree(work)
        work.mkdir(parents=True)
    else:
        temporary = tempfile.TemporaryDirectory(prefix="mirante4d-source-fixtures-")
        work = Path(temporary.name)

    producer_site, producer_installed = prepare_python_lineage(
        repo / "tools/source-fixtures/producer/requirements.lock.json", work / "producer-lineage"
    )
    reader_site, reader_installed = prepare_python_lineage(
        repo / "tools/source-fixtures/reader/requirements.lock.json", work / "reader-lineage"
    )
    reader_lock = load_lock(repo / "tools/source-fixtures/reader/requirements.lock.json")
    xsd_record = next(item for item in reader_lock["wheels"] if item["name"] == "OME-XML-XSD")
    xsd = work / "reader-lineage/ome.xsd"
    fetch(xsd_record["url"], xsd_record["sha256"], xsd)

    capability = work / "reader-capability.json"
    run(
        [str(PYTHON), "-S", str(repo / "tools/source-fixtures/reader/reader.py"), "probe", "--report", str(capability)],
        env=python_env(reader_site),
    )

    target = work / "cargo-target"
    cargo_env = os.environ.copy()
    cargo_env.update({"CARGO_TARGET_DIR": str(target), "CARGO_NET_OFFLINE": "true", "LC_ALL": "C", "TZ": "UTC"})
    manifest = repo / "tools/source-fixtures/fact-oracle/Cargo.toml"
    run(["cargo", "build", "--manifest-path", str(manifest), "--locked", "--offline"], env=cargo_env)
    oracle = target / "debug/mirante4d-source-fixture-fact-oracle"

    first = reproduce_once(repo, work / "run-1", producer_site, reader_site, xsd, oracle, "run-1")
    second = reproduce_once(repo, work / "run-2", producer_site, reader_site, xsd, oracle, "run-2")
    if first["archive"].read_bytes() != second["archive"].read_bytes() or first["stable"] != second["stable"]:
        raise RuntimeError("the two isolated fixture runs are not byte-identical")
    if first["evidence_sha"] == second["evidence_sha"]:
        raise RuntimeError("run evidence must identify the two independent executions")

    output = work / "manifest.json"
    fixture_manifest = build_manifest(
        repo,
        first["archive"],
        first["root"],
        first["files"],
        first["directories"],
        first["tree_sha"],
        sha256_file(first["reader_report"]),
        [first["evidence_sha"], second["evidence_sha"]],
        {"producer": producer_installed, "reader": reader_installed},
    )
    output.write_bytes(canonical_json(fixture_manifest))
    if args.write_repository:
        destination = repo / "fixtures/source"
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(first["archive"], destination / first["archive"].name)
        shutil.copyfile(output, destination / "manifest.json")
    print(f"archive sha256: {fixture_manifest['archive']['sha256']}")
    print(f"generated tree sha256: {first['tree_sha']}")
    print(f"manifest: {output}")
    print(f"run evidence: {work / 'run-1/run-evidence.json'}, {work / 'run-2/run-evidence.json'}")
    if temporary is not None:
        temporary.cleanup()


if __name__ == "__main__":
    main()
