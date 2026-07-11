#!/usr/bin/env python3
"""Schema and semantic validation for the bounded source-fixture archive."""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import io
import json
import subprocess
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path
from typing import Any


SPEC_IDS = [f"SRC-TIFF-SPEC-{number:03d}" for number in range(1, 5)]
ARCHIVE_NAME = "mirante4d-source-tiff-fixtures-v1.tar"
SOURCEMETA_URL = "https://github.com/sourcemeta/jsonschema/releases/download/v16.1.0/jsonschema-16.1.0-linux-x86_64.zip"
SOURCEMETA_ARCHIVE_SHA256 = "96b214be67bf25c6184f1d009a94e082d1eaa83787a8f1878607aebf3185668e"
SOURCEMETA_BINARY_SHA256 = "4aa8ba3f4bc0b1ef4f8d82b109676b186fa66603d1953be25fde22b2854190d5"
SOURCEMETA_MEMBER = "jsonschema-16.1.0-linux-x86_64/bin/jsonschema"
EXPECTED_RELEASES = {
    "CPython": ("3.12.3", "1643dacd9feaedc58f3cc581e4d22577dfe25c09b10282936186ccf0f2e61118"),
    "Rust": ("1.96.1", "3545a0efad2355ecb0a3b9ac02efee96e27f1f9d24b7ce2fc3f279b2efb0d923"),
    "tifffile": ("2026.6.1", "0d7382d2769b855b81ce358528e2b40c16d48aa39031746efa81215205332a8d"),
    "numpy": ("2.5.1", "59fda5e192b570217ec2580c96f00e9a7e12ef6866a900eb089b62c1a32545ca"),
    "Pillow": ("12.3.0", "78cb2c6865a35ab8ff8b75fd122f6033b92a62c82801110e48ddd6c936a45d91"),
    "lxml": ("6.1.1", "ebe6af670449830d6d9b752c256a983291c766a1365ba5d5460048f9e33a7818"),
    "OME-XML-XSD": ("2016-06", "64b439ff488c87d81ca112b73b7123596952ff8a8543e3b02d94ea8db5ed51ee"),
}


class ValidationError(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValidationError(message)


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256(path.read_bytes())


def prepare_schema_validator(destination: Path, supplied: Path | None) -> Path:
    if supplied is not None:
        binary = supplied.resolve()
    else:
        request = urllib.request.Request(SOURCEMETA_URL, headers={"User-Agent": "Mirante4D-fixture-validator/1"})
        with urllib.request.urlopen(request, timeout=60) as response:
            encoded = response.read()
        require(sha256(encoded) == SOURCEMETA_ARCHIVE_SHA256, "SourceMeta release archive hash failed")
        with zipfile.ZipFile(io.BytesIO(encoded)) as archive:
            require(archive.namelist().count(SOURCEMETA_MEMBER) == 1, "SourceMeta release member failed")
            binary = destination / "jsonschema"
            binary.write_bytes(archive.read(SOURCEMETA_MEMBER))
            binary.chmod(0o755)
    require(binary.is_file() and sha256_file(binary) == SOURCEMETA_BINARY_SHA256, "SourceMeta executable hash failed")
    version = subprocess.run([str(binary), "--version"], check=True, capture_output=True, text=True).stdout.strip()
    require(version == "16.1.0", "SourceMeta executable version failed")
    return binary


def schema_command(command: list[str]) -> None:
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise ValidationError(f"SourceMeta schema validation failed: {detail}")


def schema_validate(repo: Path, manifest_path: Path, validator: Path) -> None:
    schema_path = repo / "docs/plans/active/foundation-refactor/schemas/foundation-source-fixture-manifest-v1.schema.json"
    common_path = repo / "docs/plans/active/foundation-refactor/schemas/foundation-common-v1.schema.json"
    schema_command([str(validator), "metaschema", str(common_path), str(schema_path), "--format-assertion"])
    schema_command(
        [
            str(validator),
            "validate",
            str(schema_path),
            str(manifest_path),
            "--resolve",
            str(schema_path.parent),
            "--format-assertion",
        ]
    )


def audit_independence(repo: Path) -> dict[str, Any]:
    imports: dict[str, list[str]] = {}
    for name, relative in {
        "producer": "tools/source-fixtures/producer/produce.py",
        "reader": "tools/source-fixtures/reader/reader.py",
    }.items():
        found = set()
        tree = ast.parse((repo / relative).read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                found.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                found.add(node.module.split(".")[0])
        imports[name] = sorted(found)
    external = {"numpy", "tifffile", "PIL", "lxml", "mirante4d"}
    producer_external = sorted(set(imports["producer"]) & external)
    reader_external = sorted(set(imports["reader"]) & external)
    cargo = (repo / "tools/source-fixtures/fact-oracle/Cargo.toml").read_text(encoding="utf-8")
    rust = (repo / "tools/source-fixtures/fact-oracle/src/main.rs").read_text(encoding="utf-8").lower()
    require(producer_external == ["numpy", "tifffile"], "producer import boundary failed")
    require(reader_external == ["PIL", "lxml"], "reader import boundary failed")
    require("[dependencies]" not in cargo, "fact oracle must have no dependency table")
    require(not any(term in rust for term in ("use mirante4d", "tifffile", "numpy", "pillow", "lxml", "extern crate")), "fact-oracle dependency boundary failed")
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


def inspect_archive(archive_path: Path) -> tuple[dict[str, bytes], list[str], list[str], dict[str, int]]:
    encoded = archive_path.read_bytes()
    require(0 < len(encoded) <= 2 * 1024 * 1024, "archive byte limit failed")
    require(len(encoded) % 512 == 0 and encoded[257:263] == b"ustar\0", "archive is not raw POSIX ustar")
    require(not encoded.startswith((b"\x1f\x8b", b"BZh", b"\xfd7zXZ")), "archive must be uncompressed")
    files: dict[str, bytes] = {}
    directories: list[str] = []
    order: list[str] = []
    with tarfile.open(fileobj=io.BytesIO(encoded), mode="r:") as archive:
        members = archive.getmembers()
        require(len(members) <= 40, "archive member count is unreasonably large")
        for member in members:
            path = member.name.rstrip("/")
            try:
                path.encode("ascii")
            except UnicodeEncodeError as error:
                raise ValidationError("archive path is not ASCII") from error
            require(path and not path.startswith("/") and ".." not in Path(path).parts and "\\" not in path, "unsafe archive path")
            require(path not in order, "duplicate archive member")
            require(member.uid == 0 and member.gid == 0 and member.uname == "" and member.gname == "", "archive ownership is not normalized")
            require(member.mtime == 0, "archive timestamp is not normalized")
            if member.isdir():
                require(member.mode == 0o755, "directory mode is not 0755")
                directories.append(path)
            elif member.isfile():
                require(member.mode == 0o644, "file mode is not 0644")
                extracted = archive.extractfile(member)
                require(extracted is not None, "regular file cannot be read")
                files[path] = extracted.read()
                require(len(files[path]) == member.size, "archive member size mismatch")
            else:
                raise ValidationError("archive contains link, special, sparse, or extension member")
            order.append(path)
    require(len(files) == 20 and len(directories) == 7, "archive must contain exactly 20 files and 7 directories")
    require(order == sorted(directories) + sorted(files), "archive member order is not deterministic")
    children: dict[str, int] = {}
    for path in list(files) + directories:
        parent = Path(path).parent.as_posix()
        if parent == ".":
            parent = ""
        children[parent] = children.get(parent, 0) + 1
    metrics = {
        "archive_bytes": len(encoded),
        "regular_file_bytes": sum(map(len, files.values())),
        "directories": len(directories),
        "max_depth": max(len(Path(path).parts) for path in files),
        "max_fan_out": max(children.values()),
        "max_path_bytes": max(len(path.encode("ascii")) for path in list(files) + directories),
        "max_file_bytes": max(map(len, files.values())),
    }
    require(len(files) <= 32 and metrics["directories"] <= 8, "archive count limits failed")
    require(metrics["max_depth"] <= 4 and metrics["max_fan_out"] <= 16, "archive topology limits failed")
    require(metrics["max_path_bytes"] <= 120 and metrics["max_file_bytes"] <= 1024 * 1024, "archive path/file limits failed")
    require(metrics["regular_file_bytes"] <= 4 * 1024 * 1024, "regular-file byte limit failed")
    return files, sorted(directories), order, metrics


def tree_sha(files: dict[str, bytes], directories: list[str]) -> str:
    entries: list[dict[str, Any]] = [{"path": path, "type": "directory"} for path in directories]
    entries.extend({"path": path, "type": "file", "bytes": len(data), "sha256": sha256(data)} for path, data in files.items())
    entries.sort(key=lambda item: item["path"])
    return sha256(canonical_json(entries))


def validate_records(files: dict[str, bytes], manifest: dict[str, Any]) -> None:
    required_records = {
        "records/expected-facts.json",
        "records/grouping.json",
        "records/mutations.json",
        "records/provenance-license.json",
    }
    require(required_records <= set(files), "archive is missing a required record")
    facts = json.loads(files["records/expected-facts.json"])
    grouping = json.loads(files["records/grouping.json"])
    mutations = json.loads(files["records/mutations.json"])
    provenance = json.loads(files["records/provenance-license.json"])
    require(facts.get("schema") == "mirante4d-source-fixture-expected-facts" and facts.get("schema_version") == 1, "facts schema identity failed")
    require(facts.get("fact_authority") == "SRC-FACT-001" and facts.get("axes") == ["t", "c", "z", "y", "x"], "facts authority/axes failed")
    require(facts.get("logical_voxel_bytes") == 491 and facts.get("logical_voxel_bytes") <= 1024 * 1024, "logical voxel byte total failed")
    families = facts.get("specifications", [])
    require([family.get("id") for family in families] == SPEC_IDS, "facts specification identities failed")
    fact_files = [item for family in families for item in family.get("files", [])]
    require(len(fact_files) == 16 and len({item["path"] for item in fact_files}) == 16, "facts file coverage failed")
    require(all(item["path"] in files for item in fact_files), "facts name a missing TIFF")
    require(sum(item["logical_bytes"] for item in fact_files) == 491, "per-file logical byte relation failed")
    require(manifest["expected_facts"]["sha256"] == sha256(files["records/expected-facts.json"]), "facts record digest failed")
    require(manifest["expected_facts"]["logical_value_sha256"] == facts["logical_value_sha256"], "logical digest relation failed")
    require(grouping.get("schema") == "mirante4d-source-fixture-grouping" and grouping.get("schema_version") == 1, "grouping schema identity failed")
    require([group.get("specification_id") for group in grouping.get("groups", [])] == SPEC_IDS, "grouping coverage failed")
    grouped = [path for group in grouping["groups"] for path in group["paths"]]
    require(sorted(grouped) == sorted(item["path"] for item in fact_files), "grouping/facts path relation failed")
    require(mutations.get("schema") == "mirante4d-source-fixture-bound-mutations" and mutations.get("schema_version") == 1, "mutation schema identity failed")
    recipes = mutations.get("recipes", [])
    require(len(recipes) == 8 and len({item["id"] for item in recipes}) == 8, "mutation coverage failed")
    for recipe in recipes:
        require(recipe["base_path"] in files, "mutation base is absent")
        base = files[recipe["base_path"]]
        operation = recipe["operation"]
        if operation in {"replace_bytes", "replace_ome_sizez"}:
            offset = recipe["byte_offset"]
            original = bytes.fromhex(recipe["original_hex"])
            replacement = bytes.fromhex(recipe["replacement_hex"])
            require(base[offset : offset + len(original)] == original, "mutation original bytes failed")
            mutated = base[:offset] + replacement + base[offset + len(original) :]
        elif operation in {"truncate_header", "truncate_ifd", "truncate_strip_data"}:
            mutated = base[: recipe["truncate_at"]]
        elif operation == "replace_with_multipage_file":
            mutated = files[recipe["replacement_path"]]
        elif operation in {"remove_group_member", "duplicate_group_member"}:
            paths = sorted(item["path"] for item in fact_files)
            if operation == "remove_group_member":
                paths.remove(recipe["base_path"])
            else:
                paths.append(recipe["base_path"])
            mutated = canonical_json({"paths": paths})
        else:
            raise ValidationError("unknown mutation operation")
        require(recipe["mutated_bytes"] == len(mutated) and recipe["mutated_sha256"] == sha256(mutated), "bound mutation digest failed")
    require(provenance.get("schema") == "mirante4d-source-fixture-provenance-license", "provenance schema identity failed")
    require(provenance.get("fixture_license") == "MIT" and provenance.get("project_original") is True, "fixture provenance/license failed")
    require(provenance.get("ome_xsd", {}).get("sha256") == EXPECTED_RELEASES["OME-XML-XSD"][1], "OME XSD provenance failed")


def validate_lineage(repo: Path, manifest: dict[str, Any]) -> None:
    expected = {
        "producer": ("SRC-PRODUCER-001", "byte_producer", ["tifffile", "numpy"]),
        "expected_fact_authority": ("SRC-FACT-001", "fact_oracle", []),
        "independent_reader": ("SRC-READER-001", "independent_reader", ["Pillow", "lxml", "OME-XML-XSD"]),
    }
    for key, (lineage_id, lineage_class, dependency_names) in expected.items():
        lineage = manifest[key]
        require(lineage["lineage_id"] == lineage_id and lineage["lineage_class"] == lineage_class, f"{key} identity failed")
        source = repo / lineage["implementation_source"]
        lock = repo / lineage["lock_manifest_path"]
        require(source.is_file() and sha256_file(source) == lineage["implementation_sha256"], f"{key} implementation digest failed")
        require(lock.is_file() and sha256_file(lock) == lineage["lock_manifest_sha256"], f"{key} lock digest failed")
        require([item["name"] for item in lineage["dependencies"]] == dependency_names, f"{key} dependency set failed")
        artifacts = [lineage["runtime"], *lineage["dependencies"]]
        for artifact in artifacts:
            version, release = EXPECTED_RELEASES[artifact["name"]]
            require(artifact["version"] == version and artifact["release_artifact_sha256"] == release, f"{artifact['name']} pin failed")
    require(manifest["independence_audit_sha256"] == sha256(canonical_json(audit_independence(repo))), "independence audit digest failed")


def validate(
    repo: Path,
    manifest_path: Path,
    archive_path: Path,
    validator: Path,
    reader_report: Path | None = None,
) -> None:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    schema_validate(repo, manifest_path, validator)
    require(manifest["specification_ids"] == SPEC_IDS and manifest["status"] == "independently_validated", "manifest status/specifications failed")
    require(manifest["publication_class"] == "public_safe" and manifest["license"] == "MIT", "publication/license classification failed")
    require(manifest["archive"]["path"] == f"fixtures/source/{ARCHIVE_NAME}", "archive path failed")
    require(manifest["archive"]["sha256"] == sha256_file(archive_path), "archive digest failed")
    files, directories, _, metrics = inspect_archive(archive_path)
    archive_manifest = manifest["archive"]
    for key in ("archive_bytes", "regular_file_bytes", "max_depth", "max_fan_out", "max_path_bytes", "max_file_bytes"):
        require(archive_manifest[key] == metrics[key], f"archive metric {key} failed")
    require(archive_manifest["directories"] == directories, "archive directory inventory failed")
    require(archive_manifest["logical_voxel_bytes"] == 491, "manifest logical byte total failed")
    require(set(manifest["files"]) == set(files), "manifest/archive file set failed")
    for path, data in files.items():
        record = manifest["files"][path]
        require(record["bytes"] == len(data) and record["sha256"] == sha256(data), f"file digest/size failed: {path}")
        expected_role = "source_tiff" if path.endswith((".tif", ".tiff")) else {
            "records/expected-facts.json": "expected_facts",
            "records/grouping.json": "source_grouping",
            "records/mutations.json": "mutation_manifest",
            "records/provenance-license.json": "provenance_license",
        }[path]
        require(record["role"] == expected_role, f"file role failed: {path}")
    require(manifest["reproduction"]["generated_tree_sha256"] == tree_sha(files, directories), "generated tree digest failed")
    runs = manifest["reproduction"]["run_evidence_sha256"]
    require(len(runs) == 2 and len(set(runs)) == 2, "double-run evidence relation failed")
    validate_records(files, manifest)
    validate_lineage(repo, manifest)
    if reader_report is not None:
        report = json.loads(reader_report.read_text(encoding="utf-8"))
        require(report.get("status") == "passed" and len(report.get("negative_cases", [])) == 8, "independent reader report failed")
        require(all(item.get("status") == "rejected_as_expected" for item in report["negative_cases"]), "reader negative-case result failed")
        require(sha256_file(reader_report) == manifest["expected_facts"]["independent_reader_report_sha256"], "reader report digest failed")


def self_test(repo: Path, manifest_path: Path, archive_path: Path, validator: Path) -> None:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    cases = []
    for mutate in (
        lambda value: value["archive"].__setitem__("logical_voxel_bytes", 492),
        lambda value: value["files"].pop("records/grouping.json"),
        lambda value: value["producer"]["dependencies"][0].__setitem__("release_artifact_sha256", "0" * 64),
        lambda value: value["reproduction"].__setitem__("generated_tree_sha256", "0" * 64),
    ):
        candidate = copy.deepcopy(manifest)
        mutate(candidate)
        cases.append(candidate)
    for index, candidate in enumerate(cases, 1):
        with tempfile.TemporaryDirectory(prefix="mirante4d-fixture-negative-") as temporary:
            path = Path(temporary) / "manifest.json"
            path.write_bytes(canonical_json(candidate))
            try:
                validate(repo, path, archive_path, validator)
            except ValidationError:
                continue
            raise ValidationError(f"semantic validator accepted negative self-test {index}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--manifest", type=Path, default=Path("fixtures/source/manifest.json"))
    parser.add_argument("--archive", type=Path, default=Path(f"fixtures/source/{ARCHIVE_NAME}"))
    parser.add_argument("--schema-validator", type=Path)
    parser.add_argument("--reader-report", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    repo = args.repository.resolve()
    manifest = args.manifest if args.manifest.is_absolute() else repo / args.manifest
    archive = args.archive if args.archive.is_absolute() else repo / args.archive
    reader = args.reader_report
    with tempfile.TemporaryDirectory(prefix="mirante4d-sourcemeta-") as temporary:
        validator = prepare_schema_validator(Path(temporary), args.schema_validator)
        validate(repo, manifest, archive, validator, reader)
        if args.self_test:
            self_test(repo, manifest, archive, validator)
    print("source-fixture schema and semantic validation: passed")


if __name__ == "__main__":
    main()
