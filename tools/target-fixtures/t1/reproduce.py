#!/usr/bin/env python3
"""Reproduce the ignored WP-10A-C target authority twice.

This is an off-product orchestrator.  It invokes the three independent
lineages as subprocesses, derives each frozen negative case in a fresh package
directory, and never writes the tracked ``fixtures/target`` authority.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import unicodedata
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
TOOL_ROOT = ROOT / "tools/target-fixtures/t1"
CANDIDATE_ROOT = ROOT / "target/mirante4d/fixture-candidates"
DEFAULT_OUTPUT = CANDIDATE_ROOT / "target-m4d-v1/c4"
SPEC = TOOL_ROOT / "cases-v1.tsv"
ORACLE_SOURCE = TOOL_ROOT / "fact_oracle/main.rs"
PRODUCER = TOOL_ROOT / "producer/produce.py"
READER = TOOL_ROOT / "reader/reader.py"
VALIDATOR = TOOL_ROOT / "validate.py"
VECTORS = TOOL_ROOT / "hand_vectors/hand-vectors-v1.json"
VECTOR_CHECK = TOOL_ROOT / "hand_vectors/verify_hand_vectors.py"
MUTATIONS = TOOL_ROOT / "mutations-v1.json"
ZSTD = Path("/usr/bin/zstd")
PYTHON = Path("/usr/bin/python3.12")
READER_LOCK = TOOL_ROOT / "reader/requirements-linux-x86_64-py312.lock"
PRODUCER_LOCK = TOOL_ROOT / "producer/toolchain-lock.json"
ORACLE_LOCK = TOOL_ROOT / "fact_oracle/toolchain-lock.json"
OME_IMAGE_SCHEMA = ROOT / "verification/standards/ome-ngff-0.5.2/0.5/schemas/image.schema"
OME_VERSION_SCHEMA = OME_IMAGE_SCHEMA.with_name("_version.schema")
MANIFEST_SCHEMA = ROOT / "docs/plans/active/foundation-refactor/schemas/foundation-target-fixture-manifest-v1.schema.json"
SCHEMA_VALIDATOR_ID = "Mirante4D offline JSON Schema subset v1"
EXPECTED_PYTHON_SHA256 = "1643dacd9feaedc58f3cc581e4d22577dfe25c09b10282936186ccf0f2e61118"
EXPECTED_UV_SHA256 = "13b335cfb84d5ec0a649ce071d6eb7c1e81496412caf9646f75434049da9d85c"
EXPECTED_ZSTD_SHA256 = "7c5468b370f7c47eda07281e3437fafc568f95d10420051e3aa522709f9342c5"

CASE_IDS = [
    "m4d-t1-u8-2d-sparse",
    "m4d-t1-u16-3d-multiscale",
    "m4d-t1-f32-3d-validity",
]
MISSING = (1 << 64) - 1
PAGE_BYTES_MAX = 1_048_576
ARCHIVE_BYTES_MAX = 16 * 1024 * 1024
FILE_BYTES_MAX = 32 * 1024 * 1024
REGULAR_BYTES_MAX = 64 * 1024 * 1024
FILES_MAX = 64
DIRECTORIES_MAX = 64
DEPTH_MAX = 8
FAN_OUT_MAX = 64
PATH_BYTES_MAX = 240


class ReproductionError(RuntimeError):
    """A deterministic reproduction or mutation failure."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ReproductionError(message)


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
    path.write_bytes(canonical_json(value) + b"\n")


def load_json(path: Path) -> Any:
    def pairs(rows: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in rows:
            require(key not in result, f"duplicate JSON key {key!r} in {path}")
            result[key] = value
        return result

    return json.loads(path.read_bytes(), object_pairs_hook=pairs)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def run(
    command: list[str],
    *,
    input_bytes: bytes | None = None,
    extra_environment: dict[str, str] | None = None,
) -> bytes:
    environment = os.environ.copy()
    environment.update({"LC_ALL": "C", "PYTHONDONTWRITEBYTECODE": "1"})
    if extra_environment is not None:
        environment.update(extra_environment)
    result = subprocess.run(
        command,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        check=False,
        timeout=300,
    )
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", "replace").strip()
        raise ReproductionError(f"command failed ({result.returncode}): {' '.join(command)}\n{stderr}")
    return result.stdout


def checked_package_path(value: str) -> PurePosixPath:
    require(value.isascii(), "package path must be ASCII")
    path = PurePosixPath(value)
    require(
        value
        and not path.is_absolute()
        and "\\" not in value
        and "." not in path.parts
        and ".." not in path.parts
        and len(value.encode("ascii")) <= PATH_BYTES_MAX,
        f"unsafe package path {value!r}",
    )
    return path


def package_file(root: Path, value: str) -> Path:
    path = checked_package_path(value)
    return root.joinpath(*path.parts)


def safe_extract(archive_path: Path, destination: Path) -> None:
    require(not destination.exists(), f"extraction destination already exists: {destination}")
    require(0 < archive_path.stat().st_size <= ARCHIVE_BYTES_MAX, "archive byte limit exceeded")
    with tarfile.open(archive_path, mode="r:") as archive:
        members = archive.getmembers()
        files: list[tarfile.TarInfo] = []
        directories: list[tarfile.TarInfo] = []
        names: set[str] = set()
        folded: set[str] = set()
        regular_bytes = 0
        for member in members:
            raw = member.name[:-1] if member.isdir() and member.name.endswith("/") else member.name
            path = checked_package_path(raw)
            require(member.isfile() or member.isdir(), f"unsafe archive member type: {raw}")
            require(not member.linkname and not member.pax_headers, f"extended/link member rejected: {raw}")
            normalized = unicodedata.normalize("NFC", raw)
            require(normalized == raw, f"non-NFC archive path: {raw}")
            require(raw not in names, f"duplicate archive path: {raw}")
            key = raw.casefold()
            require(key not in folded, f"case-fold archive collision: {raw}")
            names.add(raw)
            folded.add(key)
            require(len(path.parts) <= DEPTH_MAX + 1, f"archive depth exceeded: {raw}")
            if member.isfile():
                require(0 <= member.size <= FILE_BYTES_MAX, f"archive member too large: {raw}")
                regular_bytes += member.size
                files.append(member)
            else:
                require(member.size == 0, f"directory carries bytes: {raw}")
                directories.append(member)
        require(len(files) <= FILES_MAX, "archive file-count limit exceeded")
        require(len(directories) <= DIRECTORIES_MAX, "archive directory-count limit exceeded")
        require(regular_bytes <= REGULAR_BYTES_MAX, "archive regular-byte limit exceeded")
        file_names = {member.name for member in files}
        children: dict[str, set[str]] = {}
        for raw in names:
            path = PurePosixPath(raw)
            for parent in path.parents:
                if parent == PurePosixPath("."):
                    break
                require(parent.as_posix() not in file_names, f"file used as directory: {parent}")
            parent = path.parent.as_posix()
            children.setdefault("" if parent == "." else parent, set()).add(path.name)
        require(max((len(rows) for rows in children.values()), default=0) <= FAN_OUT_MAX, "fan-out limit exceeded")

        destination.mkdir(parents=True)
        for member in sorted(directories, key=lambda row: (len(PurePosixPath(row.name).parts), row.name)):
            package_file(destination, member.name.rstrip("/")).mkdir(parents=True, exist_ok=True)
        for member in sorted(files, key=lambda row: row.name):
            target = package_file(destination, member.name)
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            require(source is not None, f"cannot read archive member: {member.name}")
            remaining = member.size
            with target.open("xb") as output:
                while remaining:
                    block = source.read(min(65_536, remaining))
                    require(block != b"", f"short archive member: {member.name}")
                    output.write(block)
                    remaining -= len(block)
                require(source.read(1) == b"", f"long archive member: {member.name}")


def crc32c_table() -> tuple[int, ...]:
    rows = []
    for byte in range(256):
        value = byte
        for _ in range(8):
            value = (value >> 1) ^ (0x82F63B78 if value & 1 else 0)
        rows.append(value)
    return tuple(rows)


CRC32C_TABLE = crc32c_table()


def crc32c(data: bytes) -> int:
    value = 0xFFFFFFFF
    for byte in data:
        value = CRC32C_TABLE[(value ^ byte) & 0xFF] ^ (value >> 8)
    return value ^ 0xFFFFFFFF


def product(values: list[int]) -> int:
    result = 1
    for value in values:
        result *= value
    return result


def array_metadata_for_object(root: Path, object_path: str) -> tuple[dict[str, Any], str]:
    require("/c/" in object_path, f"shard path has no /c/: {object_path}")
    base = object_path.split("/c/", 1)[0]
    metadata = load_json(package_file(root, f"{base}/zarr.json"))
    require(isinstance(metadata, dict), "array metadata is not an object")
    return metadata, base


def shard_facts(root: Path, object_path: str) -> tuple[bytearray, int, int, list[list[int]]]:
    metadata, _ = array_metadata_for_object(root, object_path)
    outer = metadata["chunk_grid"]["configuration"]["chunk_shape"]
    inner = metadata["codecs"][0]["configuration"]["chunk_shape"]
    require(
        isinstance(outer, list)
        and isinstance(inner, list)
        and len(outer) == len(inner)
        and all(isinstance(value, int) and value > 0 for value in outer + inner),
        "invalid shard shapes",
    )
    require(all(left % right == 0 for left, right in zip(outer, inner)), "nondivisible shard shapes")
    slots = product([left // right for left, right in zip(outer, inner)])
    require(slots in {16, 64}, f"unexpected shard slot count {slots}")
    encoded = bytearray(package_file(root, object_path).read_bytes())
    tail = slots * 16 + 4
    require(len(encoded) >= tail, "shard is shorter than its end index")
    index_start = len(encoded) - tail
    index = bytes(encoded[index_start:-4])
    require(crc32c(index) == struct.unpack("<I", encoded[-4:])[0], "base shard index CRC32C failed")
    entries = [list(struct.unpack_from("<QQ", index, slot * 16)) for slot in range(slots)]
    for offset, length in entries:
        if offset == MISSING and length == MISSING:
            continue
        require(offset != MISSING and length != MISSING and offset + length <= index_start, "invalid base shard range")
    return encoded, slots, index_start, entries


def decode_inner(encoded: bytes) -> bytes:
    require(len(encoded) >= 4, "encoded inner payload is too short")
    compressed = encoded[:-4]
    expected = struct.unpack("<I", encoded[-4:])[0]
    require(crc32c(compressed) == expected, "base inner CRC32C failed")
    return run([str(ZSTD), "-d", "--quiet", "--stdout"], input_bytes=compressed)


def encode_inner(decoded: bytes) -> bytes:
    compressed = run(
        [
            str(ZSTD),
            "-3",
            f"--stream-size={len(decoded)}",
            "--no-check",
            "--quiet",
            "--stdout",
        ],
        input_bytes=decoded,
    )
    return compressed + struct.pack("<I", crc32c(compressed))


def rebuild_shard(entries: list[list[int]], old: bytes, index_start: int, replacements: dict[int, bytes]) -> bytes:
    slots: list[bytes | None] = []
    cursor = 0
    for slot, (offset, length) in enumerate(entries):
        if offset == MISSING:
            slots.append(None)
            continue
        require(offset == cursor, "base shard is not zero-slack canonical")
        payload = old[offset : offset + length]
        cursor += length
        slots.append(replacements.get(slot, payload))
    require(cursor == index_start, "base shard payload has trailing slack")
    payload = bytearray()
    index = bytearray()
    for encoded in slots:
        if encoded is None:
            index.extend(struct.pack("<QQ", MISSING, MISSING))
        else:
            index.extend(struct.pack("<QQ", len(payload), len(encoded)))
            payload.extend(encoded)
    return bytes(payload) + bytes(index) + struct.pack("<I", crc32c(index))


def json_pointer_replace(path: Path, pointer: str, original: Any, replacement: Any) -> None:
    require(pointer.startswith("/") and pointer != "/", f"invalid JSON pointer {pointer!r}")
    value = load_json(path)
    parts = [part.replace("~1", "/").replace("~0", "~") for part in pointer[1:].split("/")]
    parent = value
    for part in parts[:-1]:
        parent = parent[int(part)] if isinstance(parent, list) else parent[part]
    final = parts[-1]
    actual = parent[int(final)] if isinstance(parent, list) else parent[final]
    require(actual == original and type(actual) is type(original), f"JSON mutation precondition failed: {pointer}")
    if isinstance(parent, list):
        parent[int(final)] = replacement
    else:
        parent[final] = replacement
    path.write_bytes(canonical_json(value))


def manifest_state(root: Path) -> tuple[dict[str, dict[str, str]], set[str]]:
    root_path = root / "m4d/manifest/root.json"
    encoded_root = root_path.read_bytes()
    parsed_root = load_json(root_path)
    require(encoded_root == canonical_json(parsed_root), "manifest root is not canonical")
    descriptors: dict[str, dict[str, str]] = {}
    page_paths: set[str] = set()
    for reference in parsed_root["pages"]:
        page_path = reference["path"]
        page_paths.add(page_path)
        encoded = package_file(root, page_path).read_bytes()
        require(len(encoded) == int(reference["bytes"]), "manifest page byte count drifted")
        require(f"sha256:{sha256_bytes(encoded)}" == reference["digest"], "manifest page digest drifted")
        page = json.loads(encoded)
        for descriptor in page["entries"]:
            path = descriptor["path"]
            require(path not in descriptors, f"duplicate manifest descriptor {path}")
            descriptors[path] = descriptor
    require(list(descriptors) == sorted(descriptors), "manifest descriptors are not sorted")
    return descriptors, page_paths


def pack_pages(descriptors: list[dict[str, str]]) -> list[list[dict[str, str]]]:
    require(descriptors, "manifest cannot be empty")
    pages: list[list[dict[str, str]]] = []
    current: list[dict[str, str]] = []
    for descriptor in descriptors:
        candidate = current + [descriptor]
        encoded = canonical_json({"schema": "m4d-manifest-page", "schema_version": 1, "entries": candidate})
        if len(encoded) <= PAGE_BYTES_MAX:
            current = candidate
        else:
            require(current, "one manifest descriptor exceeds the page limit")
            pages.append(current)
            current = [descriptor]
    if current:
        pages.append(current)
    require(len(pages) <= 6, "manifest page-count limit exceeded")
    return pages


def reseal_manifest(root: Path) -> str:
    existing, old_pages = manifest_state(root)
    page_root = root / "m4d/manifest/pages"
    for path in old_pages:
        package_file(root, path).unlink()
    descriptors = []
    for path, descriptor in sorted(existing.items()):
        object_path = package_file(root, path)
        if not object_path.exists():
            continue
        require(object_path.is_file() and not object_path.is_symlink(), f"invalid package object {path}")
        encoded = object_path.read_bytes()
        updated = dict(descriptor)
        updated["bytes"] = str(len(encoded))
        updated["digest"] = f"sha256:{sha256_bytes(encoded)}"
        descriptors.append(updated)
    pages = pack_pages(descriptors)
    references = []
    for ordinal, rows in enumerate(pages):
        relative = f"m4d/manifest/pages/p{ordinal:08d}.json"
        encoded = canonical_json({"schema": "m4d-manifest-page", "schema_version": 1, "entries": rows})
        package_file(root, relative).write_bytes(encoded)
        references.append(
            {
                "path": relative,
                "first_path": rows[0]["path"],
                "last_path": rows[-1]["path"],
                "entry_count": str(len(rows)),
                "bytes": str(len(encoded)),
                "digest": f"sha256:{sha256_bytes(encoded)}",
            }
        )
    for stale in page_root.glob("p*.json"):
        require(stale.name in {f"p{ordinal:08d}.json" for ordinal in range(len(pages))}, "stale manifest page")
    encoded_root = canonical_json({"schema": "m4d-manifest-root", "schema_version": 1, "pages": references})
    (root / "m4d/manifest/root.json").write_bytes(encoded_root)
    return f"m4d-package-v1-sha256:{sha256_bytes(encoded_root)}"


def package_id(root: Path) -> str:
    encoded = (root / "m4d/manifest/root.json").read_bytes()
    return f"m4d-package-v1-sha256:{sha256_bytes(encoded)}"


def tree_rows(root: Path) -> list[dict[str, Any]]:
    rows = []
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink():
            relative = path.relative_to(root).as_posix()
            rows.append({"path": relative, "bytes": path.stat().st_size, "sha256": sha256_file(path)})
    return rows


def tree_digest(root: Path) -> str:
    return sha256_bytes(canonical_json(tree_rows(root)))


def replace_valid_sample(root: Path, recipe: dict[str, Any]) -> str:
    t, logical_c, z, y, x = recipe["logical_coordinate_tczyx"]
    profile = load_json(root / "m4d/profile.json")
    image = profile["images"][0]
    mapping = {int(row["logical_layer_ordinal"]): int(row["physical_channel"]) for row in image["logical_layers"]}
    require(logical_c in mapping, "logical channel is absent")
    physical_c = mapping[logical_c]
    level = image["levels"][0]
    pixel_base = level["pixel_path"]
    metadata = load_json(package_file(root, f"{pixel_base}/zarr.json"))
    require(metadata["data_type"] == "float32", "nonfinite recipe requires float32")
    shape = metadata["shape"]
    coordinate = [t, physical_c, z, y, x]
    require(all(isinstance(value, int) and 0 <= value < bound for value, bound in zip(coordinate, shape)), "sample coordinate is out of bounds")
    outer = metadata["chunk_grid"]["configuration"]["chunk_shape"]
    inner = metadata["codecs"][0]["configuration"]["chunk_shape"]
    ratios = [left // right for left, right in zip(outer, inner)]
    outer_coordinate = [value // width for value, width in zip(coordinate, outer)]
    inner_chunk = [(value % width) // small for value, width, small in zip(coordinate, outer, inner)]
    slot = 0
    for value, width in zip(inner_chunk, ratios):
        slot = slot * width + value
    object_path = f"{pixel_base}/c/" + "/".join(str(value) for value in outer_coordinate)
    shard, _slots, index_start, entries = shard_facts(root, object_path)
    offset, length = entries[slot]
    require(offset != MISSING, "selected pixel inner payload is missing")
    decoded = bytearray(decode_inner(bytes(shard[offset : offset + length])))
    local = [value % width for value, width in zip(coordinate, inner)]
    linear = 0
    for value, width in zip(local, inner):
        linear = linear * width + value
    byte_offset = linear * 4
    require(byte_offset + 4 <= len(decoded), "selected float sample is outside decoded payload")
    original = struct.unpack_from("<I", decoded, byte_offset)[0]
    require(f"{original:08x}" == recipe["original_bits"], "float mutation bit precondition failed")
    struct.pack_into("<I", decoded, byte_offset, int(recipe["replacement_bits"], 16))
    rebuilt = rebuild_shard(entries, bytes(shard), index_start, {slot: encode_inner(bytes(decoded))})
    package_file(root, object_path).write_bytes(rebuilt)
    return object_path


def apply_mutation(root: Path, recipe: dict[str, Any]) -> str:
    operation = recipe["operation"]
    if operation == "remove_object":
        path = package_file(root, recipe["object"])
        require(path.is_file() and not path.is_symlink(), "remove target is not a regular file")
        path.unlink()
        return recipe["object"]
    if operation == "truncate_object_tail":
        path = package_file(root, recipe["object"])
        encoded = path.read_bytes()
        count = recipe["remove_bytes"]
        require(isinstance(count, int) and 0 < count < len(encoded), "invalid truncation count")
        path.write_bytes(encoded[:-count])
        return recipe["object"]
    if operation == "xor_object_byte":
        path = package_file(root, recipe["object"])
        encoded = bytearray(path.read_bytes())
        offset = recipe["byte_offset"]
        require(isinstance(offset, int) and 0 <= offset < len(encoded), "invalid byte offset")
        encoded[offset] ^= recipe["xor_mask"]
        path.write_bytes(encoded)
        return recipe["object"]
    if operation in {"xor_inner_crc32c_byte", "xor_end_index_crc32c_byte", "copy_inner_offset"}:
        object_path = recipe["object"]
        encoded, slots, index_start, entries = shard_facts(root, object_path)
        if operation == "xor_inner_crc32c_byte":
            slot = recipe["inner_slot"]
            require(isinstance(slot, int) and 0 <= slot < slots, "invalid inner slot")
            offset, length = entries[slot]
            byte = recipe["crc_byte"]
            require(offset != MISSING and length >= 4 and byte in range(4), "invalid inner CRC selector")
            encoded[offset + length - 4 + byte] ^= recipe["xor_mask"]
        elif operation == "xor_end_index_crc32c_byte":
            byte = recipe["crc_byte"]
            require(byte in range(4), "invalid end-index CRC selector")
            encoded[len(encoded) - 4 + byte] ^= recipe["xor_mask"]
        else:
            source = recipe["source_slot"]
            target = recipe["target_slot"]
            require(source in range(slots) and target in range(slots) and source != target, "invalid index slots")
            source_offset = entries[source][0]
            require(source_offset != MISSING and entries[target][0] != MISSING, "copy offset requires present slots")
            struct.pack_into("<Q", encoded, index_start + target * 16, source_offset)
            require(recipe["recompute_end_index_crc32c"] is True, "index CRC recomputation must be explicit")
            index = bytes(encoded[index_start:-4])
            struct.pack_into("<I", encoded, len(encoded) - 4, crc32c(index))
        package_file(root, object_path).write_bytes(encoded)
        return object_path
    if operation == "replace_json_value":
        path = package_file(root, recipe["object"])
        json_pointer_replace(path, recipe["json_pointer"], recipe["original"], recipe["replacement"])
        return recipe["object"]
    if operation == "add_object":
        path = package_file(root, recipe["object"])
        require(not path.exists(), "added object already exists")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(bytes.fromhex(recipe["bytes_hex"]))
        return recipe["object"]
    if operation == "replace_valid_sample_bits":
        return replace_valid_sample(root, recipe)
    raise ReproductionError(f"unsupported frozen mutation operation {operation!r}")


def object_fact(root: Path, relative: str) -> dict[str, Any] | None:
    path = package_file(root, relative)
    if not path.exists():
        return None
    require(path.is_file() and not path.is_symlink(), f"mutated object is not regular: {relative}")
    return {"path": relative, "bytes": path.stat().st_size, "sha256": sha256_file(path)}


def read_report(path: Path) -> Any:
    require(path.is_file(), f"subprocess did not write report: {path}")
    report = load_json(path)
    require(isinstance(report, dict), f"report is not an object: {path}")
    return report


def call_reader(
    reader_python: Path,
    case_id: str,
    package: Path,
    report: Path,
    expected_rejection: str | None = None,
) -> dict[str, Any]:
    command = [
        str(reader_python),
        str(READER),
        "--case-id",
        case_id,
        "--package",
        str(package),
        "--report",
        str(report),
    ]
    if expected_rejection is not None:
        command.extend(["--expect-rejection", expected_rejection])
    run(command)
    return read_report(report)


def validate_recipes() -> list[dict[str, Any]]:
    document = load_json(MUTATIONS)
    require(
        document.get("schema") == "mirante4d-target-t1-mutation-recipes"
        and document.get("schema_version") == 1,
        "mutation recipe schema identity failed",
    )
    recipes = document.get("recipes")
    require(isinstance(recipes, list) and len(recipes) == 15, "exactly 15 mutation recipes are required")
    ids: set[str] = set()
    allowed_operations = {
        "remove_object",
        "truncate_object_tail",
        "xor_object_byte",
        "xor_inner_crc32c_byte",
        "xor_end_index_crc32c_byte",
        "copy_inner_offset",
        "replace_json_value",
        "add_object",
        "replace_valid_sample_bits",
    }
    for recipe in recipes:
        require(isinstance(recipe, dict), "mutation recipe must be an object")
        require(recipe.get("id") not in ids and isinstance(recipe.get("id"), str), "duplicate/invalid recipe id")
        ids.add(recipe["id"])
        require(recipe.get("case_id") in CASE_IDS, "mutation recipe has an unknown case")
        require(recipe.get("operation") in allowed_operations, "mutation recipe has an unknown operation")
        require(recipe.get("package_manifest") in {"preserve", "reseal"}, "invalid package_manifest mode")
        require(isinstance(recipe.get("expected_stage"), str) and isinstance(recipe.get("expected_rejection"), str), "mutation expectation is incomplete")
    return recipes


def mutation_report(
    run_root: Path,
    reader_python: Path,
    archives: dict[str, Path],
    recipes: list[dict[str, Any]],
) -> dict[str, Any]:
    work = run_root / "work/mutations"
    work.mkdir(parents=True)
    reports = run_root / "work/mutation-reader-reports"
    reports.mkdir(parents=True)
    results = []
    for recipe in recipes:
        package = work / recipe["id"]
        safe_extract(archives[recipe["case_id"]], package)
        before_package_id = package_id(package)
        declared_object = recipe.get("object")
        before = object_fact(package, declared_object) if isinstance(declared_object, str) else None
        changed_path = apply_mutation(package, recipe)
        after_mutation = object_fact(package, changed_path)
        after_package_id = (
            reseal_manifest(package)
            if recipe["package_manifest"] == "reseal"
            else package_id(package)
        )
        report_path = reports / f"{recipe['id']}.json"
        observed = call_reader(
            reader_python,
            recipe["case_id"],
            package,
            report_path,
            recipe["expected_rejection"],
        )
        results.append(
            {
                "id": recipe["id"],
                "case_id": recipe["case_id"],
                "operation": recipe["operation"],
                "package_manifest": recipe["package_manifest"],
                "expected_stage": recipe["expected_stage"],
                "expected_rejection": recipe["expected_rejection"],
                "base_archive_sha256": sha256_file(archives[recipe["case_id"]]),
                "package_id_before": before_package_id,
                "package_id_after": after_package_id,
                "object_before": before,
                "object_after": after_mutation,
                "derived_tree_sha256": tree_digest(package),
                "reader_result": observed,
            }
        )
        shutil.rmtree(package)
    return {
        "schema": "mirante4d-target-t1-bound-mutations",
        "schema_version": 1,
        "status": "passed",
        "source_recipe_sha256": sha256_file(MUTATIONS),
        "recipes": results,
    }


def file_fact(path: Path, relative: str) -> dict[str, Any]:
    return {"path": relative, "bytes": path.stat().st_size, "sha256": sha256_file(path)}


def archive_inventory(path: Path) -> dict[str, Any]:
    directories: list[str] = []
    files: dict[str, dict[str, Any]] = {}
    child_counts: dict[str, int] = {}
    with tarfile.open(path, mode="r:") as archive:
        for member in archive.getmembers():
            relative = member.name.rstrip("/")
            checked_package_path(relative)
            parent = PurePosixPath(relative).parent.as_posix()
            child_counts["" if parent == "." else parent] = child_counts.get("" if parent == "." else parent, 0) + 1
            if member.isdir():
                directories.append(relative)
                continue
            require(member.isfile(), f"non-regular archive member {relative}")
            source = archive.extractfile(member)
            require(source is not None, f"cannot read archive member {relative}")
            encoded = source.read()
            require(len(encoded) == member.size, f"short archive member {relative}")
            files[relative] = {"bytes": member.size, "sha256": sha256_bytes(encoded)}
    directories.sort()
    files = dict(sorted(files.items()))
    return {
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


def logical_voxel_bytes(facts_case: dict[str, Any]) -> int:
    sample_bytes = {"uint8": 1, "uint16": 2, "float32": 4}[facts_case["dtype"]]
    return sum(product(level["shape_tczyx"]) * sample_bytes for level in facts_case["levels"])


def authority_file(path: str, sha256: str, schema: str) -> dict[str, str]:
    return {"path": path, "sha256": sha256, "schema": schema}


def lineage_facts() -> dict[str, dict[str, str]]:
    rows = {
        "producer": {
            "id": "TGT-PRODUCER-001",
            "class": "byte_producer",
            "source_path": "tools/target-fixtures/t1/producer/produce.py",
            "lock_path": "tools/target-fixtures/t1/producer/toolchain-lock.json",
            "source_sha256": sha256_file(PRODUCER),
            "lock_sha256": sha256_file(PRODUCER_LOCK),
        },
        "fact_oracle": {
            "id": "TGT-FACT-001",
            "class": "fact_oracle",
            "source_path": "tools/target-fixtures/t1/fact_oracle/main.rs",
            "lock_path": "tools/target-fixtures/t1/fact_oracle/toolchain-lock.json",
            "source_sha256": sha256_file(ORACLE_SOURCE),
            "lock_sha256": sha256_file(ORACLE_LOCK),
        },
        "independent_reader": {
            "id": "TGT-READER-001",
            "class": "independent_reader",
            "source_path": "tools/target-fixtures/t1/reader/reader.py",
            "lock_path": "tools/target-fixtures/t1/reader/requirements-linux-x86_64-py312.lock",
            "source_sha256": sha256_file(READER),
            "lock_sha256": sha256_file(READER_LOCK),
        },
    }
    return rows


def authority_binding(archives: list[dict[str, Any]], authority: dict[str, str]) -> tuple[dict[str, Any], str]:
    lineages = lineage_facts()
    preimage = {
        "archives": [{"case_id": row["case_id"], "sha256": row["sha256"]} for row in archives],
        "authority_files": authority,
        "lineages": {
            name: {
                "source_sha256": lineages[name]["source_sha256"],
                "lock_sha256": lineages[name]["lock_sha256"],
            }
            for name in ["producer", "fact_oracle", "independent_reader"]
        },
    }
    return preimage, sha256_bytes(canonical_json(preimage))


def generated_rows(authority: Path) -> list[dict[str, Any]]:
    paths = [
        *(f"archives/{case_id}.tar" for case_id in CASE_IDS),
        "expected-facts.json",
        "identity-vectors.json",
        "mutations.json",
        "independent-reader-report.json",
    ]
    return [file_fact(authority / relative, relative) for relative in sorted(paths)]


def setup_reader_environment(work_root: Path) -> Path:
    require(sha256_file(PYTHON) == EXPECTED_PYTHON_SHA256, "pinned CPython digest drifted")
    require(sha256_file(ZSTD) == EXPECTED_ZSTD_SHA256, "pinned zstd digest drifted")
    uv_name = shutil.which("uv")
    require(uv_name is not None, "uv is required for the pinned reader environment")
    uv = Path(uv_name).resolve(strict=True)
    require(sha256_file(uv) == EXPECTED_UV_SHA256, "pinned uv digest drifted")
    venv = work_root / "reader-venv"
    environment = {"UV_NO_CACHE": "1"}
    run(
        [str(uv), "venv", "--clear", "--python", str(PYTHON), str(venv)],
        extra_environment=environment,
    )
    reader_python = venv / "bin/python"
    run(
        [
            str(uv),
            "pip",
            "install",
            "--python",
            str(reader_python),
            "--require-hashes",
            "-r",
            str(READER_LOCK),
        ],
        extra_environment=environment,
    )
    return reader_python


def locked_rustc() -> Path:
    lock = load_json(ORACLE_LOCK)
    require(
        lock.get("schema") == "mirante4d-target-t1-toolchain-lock"
        and lock.get("lineage_id") == "TGT-FACT-001",
        "fact-oracle toolchain lock identity failed",
    )
    tools = lock.get("tools")
    require(isinstance(tools, list) and len(tools) == 1 and tools[0].get("name") == "Rust", "fact-oracle Rust lock is invalid")
    tool = tools[0]
    version = tool["version"]
    require(tool.get("command") == f"rustc +{version}", "fact-oracle Rust command drifted")
    proxy_name = shutil.which("rustc")
    require(proxy_name is not None, "rustc is unavailable")
    sysroot = Path(
        run([proxy_name, f"+{version}", "--print", "sysroot"])
        .decode("utf-8")
        .strip()
    )
    binary = sysroot / "bin/rustc"
    require(binary.is_file() and sha256_file(binary) == tool["installed_sha256"], "fact-oracle rustc digest drifted")
    observed = run([str(binary), "--version"]).decode("utf-8").strip()
    require(
        observed.startswith(f"rustc {version} ({tool['source_commit'][:9]} ")
        and observed.endswith(")"),
        "fact-oracle rustc version drifted",
    )
    return binary


def build_run(
    run_root: Path,
    oracle: Path,
    reader_python: Path,
    recipes: list[dict[str, Any]],
) -> tuple[str, str]:
    authority = run_root / "authority"
    authority.mkdir(parents=True)
    facts = authority / "expected-facts.json"
    run([str(oracle), "--spec", str(SPEC), "--output", str(facts)])

    producer_output = run_root / "work/producer"
    run(
        [
            str(PYTHON),
            str(PRODUCER),
            "--spec",
            str(SPEC),
            "--facts",
            str(facts),
            "--output",
            str(producer_output),
        ]
    )
    archive_root = authority / "archives"
    archive_root.mkdir()
    archives: dict[str, Path] = {}
    archive_rows = []
    for case_id in CASE_IDS:
        source = producer_output / f"archives/{case_id}.tar"
        target = archive_root / f"{case_id}.tar"
        shutil.copyfile(source, target)
        archives[case_id] = target
        archive_rows.append({"case_id": case_id, "path": f"archives/{case_id}.tar", "bytes": target.stat().st_size, "sha256": sha256_file(target)})
    shutil.copyfile(VECTORS, authority / "identity-vectors.json")
    shutil.copyfile(MUTATIONS, authority / "mutations.json")

    positive_reports = []
    positive_root = run_root / "work/positive-packages"
    positive_report_root = run_root / "work/positive-reader-reports"
    positive_root.mkdir(parents=True)
    positive_report_root.mkdir(parents=True)
    for case_id in CASE_IDS:
        package = positive_root / case_id
        safe_extract(archives[case_id], package)
        observed = call_reader(reader_python, case_id, package, positive_report_root / f"{case_id}.json")
        positive_reports.append(observed)
        shutil.rmtree(package)

    mutations = mutation_report(run_root, reader_python, archives, recipes)
    producer_report = load_json(producer_output / "producer-report.json")
    authority_files = {
        "expected_facts": sha256_file(facts),
        "identity_vectors": sha256_file(authority / "identity-vectors.json"),
        "mutations": sha256_file(authority / "mutations.json"),
    }
    binding_preimage, binding = authority_binding(archive_rows, authority_files)
    reader_report = {
        "schema": "mirante4d-target-t1-independent-reader-report",
        "schema_version": 1,
        "status": "passed",
        "authority_binding_sha256": binding,
        "cases": positive_reports,
        "mutations": mutations["recipes"],
        "official_schema": {
            "validator": SCHEMA_VALIDATOR_ID,
            "image_schema_sha256": sha256_file(OME_IMAGE_SCHEMA),
            "version_schema_sha256": sha256_file(OME_VERSION_SCHEMA),
            "cases": [
                {"case_id": case_id, "status": "passed"}
                for case_id in CASE_IDS
            ],
        },
    }
    write_json(authority / "independent-reader-report.json", reader_report)
    rows = generated_rows(authority)
    generated = sha256_bytes(canonical_json(rows))
    run_report = {
        "schema": "mirante4d-target-t1-reproduction-report",
        "schema_version": 1,
        "status": "passed",
        "authority_binding_sha256": binding,
        "authority_binding_preimage": binding_preimage,
        "generated_tree_sha256": generated,
        "generated_files": rows,
        "reproducer": {
            "source_path": "tools/target-fixtures/t1/reproduce.py",
            "source_sha256": sha256_file(Path(__file__)),
        },
        "producer": {
            "source_sha256": sha256_file(PRODUCER),
            "lock_sha256": sha256_file(PRODUCER_LOCK),
            "installed_tools": {
                row["name"]: row["installed_sha256"]
                for row in load_json(PRODUCER_LOCK)["tools"]
            },
        },
        "outputs": [
            {
                "case_id": row["case_id"],
                "archive_sha256": row["sha256"],
                "package_id": next(
                    case["package_id"]
                    for case in producer_report["cases"]
                    if case["case_id"] == row["case_id"]
                ),
            }
            for row in archive_rows
        ],
    }
    write_json(run_root / "reproduction-report.json", run_report)
    write_json(authority / "reproduction-report.json", run_report)
    return binding, generated


def compare_authorities(left: Path, right: Path) -> None:
    left_rows = tree_rows(left)
    right_rows = tree_rows(right)
    require(left_rows == right_rows, "two authority assemblies differ")
    for row in left_rows:
        relative = row["path"]
        require((left / relative).read_bytes() == (right / relative).read_bytes(), f"authority bytes differ: {relative}")


def candidate_manifest(
    authority: Path,
    producer_report: dict[str, Any],
    binding: str,
    generated: str,
) -> dict[str, Any]:
    facts = load_json(authority / "expected-facts.json")
    facts_by_case = {row["case_id"]: row for row in facts["cases"]}
    producer_by_case = {row["case_id"]: row for row in producer_report["cases"]}
    archives = []
    for case_id in CASE_IDS:
        path = authority / f"archives/{case_id}.tar"
        produced = producer_by_case[case_id]
        archives.append(
            {
                "case_id": case_id,
                "path": f"fixtures/target/archives/{case_id}.tar",
                "format": "ustar",
                "compression": "none",
                "sha256": sha256_file(path),
                "bytes": path.stat().st_size,
                "logical_voxel_bytes": logical_voxel_bytes(facts_by_case[case_id]),
                "package_id": produced["package_id"],
                "tree_sha256": produced["tree_sha256"],
                "inventory": archive_inventory(path),
            }
        )
    lineages = lineage_facts()
    approval = {
        "state": "approved",
        "approved_by": "Mirante4D repository owner",
        "approved_on": "2026-07-12",
        "reference": "WP10A-C-PREAUTH-2026-07-12",
    }
    return {
        "$schema": "../../docs/plans/active/foundation-refactor/schemas/foundation-target-fixture-manifest-v1.schema.json",
        "schema": "mirante4d-foundation-target-fixture-manifest",
        "schema_version": 1,
        "status": "independently_validated",
        "fixture_id": "target-m4d-v1",
        "publication_class": "public_safe",
        "license": "MIT",
        "profile": {
            "storage_profile": "m4d-zarr3-local-1.0",
            "lifecycle": "EXPERIMENTAL",
            "cases_path": "tools/target-fixtures/t1/cases-v1.tsv",
            "cases_sha256": sha256_file(SPEC),
            "normative_standards_path": "architecture/wp10a-normative-standards.json",
            "normative_standards_sha256": sha256_file(ROOT / "architecture/wp10a-normative-standards.json"),
        },
        "lineages": lineages,
        "archives": archives,
        "authority_files": {
            "expected_facts": authority_file(
                "fixtures/target/expected-facts.json",
                sha256_file(authority / "expected-facts.json"),
                "mirante4d-target-t1-independent-facts",
            ),
            "identity_vectors": authority_file(
                "fixtures/target/identity-vectors.json",
                sha256_file(authority / "identity-vectors.json"),
                "mirante4d-wp10a-c-hand-vectors",
            ),
            "mutations": authority_file(
                "fixtures/target/mutations.json",
                sha256_file(authority / "mutations.json"),
                "mirante4d-target-t1-mutation-recipes",
            ),
            "independent_reader_report": authority_file(
                "fixtures/target/independent-reader-report.json",
                sha256_file(authority / "independent-reader-report.json"),
                "mirante4d-target-t1-independent-reader-report",
            ),
            "reproduction_report": authority_file(
                "fixtures/target/reproduction-report.json",
                sha256_file(authority / "reproduction-report.json"),
                "mirante4d-target-t1-reproduction-report",
            ),
        },
        "limits": {
            "combined_archive_bytes": 33_554_432,
            "archive_bytes_each": ARCHIVE_BYTES_MAX,
            "combined_unpacked_regular_file_bytes": 67_108_864,
            "combined_logical_voxel_bytes": 67_108_864,
            "files_each": FILES_MAX,
            "directories_each": DIRECTORIES_MAX,
            "depth": DEPTH_MAX,
            "fan_out": FAN_OUT_MAX,
            "path_bytes": PATH_BYTES_MAX,
            "individual_file_bytes": FILE_BYTES_MAX,
            "compression_ratio": 16,
        },
        "reproduction": {
            "command": "python3 tools/target-fixtures/t1/reproduce.py",
            "clean_runs": 2,
            "generated_tree_sha256": generated,
            "authority_binding_sha256": binding,
        },
        "validator": {
            "path": "tools/target-fixtures/t1/validate.py",
            "sha256": sha256_file(VALIDATOR),
            "command": "python3 tools/target-fixtures/t1/validate.py --manifest fixtures/target/manifest.json --self-test",
            "manifest_schema_path": "docs/plans/active/foundation-refactor/schemas/foundation-target-fixture-manifest-v1.schema.json",
            "manifest_schema_sha256": sha256_file(MANIFEST_SCHEMA),
            "identity_vector_verifier_path": "tools/target-fixtures/t1/hand_vectors/verify_hand_vectors.py",
            "identity_vector_verifier_sha256": sha256_file(VECTOR_CHECK),
        },
        "approvals": {
            "repository_owner": approval | {"role": "repository_owner"},
            "scientific_content": approval | {"role": "scientific_content"},
        },
    }


def reproduce(output: Path) -> dict[str, Any]:
    CANDIDATE_ROOT.mkdir(parents=True, exist_ok=True)
    candidate_root = CANDIDATE_ROOT.resolve(strict=True)
    output = output.resolve(strict=False)
    require(output != candidate_root and candidate_root in output.parents, "output must stay below the candidate root")
    require("fixtures/target" not in output.as_posix(), "tracked target authority is forbidden")
    for required in [
        SPEC,
        ORACLE_SOURCE,
        PRODUCER,
        PRODUCER_LOCK,
        ORACLE_LOCK,
        READER,
        READER_LOCK,
        VECTORS,
        VECTOR_CHECK,
        MUTATIONS,
        OME_IMAGE_SCHEMA,
        OME_VERSION_SCHEMA,
        MANIFEST_SCHEMA,
        ZSTD,
        PYTHON,
    ]:
        require(required.is_file(), f"required C4 input is missing: {required.relative_to(ROOT) if ROOT in required.parents else required}")
    require(stat.S_ISREG(ZSTD.lstat().st_mode), "zstd is not a regular file")
    recipes = validate_recipes()
    if output.exists():
        shutil.rmtree(output)
    output.mkdir(parents=True)
    try:
        oracle = output / "work/fact-oracle"
        oracle.parent.mkdir()
        rustc = locked_rustc()
        run([str(rustc), "--edition=2021", "-D", "warnings", str(ORACLE_SOURCE), "-o", str(oracle)])
        run([str(oracle), "--self-test"])
        run([str(PYTHON), str(VECTOR_CHECK), "--vectors", str(VECTORS)])
        reader_python = setup_reader_environment(output / "work")
        bindings = []
        trees = []
        for name in ["run-a", "run-b"]:
            run_root = output / name
            run_root.mkdir()
            binding, generated = build_run(run_root, oracle, reader_python, recipes)
            bindings.append(binding)
            trees.append(generated)
        require(bindings[0] == bindings[1], "authority binding differs across reproductions")
        require(trees[0] == trees[1], "generated authority tree differs across reproductions")
        require(VALIDATOR.is_file(), "required C4 validator is missing")
        for name in ["run-a", "run-b"]:
            run_authority = output / name / "authority"
            producer_report = load_json(output / name / "work/producer/producer-report.json")
            write_json(
                run_authority / "manifest.json",
                candidate_manifest(run_authority, producer_report, bindings[0], trees[0]),
            )
        compare_authorities(output / "run-a/authority", output / "run-b/authority")
        canonical = output / "authority"
        shutil.copytree(output / "run-a/authority", canonical)
        run([str(PYTHON), str(VALIDATOR), "--manifest", str(canonical / "manifest.json"), "--self-test"])
        report = {
            "schema": "mirante4d-target-t1-reproduction-report",
            "schema_version": 1,
            "status": "passed",
            "authority": False,
            "authority_binding_sha256": bindings[0],
            "generated_tree_sha256": trees[0],
            "runs": ["run-a", "run-b"],
            "candidate_authority": "authority",
        }
        write_json(output / "reproduction-report.json", report)
        shutil.rmtree(output / "work", ignore_errors=True)
        for name in ["run-a", "run-b"]:
            shutil.rmtree(output / name / "work", ignore_errors=True)
        return report
    except BaseException:
        shutil.rmtree(output, ignore_errors=True)
        raise


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    arguments = parser.parse_args()
    report = reproduce(arguments.output)
    print(canonical_json(report).decode("utf-8"))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError, tarfile.TarError, subprocess.TimeoutExpired, ReproductionError) as error:
        print(f"target T1 reproduction failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
