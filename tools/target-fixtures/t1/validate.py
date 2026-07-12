#!/usr/bin/env python3
"""Fail-closed validation for the promoted WP-10A target T1 corpus."""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import io
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unicodedata
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
SCHEMA = ROOT / "docs/plans/active/foundation-refactor/schemas/foundation-target-fixture-manifest-v1.schema.json"
OME_SCHEMA = ROOT / "verification/standards/ome-ngff-0.5.2/0.5/schemas/image.schema"
OME_VERSION = ROOT / "verification/standards/ome-ngff-0.5.2/0.5/schemas/_version.schema"
FINAL_PREFIX = "fixtures/target/"
CASE_IDS = [
    "m4d-t1-u8-2d-sparse",
    "m4d-t1-u16-3d-multiscale",
    "m4d-t1-f32-3d-validity",
]
OME_VERSION_ID = "https://ngff.openmicroscopy.org/0.5/schemas/_version.schema"
SCHEMA_VALIDATOR_ID = "Mirante4D offline JSON Schema subset v1"
SCHEMA_KEYWORDS = {
    "$defs",
    "$id",
    "$ref",
    "$schema",
    "additionalProperties",
    "allOf",
    "const",
    "contains",
    "description",
    "enum",
    "items",
    "maxContains",
    "maxItems",
    "maxLength",
    "maxProperties",
    "maximum",
    "minContains",
    "minItems",
    "minLength",
    "minProperties",
    "minimum",
    "not",
    "oneOf",
    "pattern",
    "properties",
    "propertyNames",
    "required",
    "title",
    "type",
    "uniqueItems",
}


class ValidationError(RuntimeError):
    """The corpus failed one exact validation rule."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValidationError(message)


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def decode_json(encoded: bytes, label: str) -> Any:
    def pairs(rows: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in rows:
            require(key not in result, f"duplicate JSON key {key!r}: {label}")
            result[key] = value
        return result

    def finite_float(value: str) -> float:
        result = float(value)
        require(math.isfinite(result), f"non-finite JSON number: {label}")
        return result

    def reject_constant(value: str) -> None:
        raise ValidationError(f"non-finite JSON constant {value!r}: {label}")

    try:
        return json.loads(
            encoded,
            object_pairs_hook=pairs,
            parse_float=finite_float,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValidationError(f"invalid JSON: {label}") from error


def load_json(path: Path) -> Any:
    try:
        return decode_json(path.read_bytes(), str(path))
    except OSError as error:
        raise ValidationError(f"cannot read JSON: {path}") from error


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def checked_path(value: Any) -> str:
    require(isinstance(value, str) and value and value.isascii(), "repository path must be ASCII")
    path = PurePosixPath(value)
    require(
        not path.is_absolute()
        and "\\" not in value
        and "." not in path.parts
        and ".." not in path.parts
        and "//" not in value,
        f"unsafe repository path {value!r}",
    )
    return value


def resolve_reference(manifest_path: Path, declared: str) -> Path:
    declared = checked_path(declared)
    relative_manifest = manifest_path.relative_to(ROOT).as_posix() if ROOT in manifest_path.parents else ""
    if declared.startswith(FINAL_PREFIX) and relative_manifest != "fixtures/target/manifest.json":
        return manifest_path.parent / declared.removeprefix(FINAL_PREFIX)
    return ROOT / declared


def audit_authority_directory(manifest_path: Path, manifest: dict[str, Any]) -> None:
    require(manifest_path.name == "manifest.json", "target authority manifest must be named manifest.json")
    expected_archive_paths = {
        case_id: f"fixtures/target/archives/{case_id}.tar"
        for case_id in CASE_IDS
    }
    observed_archive_paths = {
        row["case_id"]: row["path"]
        for row in manifest["archives"]
    }
    require(observed_archive_paths == expected_archive_paths, "archive case/path mapping drifted")
    expected_authority_paths = {
        "expected_facts": "fixtures/target/expected-facts.json",
        "identity_vectors": "fixtures/target/identity-vectors.json",
        "mutations": "fixtures/target/mutations.json",
        "independent_reader_report": "fixtures/target/independent-reader-report.json",
        "reproduction_report": "fixtures/target/reproduction-report.json",
    }
    observed_authority_paths = {
        name: row["path"]
        for name, row in manifest["authority_files"].items()
    }
    require(observed_authority_paths == expected_authority_paths, "authority file/path mapping drifted")

    expected_files = {
        "manifest.json",
        *(value.removeprefix(FINAL_PREFIX) for value in expected_archive_paths.values()),
        *(value.removeprefix(FINAL_PREFIX) for value in expected_authority_paths.values()),
    }
    expected_directories = {"archives"}
    observed_files: set[str] = set()
    observed_directories: set[str] = set()

    def visit(directory: Path, prefix: PurePosixPath) -> None:
        with os.scandir(directory) as entries:
            for entry in entries:
                relative = (prefix / entry.name).as_posix()
                facts = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(facts.st_mode):
                    observed_directories.add(relative)
                    visit(Path(entry.path), prefix / entry.name)
                elif stat.S_ISREG(facts.st_mode):
                    require(facts.st_nlink == 1, f"hardlinked authority file is forbidden: {relative}")
                    observed_files.add(relative)
                else:
                    raise ValidationError(f"symlink or special authority entry is forbidden: {relative}")

    authority_root = manifest_path.parent
    root_facts = authority_root.lstat()
    require(stat.S_ISDIR(root_facts.st_mode) and not authority_root.is_symlink(), "authority root must be a real directory")
    visit(authority_root, PurePosixPath())
    require(observed_directories == expected_directories, "target authority directory set is not exact")
    require(observed_files == expected_files, "target authority file set is not exact")


def audit_schema(schema: Any, label: str) -> None:
    """Reject schemas that use anything outside this validator's closed subset."""

    require(isinstance(schema, (dict, bool)), f"{label}: schema must be an object or boolean")
    if isinstance(schema, bool):
        return
    unknown = set(schema) - SCHEMA_KEYWORDS
    require(not unknown, f"{label}: unsupported schema keyword(s): {sorted(unknown)}")
    for name in ["$schema", "$id", "$ref", "title", "description", "pattern"]:
        if name in schema:
            require(isinstance(schema[name], str), f"{label}: {name} must be a string")
    if "type" in schema:
        allowed = {"array", "boolean", "integer", "null", "number", "object", "string"}
        value = schema["type"]
        require(
            (isinstance(value, str) and value in allowed)
            or (
                isinstance(value, list)
                and value
                and len(value) == len(set(value))
                and all(isinstance(row, str) and row in allowed for row in value)
            ),
            f"{label}: invalid type declaration",
        )
    if "required" in schema:
        rows = schema["required"]
        require(
            isinstance(rows, list)
            and len(rows) == len(set(rows))
            and all(isinstance(row, str) for row in rows),
            f"{label}: required must contain unique strings",
        )
    if "enum" in schema:
        require(isinstance(schema["enum"], list) and schema["enum"], f"{label}: enum must be nonempty")
    for name in ["minimum", "maximum"]:
        if name in schema:
            require(
                isinstance(schema[name], (int, float)) and not isinstance(schema[name], bool),
                f"{label}: {name} must be numeric",
            )
    for name in [
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minContains",
        "maxContains",
        "minProperties",
        "maxProperties",
    ]:
        if name in schema:
            require(isinstance(schema[name], int) and not isinstance(schema[name], bool) and schema[name] >= 0, f"{label}: {name} must be a nonnegative integer")
    if "uniqueItems" in schema:
        require(isinstance(schema["uniqueItems"], bool), f"{label}: uniqueItems must be boolean")
    for name in ["$defs", "properties"]:
        if name in schema:
            require(isinstance(schema[name], dict), f"{label}: {name} must be an object")
            for key, child in schema[name].items():
                require(isinstance(key, str), f"{label}: {name} key must be a string")
                audit_schema(child, f"{label}/{name}/{key}")
    for name in ["additionalProperties", "contains", "items", "not", "propertyNames"]:
        if name in schema:
            audit_schema(schema[name], f"{label}/{name}")
    for name in ["allOf", "oneOf"]:
        if name in schema:
            require(isinstance(schema[name], list) and schema[name], f"{label}: {name} must be nonempty")
            for index, child in enumerate(schema[name]):
                audit_schema(child, f"{label}/{name}/{index}")


def json_equal(left: Any, right: Any) -> bool:
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return left == right
    if type(left) is not type(right):
        return False
    if isinstance(left, list):
        return len(left) == len(right) and all(json_equal(a, b) for a, b in zip(left, right, strict=True))
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(json_equal(left[key], right[key]) for key in left)
    return left == right


def schema_type_matches(instance: Any, expected: str) -> bool:
    return {
        "array": isinstance(instance, list),
        "boolean": isinstance(instance, bool),
        "integer": (
            isinstance(instance, int)
            and not isinstance(instance, bool)
        )
        or (
            isinstance(instance, float)
            and math.isfinite(instance)
            and instance.is_integer()
        ),
        "null": instance is None,
        "number": (
            isinstance(instance, (int, float))
            and not isinstance(instance, bool)
            and (not isinstance(instance, float) or math.isfinite(instance))
        ),
        "object": isinstance(instance, dict),
        "string": isinstance(instance, str),
    }[expected]


def resolve_schema_reference(reference: str, root: dict[str, Any], external: dict[str, dict[str, Any]]) -> tuple[Any, dict[str, Any]]:
    if reference.startswith("#/"):
        value: Any = root
        for raw in reference[2:].split("/"):
            token = raw.replace("~1", "/").replace("~0", "~")
            require(isinstance(value, dict) and token in value, f"unresolved local schema reference: {reference}")
            value = value[token]
        return value, root
    require(reference in external, f"unresolved external schema reference: {reference}")
    return external[reference], external[reference]


def schema_validate(
    instance: Any,
    schema: Any,
    root: dict[str, Any],
    external: dict[str, dict[str, Any]],
    path: str = "$",
) -> None:
    if isinstance(schema, bool):
        require(schema, f"{path}: rejected by false schema")
        return
    if "$ref" in schema:
        target, target_root = resolve_schema_reference(schema["$ref"], root, external)
        schema_validate(instance, target, target_root, external, path)

    expected = schema.get("type")
    if expected is not None:
        choices = [expected] if isinstance(expected, str) else expected
        require(any(schema_type_matches(instance, choice) for choice in choices), f"{path}: type mismatch")
    if "const" in schema:
        require(json_equal(instance, schema["const"]), f"{path}: const mismatch")
    if "enum" in schema:
        require(any(json_equal(instance, choice) for choice in schema["enum"]), f"{path}: enum mismatch")
    for child in schema.get("allOf", []):
        schema_validate(instance, child, root, external, path)
    if "oneOf" in schema:
        matches = 0
        for child in schema["oneOf"]:
            try:
                schema_validate(instance, child, root, external, path)
                matches += 1
            except ValidationError:
                pass
        require(matches == 1, f"{path}: oneOf matched {matches} branches")
    if "not" in schema:
        try:
            schema_validate(instance, schema["not"], root, external, path)
        except ValidationError:
            pass
        else:
            raise ValidationError(f"{path}: not schema matched")

    if isinstance(instance, dict):
        properties = schema.get("properties", {})
        missing = [name for name in schema.get("required", []) if name not in instance]
        require(not missing, f"{path}: missing required properties {missing}")
        if "minProperties" in schema:
            require(len(instance) >= schema["minProperties"], f"{path}: too few properties")
        if "maxProperties" in schema:
            require(len(instance) <= schema["maxProperties"], f"{path}: too many properties")
        if "propertyNames" in schema:
            for name in instance:
                schema_validate(name, schema["propertyNames"], root, external, f"{path}/<property-name>")
        for name, value in instance.items():
            if name in properties:
                schema_validate(value, properties[name], root, external, f"{path}/{name}")
            elif "additionalProperties" in schema:
                additional = schema["additionalProperties"]
                require(additional is not False, f"{path}: additional property {name!r}")
                if additional is not True:
                    schema_validate(value, additional, root, external, f"{path}/{name}")

    if isinstance(instance, list):
        if "minItems" in schema:
            require(len(instance) >= schema["minItems"], f"{path}: too few items")
        if "maxItems" in schema:
            require(len(instance) <= schema["maxItems"], f"{path}: too many items")
        if schema.get("uniqueItems"):
            require(
                all(not json_equal(left, right) for index, left in enumerate(instance) for right in instance[index + 1 :]),
                f"{path}: duplicate items",
            )
        if "items" in schema:
            for index, value in enumerate(instance):
                schema_validate(value, schema["items"], root, external, f"{path}/{index}")
        if "contains" in schema:
            matches = 0
            for value in instance:
                try:
                    schema_validate(value, schema["contains"], root, external, path)
                    matches += 1
                except ValidationError:
                    pass
            minimum = schema.get("minContains", 1)
            require(matches >= minimum, f"{path}: contains matched fewer than {minimum} items")
            if "maxContains" in schema:
                require(matches <= schema["maxContains"], f"{path}: contains matched too many items")

    if isinstance(instance, str):
        if "minLength" in schema:
            require(len(instance) >= schema["minLength"], f"{path}: string is too short")
        if "maxLength" in schema:
            require(len(instance) <= schema["maxLength"], f"{path}: string is too long")
        if "pattern" in schema:
            require(re.search(schema["pattern"], instance) is not None, f"{path}: pattern mismatch")
    if isinstance(instance, (int, float)) and not isinstance(instance, bool):
        if "minimum" in schema:
            require(instance >= schema["minimum"], f"{path}: number is below minimum")
        if "maximum" in schema:
            require(instance <= schema["maximum"], f"{path}: number is above maximum")


def validate_schemas(manifest_path: Path) -> None:
    manifest_schema = load_json(SCHEMA)
    ome_schema = load_json(OME_SCHEMA)
    ome_version = load_json(OME_VERSION)
    require(all(isinstance(row, dict) for row in [manifest_schema, ome_schema, ome_version]), "schema root must be an object")
    audit_schema(manifest_schema, "target manifest schema")
    audit_schema(ome_schema, "OME image schema")
    audit_schema(ome_version, "OME version schema")
    external = {OME_VERSION_ID: ome_version}
    schema_validate(load_json(manifest_path), manifest_schema, manifest_schema, external)


def validate_ome_attributes(attributes: dict[str, Any]) -> None:
    ome_schema = load_json(OME_SCHEMA)
    ome_version = load_json(OME_VERSION)
    require(isinstance(ome_schema, dict) and isinstance(ome_version, dict), "OME schema root must be an object")
    schema_validate(attributes, ome_schema, ome_schema, {OME_VERSION_ID: ome_version})


def tar_path(value: str) -> str:
    value = value[:-1] if value.endswith("/") else value
    require(value and value.isascii() and len(value.encode("ascii")) <= 240, "invalid archive path")
    path = PurePosixPath(value)
    require(
        not path.is_absolute()
        and "\\" not in value
        and "//" not in value
        and "." not in path.parts
        and ".." not in path.parts
        and path.as_posix() == value,
        "unsafe archive path",
    )
    require(unicodedata.normalize("NFC", value) == value, "archive path is not NFC")
    return value


def inspect_archive(
    path: Path,
    declaration: dict[str, Any],
    limits: dict[str, Any],
    extract_destination: Path | None,
) -> dict[str, Any]:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as source:
        opened = os.fstat(source.fileno())
        require(
            stat.S_ISREG(opened.st_mode)
            and opened.st_nlink == 1
            and 0 < opened.st_size <= limits["archive_bytes_each"]
            and opened.st_size == declaration["bytes"],
            "archive type or byte count exceeds its declaration/limit",
        )
        encoded_archive = source.read(limits["archive_bytes_each"] + 1)
        require(len(encoded_archive) == opened.st_size and source.read(1) == b"", "archive changed or exceeded its byte limit")
    require(sha256(encoded_archive) == declaration["sha256"], "archive digest mismatch")
    require(len(encoded_archive) % 512 == 0, "archive is not block aligned")

    names: set[str] = set()
    folded: set[str] = set()
    directories: list[str] = []
    files: dict[str, dict[str, Any]] = {}
    contents: dict[str, bytes] = {}
    member_end = 0
    try:
        archive = tarfile.open(fileobj=io.BytesIO(encoded_archive), mode="r:")
    except tarfile.TarError as error:
        raise ValidationError("archive parser rejected USTAR bytes") from error
    with archive:
        members = archive.getmembers()
        for member in members:
            name = tar_path(member.name)
            require(name not in names and name.casefold() not in folded, "duplicate/colliding archive path")
            names.add(name)
            folded.add(name.casefold())
            require(not member.pax_headers and not member.linkname, "archive extensions or links are forbidden")
            require(member.uid == 0 and member.gid == 0 and member.mtime == 0, "archive ownership/time is not normalized")
            require(member.uname == "" and member.gname == "", "archive owner names are not normalized")
            header = encoded_archive[member.offset : member.offset + 512]
            require(header[257:263] == b"ustar\0" and header[263:265] == b"00", "member is not normalized USTAR")
            member_end = max(member_end, member.offset_data + ((member.size + 511) // 512) * 512)
            if member.isdir():
                require(
                    header[156:157] == b"5" and member.mode == 0o755 and member.size == 0,
                    "directory metadata is not normalized",
                )
                directories.append(name)
                continue
            require(member.isfile() and header[156:157] == b"0", "archive contains a noncanonical file type")
            require(member.mode == 0o644 and 0 <= member.size <= limits["individual_file_bytes"], "file metadata exceeds limits")
            source = archive.extractfile(member)
            require(source is not None, "archive member is unreadable")
            data = source.read()
            require(len(data) == member.size, "archive member is short")
            padding_end = member.offset_data + ((member.size + 511) // 512) * 512
            require(not any(encoded_archive[member.offset_data + member.size : padding_end]), "archive member padding is nonzero")
            files[name] = {"bytes": len(data), "sha256": sha256(data)}
            contents[name] = data

    require(len(encoded_archive) - member_end >= 1024 and not any(encoded_archive[member_end:]), "archive terminator/padding is not canonical")
    directories.sort()
    files = dict(sorted(files.items()))
    require([member.name.rstrip("/") for member in members] == [*directories, *files], "archive member order is not canonical")
    directory_set = set(directories)
    for name in [*directories, *files]:
        parent = PurePosixPath(name).parent
        while parent != PurePosixPath("."):
            require(parent.as_posix() in directory_set, "archive omits an ancestor directory")
            parent = parent.parent
    child_counts: dict[str, int] = {}
    for name in [*directories, *files]:
        parent = PurePosixPath(name).parent.as_posix()
        child_counts["" if parent == "." else parent] = child_counts.get("" if parent == "." else parent, 0) + 1
    inventory = {
        "file_count": len(files),
        "directory_count": len(directories),
        "regular_file_bytes": sum(row["bytes"] for row in files.values()),
        "max_depth": max(len(PurePosixPath(name).parent.parts) for name in files),
        "max_fan_out": max(child_counts.values()),
        "max_path_bytes": max(len(name.encode("ascii")) for name in [*directories, *files]),
        "max_file_bytes": max(row["bytes"] for row in files.values()),
        "directories": directories,
        "files": files,
    }
    require(inventory == declaration["inventory"], "archive inventory disagrees with manifest")
    for observed, key in [
        (inventory["file_count"], "files_each"),
        (inventory["directory_count"], "directories_each"),
        (inventory["max_depth"], "depth"),
        (inventory["max_fan_out"], "fan_out"),
        (inventory["max_path_bytes"], "path_bytes"),
        (inventory["max_file_bytes"], "individual_file_bytes"),
    ]:
        require(observed <= limits[key], f"archive exceeds {key}")
    require(inventory["regular_file_bytes"] <= limits["combined_unpacked_regular_file_bytes"], "archive bytes exceed corpus limit")
    require(inventory["regular_file_bytes"] <= len(encoded_archive) * limits["compression_ratio"], "archive ratio exceeds limit")

    root_bytes = contents.get("m4d/manifest/root.json")
    require(root_bytes is not None, "package manifest root is absent")
    require(declaration["package_id"] == f"m4d-package-v1-sha256:{sha256(root_bytes)}", "PackageId mismatch")
    tree_rows = [{"path": name, **facts} for name, facts in files.items()]
    require(declaration["tree_sha256"] == sha256(canonical_json(tree_rows) + b"\n"), "package tree digest mismatch")

    ome = decode_json(contents["images/i00000000/zarr.json"], "OME image zarr.json")
    attributes = ome.get("attributes")
    require(isinstance(attributes, dict), "OME attributes are absent")
    validate_ome_attributes(attributes)
    axes = [axis["name"] for axis in attributes["ome"]["multiscales"][0]["axes"]]
    for dataset in attributes["ome"]["multiscales"][0]["datasets"]:
        pixel = decode_json(
            contents[f"images/i00000000/{dataset['path']}/zarr.json"],
            f"OME dataset {dataset['path']} zarr.json",
        )
        require(pixel.get("dimension_names") == axes, "OME axes and Zarr dimension_names disagree")

    if extract_destination is not None:
        require(not extract_destination.exists(), "extraction destination must be absent")
        extract_destination.mkdir(mode=0o700)
        for directory in directories:
            (extract_destination / directory).mkdir(mode=0o755, parents=True, exist_ok=True)
        for name, data in contents.items():
            target = extract_destination / name
            target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            with target.open("xb") as output:
                output.write(data)
            target.chmod(0o644)
    return inventory


def reference_file(manifest_path: Path, reference: dict[str, Any]) -> tuple[Path, Any]:
    path = resolve_reference(manifest_path, reference["path"])
    require(path.is_file() and not path.is_symlink(), f"authority file is absent: {reference['path']}")
    require(sha256_file(path) == reference["sha256"], f"authority digest mismatch: {reference['path']}")
    value = load_json(path)
    require(isinstance(value, dict) and value.get("schema") == reference["schema"], f"authority schema mismatch: {reference['path']}")
    return path, value


def authority_binding(manifest: dict[str, Any]) -> tuple[dict[str, Any], str]:
    lineages = manifest["lineages"]
    preimage = {
        "archives": [
            {"case_id": row["case_id"], "sha256": row["sha256"]}
            for row in manifest["archives"]
        ],
        "authority_files": {
            name: manifest["authority_files"][name]["sha256"]
            for name in ["expected_facts", "identity_vectors", "mutations"]
        },
        "lineages": {
            name: {
                "source_sha256": lineages[name]["source_sha256"],
                "lock_sha256": lineages[name]["lock_sha256"],
            }
            for name in ["producer", "fact_oracle", "independent_reader"]
        },
    }
    return preimage, sha256(canonical_json(preimage))


def compare_reader_facts(
    facts: dict[str, Any],
    reader: dict[str, Any],
    archives: dict[str, dict[str, Any]],
) -> None:
    require(reader.get("status") == "passed", "independent reader report did not pass")
    expected_cases = {row["case_id"]: row for row in facts["cases"]}
    observed_cases = {row["case_id"]: row for row in reader.get("cases", [])}
    require(set(expected_cases) == set(CASE_IDS) == set(observed_cases), "reader/fact case set mismatch")
    for case_id in CASE_IDS:
        expected = expected_cases[case_id]
        observed = observed_cases[case_id]
        package = observed["package"]
        require(observed.get("status") == "passed" and observed.get("reader_id") == "TGT-READER-001", "reader case did not pass")
        require(package["observed_package_id"] == archives[case_id]["package_id"], "reader PackageId mismatch")
        require(package["declared_scientific_content_id"] == expected["scientific_content_id"], "reader scientific ID mismatch")
        mapping = [row["physical_channel"] for row in expected["physical_mapping"]]
        require(observed["image"]["logical_to_physical_channels"] == mapping, "reader channel mapping mismatch")
        axes = observed["image"]["axes"]
        require([axis["name"] for axis in axes] == ["t", "c", "z", "y", "x"], "reader OME axis order mismatch")
        expected_spatial_unit = "micrometer" if expected["ome_projection"] == "diagonal_micrometer" else None
        require([axis.get("unit") for axis in axes[2:]] == [expected_spatial_unit] * 3, "reader OME spatial units mismatch")
        science_layers = observed["image"]["science_layers"]
        require(len(science_layers) == len(mapping), "reader science layer count mismatch")
        for layer in science_layers:
            require(layer["dtype"] == expected["dtype"], "reader science dtype mismatch")
            require([int(value) for value in layer["base_shape_tzyx"]] == [expected["shape_tczyx"][index] for index in [0, 2, 3, 4]], "reader science shape mismatch")
            require(layer["grid_to_world_micrometer_f64_bits"] == expected["grid_to_world_f64_bits"], "reader scientific transform mismatch")
            require(layer["temporal_step_f64_bits"] == expected["temporal_step_f64_bits"], "reader temporal calibration mismatch")
        observed_ome = observed["image"]["ome_datasets"]
        require(len(observed_ome) == len(expected["ome_levels"]), "reader OME level count mismatch")
        for expected_ome, observed_ome_level in zip(expected["ome_levels"], observed_ome, strict=True):
            transformations = observed_ome_level["transformations"]
            expected_transformations = [
                {
                    "type": "scale",
                    "f64_bits": [
                        expected["temporal_step_f64_bits"],
                        "3ff0000000000000",
                        *expected_ome["scale_zyx_f64_bits"],
                    ],
                }
            ]
            translation = ["0000000000000000", "0000000000000000", *expected_ome["translation_zyx_f64_bits"]]
            if any(value != "0000000000000000" for value in translation):
                expected_transformations.append({"type": "translation", "f64_bits": translation})
            require(observed_ome_level["path"] == f"s{expected_ome['ordinal']:02d}", "reader OME path mismatch")
            require(transformations == expected_transformations, "reader OME transform mismatch")
        levels = {row["ordinal"]: row for row in observed["image"]["levels"]}
        require(set(levels) == set(range(expected["level_count"])), "reader level set mismatch")
        for expected_level in expected["levels"]:
            level = levels[expected_level["ordinal"]]
            pixel = level["pixel"]
            validity = level["validity"]
            require(pixel["shape"] == expected_level["shape_tczyx"], "reader shape mismatch")
            require(pixel["dtype"] == expected["dtype"], "reader dtype mismatch")
            require(pixel["logical_layer_major_ctzyx_le_sha256"] == expected_level["raw_values_sha256"], "reader raw digest mismatch")
            require(pixel["canonical_logical_layer_major_ctzyx_le_sha256"] == expected_level["canonical_values_sha256"], "reader canonical digest mismatch")
            require(validity["logical_layer_packed_lsb0_sha256"] == expected_level["validity_sha256"], "reader validity digest mismatch")
            observed_layers = {row["logical_layer"]: row for row in level["layers"]}
            for expected_layer in expected_level["layers"]:
                layer = observed_layers[expected_layer["logical_layer"]]
                require(layer["physical_channel"] == expected_layer["physical_channel"], "reader physical channel mismatch")
                require(layer["raw_values_c_order_le_sha256"] == expected_layer["raw_values_sha256"], "reader layer raw digest mismatch")
                require(layer["canonical_values_c_order_le_sha256"] == expected_layer["canonical_values_sha256"], "reader layer canonical digest mismatch")
                require(layer["validity_packed_lsb0_sha256"] == expected_layer["validity_sha256"], "reader layer validity digest mismatch")


def logical_voxel_bytes(facts_case: dict[str, Any]) -> int:
    widths = {"uint8": 1, "uint16": 2, "float32": 4}
    dtype = facts_case.get("dtype")
    require(dtype in widths, "fact authority has an unsupported dtype")
    levels = facts_case.get("levels")
    require(isinstance(levels, list) and levels, "fact authority has no levels")
    total = 0
    for level in levels:
        shape = level.get("shape_tczyx") if isinstance(level, dict) else None
        require(
            isinstance(shape, list)
            and len(shape) == 5
            and all(type(value) is int and value > 0 for value in shape),
            "fact authority has an invalid level shape",
        )
        total += math.prod(shape) * widths[dtype]
    return total


def audit_lineages(manifest_path: Path, manifest: dict[str, Any]) -> None:
    lineages = manifest["lineages"]
    source_paths: set[str] = set()
    source_digests: set[str] = set()
    lock_paths: set[str] = set()
    lock_digests: set[str] = set()
    resolved: dict[str, Path] = {}
    for name in ["producer", "fact_oracle", "independent_reader"]:
        row = lineages[name]
        source = ROOT / checked_path(row["source_path"])
        lock = ROOT / checked_path(row["lock_path"])
        require(source.is_file() and lock.is_file(), f"lineage {name} source/lock is absent")
        require(sha256_file(source) == row["source_sha256"], f"lineage {name} source drifted")
        require(sha256_file(lock) == row["lock_sha256"], f"lineage {name} lock drifted")
        source_paths.add(row["source_path"])
        source_digests.add(row["source_sha256"])
        lock_paths.add(row["lock_path"])
        lock_digests.add(row["lock_sha256"])
        resolved[name] = source
    require(all(len(rows) == 3 for rows in [source_paths, source_digests, lock_paths, lock_digests]), "lineage sources and locks are not pairwise distinct")

    imports: dict[str, set[str]] = {}
    for name in ["producer", "independent_reader"]:
        tree = ast.parse(resolved[name].read_text(encoding="utf-8"))
        roots: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                roots.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                roots.add(node.module.split(".")[0])
        imports[name] = roots
    require(not (imports["producer"] & {"numpy", "zarr", "google_crc32c", "mirante4d"}), "producer lineage imports a forbidden implementation")
    require(imports["producer"] <= set(sys.stdlib_module_names), "producer lineage has an unlocked Python dependency")
    require(not (imports["independent_reader"] & {"mirante4d"}), "reader lineage imports Mirante")
    require("include!" not in resolved["fact_oracle"].read_text(encoding="utf-8"), "fact oracle includes another implementation")

    producer_lock = load_json(ROOT / lineages["producer"]["lock_path"])
    oracle_lock = load_json(ROOT / lineages["fact_oracle"]["lock_path"])
    require(producer_lock.get("lineage_id") == "TGT-PRODUCER-001", "producer lock identity mismatch")
    require(oracle_lock.get("lineage_id") == "TGT-FACT-001", "oracle lock identity mismatch")


def semantic_validate(
    manifest_path: Path,
    manifest: dict[str, Any],
    extract_root: Path | None,
) -> dict[str, Any]:
    require(manifest.get("status") == "independently_validated", "manifest status is not accepted")
    require(
        manifest.get("approvals", {}).get("repository_owner", {}).get("state") == "approved"
        and manifest.get("approvals", {}).get("scientific_content", {}).get("state") == "approved",
        "manifest approvals are incomplete",
    )
    audit_authority_directory(manifest_path, manifest)
    audit_lineages(manifest_path, manifest)
    for path_field, digest_field in [
        ("cases_path", "cases_sha256"),
        ("normative_standards_path", "normative_standards_sha256"),
    ]:
        path = ROOT / checked_path(manifest["profile"][path_field])
        require(path.is_file() and sha256_file(path) == manifest["profile"][digest_field], f"profile binding drifted: {path_field}")

    authority_values: dict[str, dict[str, Any]] = {}
    authority_paths: dict[str, Path] = {}
    for name, reference in manifest["authority_files"].items():
        path, value = reference_file(manifest_path, reference)
        authority_paths[name] = path
        authority_values[name] = value
    require(authority_values["expected_facts"].get("lineage_id") == "TGT-FACT-001", "fact lineage mismatch")
    require(authority_values["independent_reader_report"].get("status") == "passed", "reader report status mismatch")
    require(authority_values["reproduction_report"].get("status") == "passed", "reproduction report status mismatch")

    limits = manifest["limits"]
    archive_rows = manifest["archives"]
    require([row["case_id"] for row in archive_rows] == CASE_IDS, "archive order or case set drifted")
    fact_cases = {
        row["case_id"]: row
        for row in authority_values["expected_facts"].get("cases", [])
        if isinstance(row, dict) and isinstance(row.get("case_id"), str)
    }
    require(set(fact_cases) == set(CASE_IDS), "fact authority case set mismatch")
    logical_bytes = {
        case_id: logical_voxel_bytes(fact_cases[case_id])
        for case_id in CASE_IDS
    }
    for row in archive_rows:
        require(
            row["logical_voxel_bytes"] == logical_bytes[row["case_id"]],
            "archive logical byte count disagrees with independent facts",
        )
    inventories = []
    archives_by_case = {row["case_id"]: row for row in archive_rows}
    for row in archive_rows:
        archive_path = resolve_reference(manifest_path, row["path"])
        require(archive_path.is_file() and not archive_path.is_symlink(), "archive is absent or unsafe")
        inventories.append(
            inspect_archive(
                archive_path,
                row,
                limits,
                None,
            )
        )
    require(sum(row["bytes"] for row in archive_rows) <= limits["combined_archive_bytes"], "combined archive limit exceeded")
    require(sum(row["regular_file_bytes"] for row in inventories) <= limits["combined_unpacked_regular_file_bytes"], "combined unpacked limit exceeded")
    require(sum(logical_bytes.values()) <= limits["combined_logical_voxel_bytes"], "combined logical limit exceeded")

    preimage, binding = authority_binding(manifest)
    require(manifest["reproduction"]["authority_binding_sha256"] == binding, "manifest authority binding mismatch")
    reader_report = authority_values["independent_reader_report"]
    reproduction = authority_values["reproduction_report"]
    require(reader_report.get("authority_binding_sha256") == binding, "reader authority binding mismatch")
    require(reproduction.get("authority_binding_sha256") == binding, "reproduction authority binding mismatch")
    if "authority_binding_preimage" in reproduction:
        require(reproduction["authority_binding_preimage"] == preimage, "recorded binding preimage mismatch")

    generated_paths = [row["path"] for row in archive_rows] + [
        manifest["authority_files"][name]["path"]
        for name in ["expected_facts", "identity_vectors", "mutations", "independent_reader_report"]
    ]
    generated_rows = []
    for declared in sorted(generated_paths):
        path = resolve_reference(manifest_path, declared)
        generated_rows.append(
            {
                "path": declared.removeprefix(FINAL_PREFIX),
                "bytes": path.stat().st_size,
                "sha256": sha256_file(path),
            }
        )
    generated = sha256(canonical_json(generated_rows))
    require(manifest["reproduction"]["generated_tree_sha256"] == generated, "manifest generated-tree digest mismatch")
    require(reproduction.get("generated_tree_sha256") == generated, "reproduction generated-tree digest mismatch")
    if "generated_files" in reproduction:
        require(reproduction["generated_files"] == generated_rows, "reproduction generated-file list mismatch")

    compare_reader_facts(authority_values["expected_facts"], reader_report, archives_by_case)
    official = reader_report.get("official_schema")
    require(isinstance(official, dict), "reader report lacks official-schema results")
    require(official.get("validator") == SCHEMA_VALIDATOR_ID, "reader schema-validator identity mismatch")
    require(official.get("image_schema_sha256") == sha256_file(OME_SCHEMA), "reader OME image-schema digest mismatch")
    require(official.get("version_schema_sha256") == sha256_file(OME_VERSION), "reader OME version-schema digest mismatch")
    require(official.get("cases") == [{"case_id": case_id, "status": "passed"} for case_id in CASE_IDS], "reader official-schema case results mismatch")
    recipes = authority_values["mutations"]["recipes"]
    observed_mutations = {row["id"]: row for row in reader_report.get("mutations", [])}
    require(len(recipes) == 15 and set(observed_mutations) == {row["id"] for row in recipes}, "mutation result set mismatch")
    for recipe in recipes:
        observed = observed_mutations[recipe["id"]]
        require(observed["expected_rejection"] == recipe["expected_rejection"], "mutation expected code mismatch")
        require(observed["reader_result"]["observed_rejection"] == recipe["expected_rejection"], "mutation reader rejection mismatch")

    producer = reproduction.get("producer")
    require(isinstance(producer, dict), "reproduction report lacks producer binding")
    reproducer = reproduction.get("reproducer")
    require(isinstance(reproducer, dict), "reproduction report lacks reproducer binding")
    require(reproducer.get("source_path") == "tools/target-fixtures/t1/reproduce.py", "reproducer path mismatch")
    require(reproducer.get("source_sha256") == sha256_file(ROOT / "tools/target-fixtures/t1/reproduce.py"), "reproducer source mismatch")
    require(producer.get("source_sha256") == manifest["lineages"]["producer"]["source_sha256"], "reproduction producer source mismatch")
    require(producer.get("lock_sha256") == manifest["lineages"]["producer"]["lock_sha256"], "reproduction producer lock mismatch")
    producer_lock = load_json(ROOT / manifest["lineages"]["producer"]["lock_path"])
    locked_tools = {row["name"]: row["installed_sha256"] for row in producer_lock["tools"]}
    require(producer.get("installed_tools") == locked_tools, "reproduction producer tools mismatch")
    outputs = {row["case_id"]: row for row in reproduction.get("outputs", [])}
    require(set(outputs) == set(CASE_IDS), "reproduction output set mismatch")
    for case_id in CASE_IDS:
        require(outputs[case_id]["archive_sha256"] == archives_by_case[case_id]["sha256"], "reproduction archive digest mismatch")
        require(outputs[case_id]["package_id"] == archives_by_case[case_id]["package_id"], "reproduction PackageId mismatch")

    validator_decl = manifest["validator"]
    require(validator_decl["path"] == "tools/target-fixtures/t1/validate.py", "validator path mismatch")
    require(sha256_file(Path(__file__)) == validator_decl["sha256"], "validator source digest mismatch")
    require(
        validator_decl["manifest_schema_path"]
        == "docs/plans/active/foundation-refactor/schemas/foundation-target-fixture-manifest-v1.schema.json",
        "manifest schema path mismatch",
    )
    require(sha256_file(SCHEMA) == validator_decl["manifest_schema_sha256"], "manifest schema digest mismatch")
    vector_verifier = ROOT / checked_path(validator_decl["identity_vector_verifier_path"])
    require(
        validator_decl["identity_vector_verifier_path"]
        == "tools/target-fixtures/t1/hand_vectors/verify_hand_vectors.py",
        "identity-vector verifier path mismatch",
    )
    require(
        vector_verifier.is_file()
        and not vector_verifier.is_symlink()
        and sha256_file(vector_verifier) == validator_decl["identity_vector_verifier_sha256"],
        "identity-vector verifier digest mismatch",
    )
    environment = os.environ.copy()
    environment.update({"LC_ALL": "C", "PYTHONDONTWRITEBYTECODE": "1"})
    vector_result = subprocess.run(
        [sys.executable, str(vector_verifier), "--vectors", str(authority_paths["identity_vectors"])],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
    )
    require(
        vector_result.returncode == 0
        and vector_result.stdout.strip() == "WP-10A-C static hand vectors: PASS",
        "identity-vector verifier failed",
    )
    if extract_root is not None:
        for row in archive_rows:
            inspect_archive(
                resolve_reference(manifest_path, row["path"]),
                row,
                limits,
                extract_root / row["case_id"],
            )
    return {
        "archives": len(archive_rows),
        "authority_binding_sha256": binding,
        "generated_tree_sha256": generated,
        "mutations": len(recipes),
    }


def expect_failure(action: Any, label: str) -> None:
    try:
        action()
    except (ValidationError, KeyError, TypeError, ValueError, OSError):
        return
    raise ValidationError(f"negative self-test was accepted: {label}")


def self_test(
    manifest_path: Path,
    manifest: dict[str, Any],
    temporary: Path,
) -> None:
    mutations = [
        ("archive digest", lambda value: value["archives"][0].__setitem__("sha256", "0" * 64)),
        ("binding", lambda value: value["reproduction"].__setitem__("authority_binding_sha256", "0" * 64)),
        ("lineage collision", lambda value: value["lineages"]["producer"].__setitem__("source_sha256", value["lineages"]["fact_oracle"]["source_sha256"])),
        ("approval", lambda value: value["approvals"]["repository_owner"].__setitem__("state", "pending")),
        ("manifest schema binding", lambda value: value["validator"].__setitem__("manifest_schema_sha256", "0" * 64)),
        ("identity-vector verifier binding", lambda value: value["validator"].__setitem__("identity_vector_verifier_sha256", "0" * 64)),
        ("logical byte understatement", lambda value: value["archives"][0].__setitem__("logical_voxel_bytes", 1)),
    ]
    for label, mutate in mutations:
        candidate = copy.deepcopy(manifest)
        mutate(candidate)
        expect_failure(
            lambda candidate=candidate: semantic_validate(
                manifest_path,
                candidate,
                None,
            ),
            label,
        )

    first = manifest["archives"][0]
    source = resolve_reference(manifest_path, first["path"])
    tampered = temporary / "tampered.tar"
    encoded = source.read_bytes() + b"X" * 512
    tampered.write_bytes(encoded)
    declaration = copy.deepcopy(first)
    declaration["bytes"] = len(encoded)
    declaration["sha256"] = sha256(encoded)
    expect_failure(
        lambda: inspect_archive(
            tampered,
            declaration,
            manifest["limits"],
            None,
        ),
        "nonzero archive trailer",
    )
    typeflag = bytearray(source.read_bytes())
    with tarfile.open(fileobj=io.BytesIO(typeflag), mode="r:") as archive:
        regular = next(member for member in archive.getmembers() if member.isfile())
    header = bytearray(typeflag[regular.offset : regular.offset + 512])
    header[156:157] = b"7"
    header[148:156] = b"        "
    header[148:156] = f"{sum(header):06o}\0 ".encode("ascii")
    typeflag[regular.offset : regular.offset + 512] = header
    typeflag_path = temporary / "noncanonical-typeflag.tar"
    typeflag_path.write_bytes(typeflag)
    typeflag_declaration = copy.deepcopy(first)
    typeflag_declaration["sha256"] = sha256(typeflag)
    expect_failure(
        lambda: inspect_archive(
            typeflag_path,
            typeflag_declaration,
            manifest["limits"],
            None,
        ),
        "noncanonical archive typeflag",
    )
    padded = bytearray(source.read_bytes())
    with tarfile.open(fileobj=io.BytesIO(padded), mode="r:") as archive:
        padded_member = next(
            member
            for member in archive.getmembers()
            if member.isfile() and member.size % 512 != 0
        )
    padded[padded_member.offset_data + padded_member.size] = 1
    padded_path = temporary / "nonzero-member-padding.tar"
    padded_path.write_bytes(padded)
    padded_declaration = copy.deepcopy(first)
    padded_declaration["sha256"] = sha256(padded)
    expect_failure(
        lambda: inspect_archive(
            padded_path,
            padded_declaration,
            manifest["limits"],
            None,
        ),
        "nonzero archive member padding",
    )
    extra_authority = temporary / "extra-authority"
    shutil.copytree(manifest_path.parent, extra_authority)
    (extra_authority / "unexpected").write_bytes(b"unexpected\n")
    expect_failure(
        lambda: audit_authority_directory(extra_authority / "manifest.json", manifest),
        "authority directory closure",
    )
    expect_failure(
        lambda: audit_schema({"unsupported": True}, "negative schema"),
        "unsupported JSON Schema keyword",
    )
    expect_failure(
        lambda: schema_validate(True, {"type": "integer"}, {}, {}),
        "JSON Schema boolean/integer distinction",
    )
    schema_validate(1.0, {"type": "integer"}, {}, {})
    expect_failure(
        lambda: schema_validate(None, {"oneOf": [{}, {}]}, {}, {}),
        "JSON Schema oneOf exact match",
    )
    expect_failure(
        lambda: schema_validate(None, {"not": {}}, {}, {}),
        "JSON Schema not",
    )
    expect_failure(
        lambda: schema_validate([1, 1.0], {"uniqueItems": True}, {}, {}),
        "JSON Schema numeric equality",
    )


def prepare_extraction_root(requested: Path) -> Path:
    root = Path(os.path.abspath(requested))
    require(root.name not in {"", ".", ".."}, "extraction root must have a final path component")
    require(not os.path.lexists(root), "extraction root must be absent")
    parent = root.parent
    parent_facts = parent.lstat()
    require(
        stat.S_ISDIR(parent_facts.st_mode)
        and not parent.is_symlink()
        and parent_facts.st_uid == os.getuid(),
        "extraction parent must be a pre-existing, caller-owned, non-symlink directory",
    )
    require(parent.resolve(strict=True) == parent, "extraction parent path must not traverse symlinks")
    root.mkdir(mode=0o700)
    return root


def validate(
    manifest_path: Path,
    *,
    extract_root: Path | None,
    run_self_test: bool,
) -> dict[str, Any]:
    manifest_path = Path(os.path.abspath(manifest_path))
    manifest_facts = manifest_path.lstat()
    require(stat.S_ISREG(manifest_facts.st_mode) and not manifest_path.is_symlink(), "manifest must be a regular file")
    manifest_path = manifest_path.resolve(strict=True)
    manifest = load_json(manifest_path)
    require(isinstance(manifest, dict), "manifest must be one JSON object")
    validate_schemas(manifest_path)
    if extract_root is not None:
        extract_root = prepare_extraction_root(extract_root)
    try:
        with tempfile.TemporaryDirectory(prefix="mirante4d-target-validate-") as directory:
            temporary = Path(directory)
            result = semantic_validate(manifest_path, manifest, extract_root)
            if run_self_test:
                self_test(manifest_path, manifest, temporary)
    except BaseException:
        if extract_root is not None:
            shutil.rmtree(extract_root, ignore_errors=True)
        raise
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, default=ROOT)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--extract-root", type=Path)
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    repository = arguments.repository.resolve(strict=True)
    require(repository == ROOT, "validator repository root is fixed")
    manifest = arguments.manifest
    if not manifest.is_absolute():
        manifest = ROOT / manifest
    result = validate(
        manifest,
        extract_root=arguments.extract_root,
        run_self_test=arguments.self_test,
    )
    print(json.dumps({"result": "passed", **result}, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        ValidationError,
        KeyError,
        TypeError,
        ValueError,
        tarfile.TarError,
        subprocess.TimeoutExpired,
    ) as error:
        print(f"target T1 validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
