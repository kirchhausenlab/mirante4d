#!/usr/bin/env python3
"""Prove production-writer semantics with the pinned independent T1 reader."""

from __future__ import annotations

import argparse
import copy
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
from types import ModuleType
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
T1_ROOT = ROOT / "tools/target-fixtures/t1"
REPRODUCER = T1_ROOT / "reproduce.py"
VALIDATOR = T1_ROOT / "validate.py"
READER = T1_ROOT / "reader/reader.py"
READER_LOCK = T1_ROOT / "reader/requirements-linux-x86_64-py312.lock"
OME_IMAGE_SCHEMA = ROOT / "verification/standards/ome-ngff-0.5.2/0.5/schemas/image.schema"
OME_VERSION_SCHEMA = OME_IMAGE_SCHEMA.with_name("_version.schema")
WRITER_TEST = ROOT / "crates/mirante4d-storage/tests/target_writer_conformance.rs"
MANIFEST = ROOT / "fixtures/target/manifest.json"
EXPECTED_FACTS = ROOT / "fixtures/target/expected-facts.json"
PROMOTED_READER_REPORT = ROOT / "fixtures/target/independent-reader-report.json"
REPRODUCTION_REPORT = ROOT / "fixtures/target/reproduction-report.json"
RUN_BASE = ROOT / "target/mirante4d/production-conformance"
CASE_IDS = [
    "m4d-t1-u8-2d-sparse",
    "m4d-t1-u16-3d-multiscale",
    "m4d-t1-f32-3d-validity",
]
WRITER_TEST_NAME = "production_writer_preserves_promoted_metadata_and_scientific_content"
COMMAND = [
    "cargo",
    "test",
    "-p",
    "mirante4d-storage",
    "--test",
    "target_writer_conformance",
    "--frozen",
    WRITER_TEST_NAME,
    "--",
    "--exact",
    "--nocapture",
]


class ConformanceError(RuntimeError):
    """A closed writer-to-independent-reader evidence failure."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ConformanceError(message)


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_bytes(canonical_json(value) + b"\n")
    os.replace(temporary, path)


def load_json(path: Path) -> Any:
    def pairs(rows: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in rows:
            require(key not in result, f"duplicate JSON key {key!r} in {path.name}")
            result[key] = value
        return result

    return json.loads(path.read_bytes(), object_pairs_hook=pairs)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def repository_path(path: Path) -> str:
    return path.resolve(strict=True).relative_to(ROOT).as_posix()


def load_immutable_module(name: str, path: Path) -> ModuleType:
    """Execute a pinned helper without writing bytecode beside its source."""

    module = ModuleType(name)
    module.__file__ = str(path)
    code = compile(path.read_bytes(), str(path), "exec", dont_inherit=True)
    exec(code, module.__dict__)
    return module


def load_reproducer() -> ModuleType:
    return load_immutable_module("mirante4d_target_t1_reproducer", REPRODUCER)


def load_validator() -> ModuleType:
    return load_immutable_module("mirante4d_target_t1_validator", VALIDATOR)


def validate_authority_bindings() -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    manifest = load_json(MANIFEST)
    facts = load_json(EXPECTED_FACTS)
    promoted = load_json(PROMOTED_READER_REPORT)
    reproduction = load_json(REPRODUCTION_REPORT)
    require(manifest.get("fixture_id") == "target-m4d-v1", "target authority identity drifted")
    require(manifest.get("status") == "independently_validated", "target authority is not promoted")
    reader = manifest.get("lineages", {}).get("independent_reader", {})
    require(reader.get("source_path") == repository_path(READER), "reader source path drifted")
    require(reader.get("source_sha256") == sha256_file(READER), "reader source digest drifted")
    require(reader.get("lock_path") == repository_path(READER_LOCK), "reader lock path drifted")
    require(reader.get("lock_sha256") == sha256_file(READER_LOCK), "reader lock digest drifted")
    reproducer = reproduction.get("reproducer", {})
    require(
        reproducer.get("source_path") == repository_path(REPRODUCER),
        "reproducer source path drifted",
    )
    require(
        reproducer.get("source_sha256") == sha256_file(REPRODUCER),
        "reproducer source digest drifted",
    )
    validator = manifest.get("validator", {})
    require(validator.get("path") == repository_path(VALIDATOR), "validator source path drifted")
    require(validator.get("sha256") == sha256_file(VALIDATOR), "validator source digest drifted")
    official_schema = promoted.get("official_schema", {})
    require(
        official_schema.get("image_schema_sha256") == sha256_file(OME_IMAGE_SCHEMA),
        "pinned OME image-schema digest drifted",
    )
    require(
        official_schema.get("version_schema_sha256") == sha256_file(OME_VERSION_SCHEMA),
        "pinned OME version-schema digest drifted",
    )
    require(
        [row.get("case_id") for row in facts.get("cases", [])] == sorted(CASE_IDS),
        "independent fact case set/order drifted",
    )
    require(
        [row.get("case_id") for row in promoted.get("cases", [])] == CASE_IDS,
        "promoted reader case set/order drifted",
    )
    return manifest, facts, promoted


def prepare_schema_validator(module: ModuleType, promoted: dict[str, Any]) -> dict[str, Any]:
    require(module.OME_SCHEMA == OME_IMAGE_SCHEMA, "validator OME image-schema path drifted")
    require(module.OME_VERSION == OME_VERSION_SCHEMA, "validator OME version-schema path drifted")
    reference = promoted.get("official_schema", {})
    require(
        module.SCHEMA_VALIDATOR_ID == reference.get("validator"),
        "official schema-validator identity drifted",
    )
    try:
        image_schema = module.load_json(OME_IMAGE_SCHEMA)
        version_schema = module.load_json(OME_VERSION_SCHEMA)
        module.audit_schema(image_schema, "OME image schema")
        module.audit_schema(version_schema, "OME version schema")
    except module.ValidationError as error:
        raise ConformanceError(f"pinned OME schema audit failed: {error}") from error
    return {
        "validator": module.SCHEMA_VALIDATOR_ID,
        "validator_path": repository_path(VALIDATOR),
        "validator_source_sha256": sha256_file(VALIDATOR),
        "image_schema_path": repository_path(OME_IMAGE_SCHEMA),
        "image_schema_sha256": sha256_file(OME_IMAGE_SCHEMA),
        "version_schema_path": repository_path(OME_VERSION_SCHEMA),
        "version_schema_sha256": sha256_file(OME_VERSION_SCHEMA),
    }


def fresh_run_root(requested: Path | None) -> tuple[Path, str]:
    RUN_BASE.mkdir(parents=True, exist_ok=True)
    require(RUN_BASE.is_dir() and not RUN_BASE.is_symlink(), "run base is unsafe")
    if requested is None:
        prefix = datetime.now(UTC).strftime("run-%Y%m%dT%H%M%SZ")
        run_id = f"{prefix}-{os.getpid()}"
        root = RUN_BASE / run_id
    else:
        root = requested if requested.is_absolute() else ROOT / requested
        root = Path(os.path.abspath(root))
        require(root.parent == RUN_BASE.resolve(strict=True), "output must be one child of the ignored run base")
        run_id = root.name
    require(run_id and run_id not in {".", ".."}, "run id is invalid")
    require(not os.path.lexists(root), "run root must be absent")
    root.mkdir(mode=0o700)
    facts = root.lstat()
    require(stat.S_ISDIR(facts.st_mode) and not root.is_symlink(), "run root is unsafe")
    ignored = subprocess.run(
        ["git", "check-ignore", "--quiet", str(root)],
        cwd=ROOT,
        check=False,
        timeout=30,
    )
    require(ignored.returncode == 0, "run root is not ignored by Git")
    return root, run_id


def run_writer_test(run_root: Path) -> tuple[Path, dict[str, Any]]:
    cargo_name = shutil.which("cargo")
    require(cargo_name is not None, "cargo is unavailable")
    cargo = Path(cargo_name)
    cargo_binary = cargo.resolve(strict=True)
    output_root = run_root / "outputs"
    environment = os.environ.copy()
    environment.update(
        {
            "LC_ALL": "C",
            "MIRANTE4D_WRITER_CONFORMANCE_OUTPUT_ROOT": str(output_root),
        }
    )
    result = subprocess.run(
        [str(cargo), *COMMAND[1:]],
        cwd=ROOT,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=600,
    )
    logs = run_root / "logs"
    logs.mkdir()
    stdout_path = logs / "writer-test.stdout.log"
    stderr_path = logs / "writer-test.stderr.log"
    stdout_path.write_bytes(result.stdout)
    stderr_path.write_bytes(result.stderr)
    require(result.returncode == 0, "production writer conformance test failed")
    require(output_root.is_dir() and not output_root.is_symlink(), "writer output root is absent or unsafe")
    require(
        sorted(path.name for path in output_root.iterdir()) == sorted(CASE_IDS),
        "writer output case closure drifted",
    )
    for case_id in CASE_IDS:
        package = output_root / case_id
        require(package.is_dir() and not package.is_symlink(), f"unsafe writer output for {case_id}")
    command_facts = {
        "argv": COMMAND,
        "cargo_sha256": sha256_file(cargo_binary),
        "cargo_version": command_output([str(cargo), "--version"]),
        "stdout": file_fact(stdout_path, run_root),
        "stderr": file_fact(stderr_path, run_root),
    }
    return output_root, command_facts


def command_output(command: list[str]) -> str:
    result = subprocess.run(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=60,
    )
    require(result.returncode == 0, f"tool identity command failed: {command[0]}")
    return result.stdout.decode("utf-8", "strict").strip()


def file_fact(path: Path, relative_root: Path = ROOT) -> dict[str, Any]:
    return {
        "path": path.relative_to(relative_root).as_posix(),
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
    }


def tree_rows(root: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for path in sorted(root.rglob("*")):
        facts = path.lstat()
        require(not stat.S_ISLNK(facts.st_mode), f"symlink in writer output: {path.name}")
        if stat.S_ISDIR(facts.st_mode):
            continue
        require(stat.S_ISREG(facts.st_mode), f"non-regular writer output: {path.name}")
        rows.append(file_fact(path, root))
    return rows


def semantic_image(image: Any) -> Any:
    result = copy.deepcopy(image)
    require(isinstance(result, dict), "reader image report is not an object")
    levels = result.get("levels")
    require(isinstance(levels, list), "reader image levels are absent")
    for level in levels:
        require(isinstance(level, dict), "reader image level is not an object")
        arrays = [level.get("pixel"), level.get("packed_index")]
        validity = level.get("validity")
        require(isinstance(validity, dict), "reader validity report is absent")
        if validity.get("array") is not None:
            arrays.append(validity["array"])
        for array in arrays:
            require(isinstance(array, dict), "reader array report is absent")
            shards = array.get("stored_shards")
            require(isinstance(shards, list), "reader stored-shard report is absent")
            for shard in shards:
                require(isinstance(shard, dict), "reader shard report is not an object")
                require("bytes" in shard and "sha256" in shard, "encoded shard facts are absent")
                del shard["bytes"]
                del shard["sha256"]
    return result


def stable_package(package: Any) -> dict[str, Any]:
    require(isinstance(package, dict), "reader package report is not an object")
    keys = [
        "compatibility",
        "declared_scientific_content_id",
        "directories",
        "files",
        "manifest_descriptors",
        "manifest_pages",
        "ome_interoperability_base",
        "required_capabilities",
    ]
    require(all(key in package for key in keys), "stable package facts are incomplete")
    return {key: package[key] for key in keys}


def compare_expected_facts(observed: dict[str, Any], facts: dict[str, Any]) -> None:
    package = observed["package"]
    image = observed["image"]
    require(
        package["declared_scientific_content_id"] == facts["scientific_content_id"],
        "writer output scientific identity differs from independent facts",
    )
    expected_mapping = [row["physical_channel"] for row in facts["physical_mapping"]]
    require(
        image["logical_to_physical_channels"] == expected_mapping,
        "writer output logical/physical mapping differs from independent facts",
    )
    require(len(image["levels"]) == len(facts["levels"]), "writer output level count differs")
    for level, expected_level in zip(image["levels"], facts["levels"], strict=True):
        require(level["ordinal"] == expected_level["ordinal"], "writer output level order differs")
        pixel = level["pixel"]
        validity = level["validity"]
        require(
            pixel["logical_layer_major_ctzyx_le_sha256"] == expected_level["raw_values_sha256"],
            "writer output raw-value digest differs from independent facts",
        )
        require(
            pixel["canonical_logical_layer_major_ctzyx_le_sha256"]
            == expected_level["canonical_values_sha256"],
            "writer output canonical-value digest differs from independent facts",
        )
        require(
            validity["logical_layer_packed_lsb0_sha256"] == expected_level["validity_sha256"],
            "writer output validity digest differs from independent facts",
        )
        require(len(level["layers"]) == len(expected_level["layers"]), "layer count differs")
        for layer, expected_layer in zip(level["layers"], expected_level["layers"], strict=True):
            require(
                layer["logical_layer"] == expected_layer["logical_layer"]
                and layer["physical_channel"] == expected_layer["physical_channel"],
                "writer output layer mapping differs",
            )
            require(
                layer["raw_values_c_order_le_sha256"] == expected_layer["raw_values_sha256"],
                "writer output layer raw digest differs",
            )
            require(
                layer["canonical_values_c_order_le_sha256"]
                == expected_layer["canonical_values_sha256"],
                "writer output layer canonical digest differs",
            )
            require(
                layer["validity_packed_lsb0_sha256"] == expected_layer["validity_sha256"],
                "writer output layer validity digest differs",
            )


def validate_output_ome_schema(
    validator: ModuleType,
    case_id: str,
    package: Path,
) -> dict[str, str]:
    metadata_path = package / "images/i00000000/zarr.json"
    try:
        document = validator.decode_json(
            metadata_path.read_bytes(),
            f"Mirante writer OME metadata for {case_id}",
        )
        require(isinstance(document, dict), f"OME metadata is not an object for {case_id}")
        attributes = document.get("attributes")
        require(isinstance(attributes, dict), f"OME attributes are absent for {case_id}")
        validator.validate_ome_attributes(attributes)
    except validator.ValidationError as error:
        raise ConformanceError(f"official OME schema rejected {case_id}: {error}") from error
    return {"case_id": case_id, "status": "passed"}


def read_outputs(
    reader_module: ModuleType,
    schema_validator: ModuleType,
    reader_python: Path,
    run_root: Path,
    output_root: Path,
    facts: dict[str, Any],
    promoted: dict[str, Any],
) -> tuple[list[dict[str, Any]], list[dict[str, str]]]:
    report_root = run_root / "reader-reports"
    report_root.mkdir()
    facts_by_case = {row["case_id"]: row for row in facts["cases"]}
    promoted_by_case = {row["case_id"]: row for row in promoted["cases"]}
    require(set(facts_by_case) == set(CASE_IDS), "independent fact case closure drifted")
    require(set(promoted_by_case) == set(CASE_IDS), "promoted report case closure drifted")
    rows = []
    schema_rows = []
    for case_id in CASE_IDS:
        schema_rows.append(
            validate_output_ome_schema(schema_validator, case_id, output_root / case_id)
        )
        report_path = report_root / f"{case_id}.json"
        observed = reader_module.call_reader(
            reader_python,
            case_id,
            output_root / case_id,
            report_path,
        )
        reference = promoted_by_case[case_id]
        require(observed.get("status") == "passed", f"reader did not pass {case_id}")
        require(observed.get("reader_id") == reference.get("reader_id"), "reader identity drifted")
        require(
            observed.get("reader_source_sha256") == reference.get("reader_source_sha256"),
            "reader source observation drifted",
        )
        require(
            semantic_image(observed.get("image")) == semantic_image(reference.get("image")),
            f"writer output semantic image differs for {case_id}",
        )
        require(
            stable_package(observed.get("package")) == stable_package(reference.get("package")),
            f"writer output stable package facts differ for {case_id}",
        )
        compare_expected_facts(observed, facts_by_case[case_id])
        output_rows = tree_rows(output_root / case_id)
        rows.append(
            {
                "case_id": case_id,
                "output_file_count": len(output_rows),
                "output_regular_bytes": sum(row["bytes"] for row in output_rows),
                "output_tree_sha256": sha256_bytes(canonical_json(output_rows)),
                "observed_package_id": observed["package"]["observed_package_id"],
                "promoted_package_id": reference["package"]["observed_package_id"],
                "exact_package_equal": observed["package"]["observed_package_id"]
                == reference["package"]["observed_package_id"],
                "reader_report": file_fact(report_path, run_root),
                "semantic_image_sha256": sha256_bytes(canonical_json(semantic_image(observed["image"]))),
            }
        )
    return rows, schema_rows


def repository_state() -> dict[str, Any]:
    commit = command_output(["git", "rev-parse", "HEAD"])
    tree = command_output(["git", "rev-parse", "HEAD^{tree}"])
    status = subprocess.run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=60,
    )
    require(status.returncode == 0, "cannot inspect repository state")
    return {
        "commit": commit,
        "tree": tree,
        "worktree_clean": status.stdout == b"",
        "porcelain_sha256": sha256_bytes(status.stdout),
    }


def reproduce(requested: Path | None) -> Path:
    manifest, facts, promoted = validate_authority_bindings()
    reader_support = load_reproducer()
    schema_validator = load_validator()
    schema_identity = prepare_schema_validator(schema_validator, promoted)
    run_root, run_id = fresh_run_root(requested)
    output_root, writer_command = run_writer_test(run_root)
    work_root = run_root / "work"
    work_root.mkdir()
    reader_python = reader_support.setup_reader_environment(work_root)
    try:
        cases, schema_cases = read_outputs(
            reader_support,
            schema_validator,
            reader_python,
            run_root,
            output_root,
            facts,
            promoted,
        )
        require(
            schema_cases == promoted["official_schema"]["cases"],
            "writer official-schema case results drifted",
        )
        uv_name = shutil.which("uv")
        require(uv_name is not None, "uv disappeared after reader provisioning")
        uv = Path(uv_name).resolve(strict=True)
        report = {
            "schema": "mirante4d-wp10a-d-production-writer-independent-readback",
            "schema_version": 1,
            "status": "passed",
            "run_id": run_id,
            "repository": repository_state(),
            "authority": {
                "manifest_sha256": sha256_file(MANIFEST),
                "expected_facts_sha256": sha256_file(EXPECTED_FACTS),
                "promoted_reader_report_sha256": sha256_file(PROMOTED_READER_REPORT),
                "authority_binding_sha256": manifest["reproduction"]["authority_binding_sha256"],
            },
            "sources": {
                "runner": file_fact(Path(__file__)),
                "writer_test": file_fact(WRITER_TEST),
                "reader": file_fact(READER),
                "reader_lock": file_fact(READER_LOCK),
                "reader_provisioner": file_fact(REPRODUCER),
            },
            "tools": {
                "python": {
                    "version": command_output([str(reader_support.PYTHON), "--version"]),
                    "sha256": sha256_file(reader_support.PYTHON),
                },
                "uv": {
                    "version": command_output([str(uv), "--version"]),
                    "sha256": sha256_file(uv),
                },
                "zstd": {
                    "version": command_output([str(reader_support.ZSTD), "--version"]),
                    "sha256": sha256_file(reader_support.ZSTD),
                },
            },
            "writer_test": writer_command,
            "official_schema": {**schema_identity, "cases": schema_cases},
            "cases": cases,
            "claim": "semantic-equivalence-only; exact PackageId and encoded shard bytes may differ",
        }
        report_path = run_root / "report.json"
        write_json(report_path, report)
        return report_path
    finally:
        shutil.rmtree(work_root, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=Path,
        help="fresh direct child of target/mirante4d/production-conformance",
    )
    arguments = parser.parse_args()
    report = reproduce(arguments.output)
    print(canonical_json({"result": "passed", "report": report.relative_to(ROOT).as_posix()}).decode("utf-8"))


if __name__ == "__main__":
    try:
        main()
    except (ConformanceError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(f"production conformance failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
