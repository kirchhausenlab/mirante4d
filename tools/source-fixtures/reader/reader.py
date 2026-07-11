#!/usr/bin/env python3
"""SRC-READER-001: independent Pillow/lxml observation and stop gate."""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import math
import struct
import tempfile
from pathlib import Path
from typing import Any

from PIL import Image, __version__ as pillow_version
from lxml import etree


EXPECTED_HEADER = [
    "kind",
    "spec_id",
    "path",
    "dtype",
    "pages",
    "height",
    "width",
    "t",
    "c",
    "z_start",
    "value_rule",
    "rows_per_strip",
    "expected_class",
    "shape_tczyx",
    "calibration_xyz_um",
    "grouping_id",
]
F32_CAPABILITY_BITS = [
    0x00000000,
    0x80000000,
    0x3F800000,
    0x7FC00000,
    0x7F800000,
    0xFF800000,
]
FORBIDDEN_METADATA_TAGS = [269, 305, 306, 315, 316]


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def parse_spec(path: Path) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    with path.open(newline="", encoding="utf-8") as source:
        reader = csv.DictReader(source, delimiter="|")
        if reader.fieldnames != EXPECTED_HEADER:
            raise ValueError(f"unexpected specification header: {reader.fieldnames!r}")
        rows = list(reader)
    families = [row for row in rows if row["kind"] == "family"]
    files = [row for row in rows if row["kind"] == "file"]
    if len(families) != 4 or len(files) != 16:
        raise ValueError("v1 specification must contain four families and sixteen files")
    if len({row["path"] for row in files}) != len(files):
        raise ValueError("v1 specification contains duplicate paths")
    return families, sorted(files, key=lambda row: row["path"])


def short_value(value: int) -> bytes:
    return struct.pack("<H", value) + b"\0\0"


def long_value(value: int) -> bytes:
    return struct.pack("<I", value)


def capability_tiff() -> tuple[bytes, bytes]:
    payload = b"".join(struct.pack("<I", bits) for bits in F32_CAPABILITY_BITS)
    tags = [
        (256, 4, 1, long_value(3)),
        (257, 4, 1, long_value(2)),
        (258, 3, 1, short_value(32)),
        (259, 3, 1, short_value(1)),
        (262, 3, 1, short_value(1)),
        (273, 4, 1, b"\0\0\0\0"),
        (277, 3, 1, short_value(1)),
        (278, 4, 1, long_value(2)),
        (279, 4, 1, long_value(len(payload))),
        (339, 3, 1, short_value(3)),
    ]
    data_offset = 8 + 2 + len(tags) * 12 + 4
    tags[5] = (273, 4, 1, long_value(data_offset))
    ifd = struct.pack("<H", len(tags))
    for tag, field_type, count, value in tags:
        ifd += struct.pack("<HHI", tag, field_type, count) + value
    ifd += struct.pack("<I", 0)
    return b"II" + struct.pack("<HI", 42, 8) + ifd + payload, payload


def normalize_scalar(value: Any) -> Any:
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="strict").rstrip("\0")
    if hasattr(value, "numerator") and hasattr(value, "denominator"):
        numerator = int(value.numerator)
        denominator = int(value.denominator)
        return numerator if denominator == 1 else [numerator, denominator]
    if isinstance(value, (list, tuple)):
        return [normalize_scalar(item) for item in value]
    if isinstance(value, (int, float, str)) or value is None:
        return value
    return str(value)


def require_tag(image: Image.Image, tag: int) -> Any:
    value = image.tag_v2.get(tag)
    if value is None:
        raise ValueError(f"required TIFF tag {tag} is absent")
    return normalize_scalar(value)


def run_capability_probe() -> dict[str, Any]:
    encoded, expected_payload = capability_tiff()
    with Image.open(io.BytesIO(encoded)) as image:
        image.load()
        if image.format != "TIFF" or image.mode != "F" or image.size != (3, 2):
            raise RuntimeError(
                f"Pillow F32 capability mismatch: {image.format=} {image.mode=} {image.size=}"
            )
        observed = image.tobytes()
        if observed != expected_payload:
            raise RuntimeError(
                "Pillow did not preserve exact F32 bits: "
                f"expected={expected_payload.hex()} observed={observed.hex()}"
            )
        required = {
            "bits_per_sample": require_tag(image, 258),
            "compression": require_tag(image, 259),
            "photometric": require_tag(image, 262),
            "strip_offsets": require_tag(image, 273),
            "rows_per_strip": require_tag(image, 278),
            "strip_byte_counts": require_tag(image, 279),
            "sample_format": require_tag(image, 339),
        }
    return {
        "schema": "mirante4d-source-fixture-reader-capability",
        "schema_version": 1,
        "status": "passed",
        "python": ".".join(map(str, __import__("sys").version_info[:3])),
        "pillow": pillow_version,
        "lxml": ".".join(map(str, etree.LXML_VERSION)),
        "f32_bits_sha256": sha256(expected_payload),
        "f32_bits_hex": [f"{bits:08x}" for bits in F32_CAPABILITY_BITS],
        "required_tags": required,
    }


def tag_int(value: Any) -> int:
    value = normalize_scalar(value)
    if isinstance(value, list):
        if len(value) != 1:
            raise ValueError(f"expected one TIFF tag value, found {value!r}")
        value = value[0]
    return int(value)


def frame_bytes(image: Image.Image, dtype: str) -> bytes:
    raw = image.tobytes()
    expected = image.width * image.height * {"u8": 1, "u16": 2, "u32": 4, "f32": 4}[dtype]
    if len(raw) != expected:
        raise ValueError(f"unexpected decoded byte count {len(raw)}; expected {expected}")
    return raw


def payload_summary(payload: bytes, dtype: str) -> tuple[Any, Any, str, str]:
    if dtype == "u8":
        values = list(payload)
        first_bits = f"{values[0]:02x}"
        last_bits = f"{values[-1]:02x}"
    elif dtype == "u16":
        values = list(struct.unpack(f"<{len(payload) // 2}H", payload))
        first_bits = f"{values[0]:04x}"
        last_bits = f"{values[-1]:04x}"
    elif dtype == "u32":
        values = list(struct.unpack(f"<{len(payload) // 4}I", payload))
        first_bits = f"{values[0]:08x}"
        last_bits = f"{values[-1]:08x}"
    elif dtype == "f32":
        bits = list(struct.unpack(f"<{len(payload) // 4}I", payload))
        values = [struct.unpack("<f", struct.pack("<I", item))[0] for item in bits]
        first_bits = f"{bits[0]:08x}"
        last_bits = f"{bits[-1]:08x}"
    else:
        raise ValueError(f"unsupported dtype {dtype!r}")
    finite = all(math.isfinite(float(value)) for value in values)
    return (
        min(values) if finite else None,
        max(values) if finite else None,
        first_bits,
        last_bits,
    )


def load_ome_schema(path: Path) -> etree.XMLSchema:
    parser = etree.XMLParser(resolve_entities=False, no_network=True)
    document = etree.parse(str(path), parser)
    return etree.XMLSchema(document)


def inspect_file(
    path: Path,
    row: dict[str, str],
    ome_schema: etree.XMLSchema,
    expected_ome: str,
) -> tuple[dict[str, Any], bytes]:
    encoded = path.read_bytes()
    if encoded[:2] != b"II" or encoded[2:4] != b"*\0":
        raise ValueError("source is not classic little-endian TIFF version 42")

    payload = bytearray()
    frame_tags: list[dict[str, Any]] = []
    with Image.open(path) as image:
        if image.format != "TIFF":
            raise ValueError("Pillow did not identify TIFF")
        if image.n_frames != int(row["pages"]):
            raise ValueError(
                f"IFD count {image.n_frames} does not match {row['pages']} for {row['path']}"
            )
        for frame in range(image.n_frames):
            image.seek(frame)
            image.load()
            if image.size != (int(row["width"]), int(row["height"])):
                raise ValueError(f"unexpected dimensions for {row['path']} frame {frame}")
            compression = tag_int(require_tag(image, 259))
            photometric = tag_int(require_tag(image, 262))
            samples = tag_int(image.tag_v2.get(277, 1))
            planar = tag_int(image.tag_v2.get(284, 1))
            bits = tag_int(require_tag(image, 258))
            sample_format = tag_int(image.tag_v2.get(339, 1))
            rows_per_strip = tag_int(require_tag(image, 278))
            expected_bits, expected_sample_format = {
                "u8": (8, 1),
                "u16": (16, 1),
                "u32": (32, 1),
                "f32": (32, 3),
            }[row["dtype"]]
            expected_rows = (
                int(row["height"])
                if row["rows_per_strip"] == "full"
                else int(row["rows_per_strip"])
            )
            if (compression, photometric, samples, planar) != (1, 1, 1, 1):
                raise ValueError("TIFF must be uncompressed one-sample chunky grayscale")
            if (bits, sample_format, rows_per_strip) != (
                expected_bits,
                expected_sample_format,
                expected_rows,
            ):
                raise ValueError(f"dtype/layout tags do not match {row['path']}")
            strip_offsets = require_tag(image, 273)
            strip_byte_counts = require_tag(image, 279)
            for forbidden in FORBIDDEN_METADATA_TAGS:
                if image.tag_v2.get(forbidden) is not None:
                    raise ValueError(f"forbidden mutable TIFF metadata tag {forbidden}")

            description_value = image.tag_v2.get(270)
            description = (
                normalize_scalar(description_value).rstrip("\0")
                if description_value is not None
                else None
            )
            if row["spec_id"] == "SRC-TIFF-SPEC-001" and frame == 0:
                if description != expected_ome:
                    raise ValueError("OME ImageDescription bytes differ from specification")
                xml = etree.fromstring(description.encode("utf-8"))
                ome_schema.assertValid(xml)
                pixels = xml.find(
                    ".//{http://www.openmicroscopy.org/Schemas/OME/2016-06}Pixels"
                )
                if pixels is None:
                    raise ValueError("OME Pixels element is absent")
                if int(pixels.get("SizeZ", "0")) != image.n_frames:
                    raise ValueError("OME SizeZ contradicts IFD count")
            elif description is not None:
                raise ValueError("unexpected ImageDescription outside first OME IFD")

            resolution_unit = image.tag_v2.get(296)
            if row["spec_id"] != "SRC-TIFF-SPEC-001" and resolution_unit not in (None, 1):
                raise ValueError("non-OME source unexpectedly declares physical resolution")

            payload.extend(frame_bytes(image, row["dtype"]))
            frame_tags.append(
                {
                    "frame": frame,
                    "bits_per_sample": bits,
                    "sample_format": sample_format,
                    "compression": compression,
                    "photometric": photometric,
                    "samples_per_pixel": samples,
                    "planar_configuration": planar,
                    "rows_per_strip": rows_per_strip,
                    "strip_offsets": strip_offsets,
                    "strip_byte_counts": strip_byte_counts,
                    "image_description": description is not None,
                }
            )

    minimum, maximum, first_bits, last_bits = payload_summary(bytes(payload), row["dtype"])
    observation = {
        "path": row["path"],
        "specification_id": row["spec_id"],
        "expected_class": row["expected_class"],
        "byte_order": "little",
        "tiff_version": 42,
        "dtype": row["dtype"],
        "width": int(row["width"]),
        "height": int(row["height"]),
        "ifd_count": int(row["pages"]),
        "encoded_bytes": len(encoded),
        "logical_bytes": len(payload),
        "logical_value_sha256": sha256(bytes(payload)),
        "minimum": minimum,
        "maximum": maximum,
        "first_value_bits_hex": first_bits,
        "last_value_bits_hex": last_bits,
        "frames": frame_tags,
    }
    return observation, bytes(payload)


def validate_path_set(expected: list[str], observed: list[str]) -> None:
    if len(observed) != len(set(observed)):
        raise ValueError("duplicate grouped source member")
    if sorted(observed) != sorted(expected):
        raise ValueError("grouped source member set differs from specification")


def mutate_file(root: Path, recipe: dict[str, Any]) -> bytes:
    base = (root / recipe["base_path"]).read_bytes()
    operation = recipe["operation"]
    if operation in {"replace_bytes", "replace_ome_sizez"}:
        offset = int(recipe["byte_offset"])
        original = bytes.fromhex(recipe["original_hex"])
        replacement = bytes.fromhex(recipe["replacement_hex"])
        if base[offset : offset + len(original)] != original:
            raise ValueError(f"mutation {recipe['id']} original bytes do not match")
        return base[:offset] + replacement + base[offset + len(original) :]
    if operation in {"truncate_header", "truncate_ifd", "truncate_strip_data"}:
        return base[: int(recipe["truncate_at"])]
    if operation == "replace_with_multipage_file":
        return (root / recipe["replacement_path"]).read_bytes()
    raise ValueError(f"operation {operation!r} is not a byte mutation")


def grouping_mutation_digest(paths: list[str]) -> str:
    return sha256(canonical_json({"paths": paths}))


def run_negative_cases(
    root: Path,
    rows_by_path: dict[str, dict[str, str]],
    recipes: list[dict[str, Any]],
    ome_schema: etree.XMLSchema,
    expected_ome: str,
) -> list[dict[str, str]]:
    expected_paths = sorted(rows_by_path)
    outcomes = []
    for recipe in recipes:
        operation = recipe["operation"]
        rejected = False
        if operation in {"remove_group_member", "duplicate_group_member"}:
            paths = expected_paths.copy()
            if operation == "remove_group_member":
                paths.remove(recipe["base_path"])
            else:
                paths.append(recipe["base_path"])
            if grouping_mutation_digest(paths) != recipe["mutated_sha256"]:
                raise ValueError(f"mutation digest mismatch for {recipe['id']}")
            try:
                validate_path_set(expected_paths, paths)
            except ValueError:
                rejected = True
        else:
            mutated = mutate_file(root, recipe)
            if sha256(mutated) != recipe["mutated_sha256"] or len(mutated) != int(
                recipe["mutated_bytes"]
            ):
                raise ValueError(f"mutation output mismatch for {recipe['id']}")
            with tempfile.TemporaryDirectory(prefix="mirante4d-reader-negative-") as temporary:
                candidate = Path(temporary) / Path(recipe["base_path"]).name
                candidate.write_bytes(mutated)
                try:
                    inspect_file(
                        candidate,
                        rows_by_path[recipe["base_path"]],
                        ome_schema,
                        expected_ome,
                    )
                except Exception:
                    rejected = True
        if not rejected:
            raise RuntimeError(f"negative case was accepted: {recipe['id']}")
        outcomes.append({"id": recipe["id"], "status": "rejected_as_expected"})
    return outcomes


def observe(
    root: Path,
    spec_path: Path,
    ome_path: Path,
    xsd_path: Path,
    mutations_path: Path,
) -> dict[str, Any]:
    _, rows = parse_spec(spec_path)
    rows_by_path = {row["path"]: row for row in rows}
    actual_paths = sorted(
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in {".tif", ".tiff"}
    )
    validate_path_set(sorted(rows_by_path), actual_paths)

    ome_schema = load_ome_schema(xsd_path)
    expected_ome = ome_path.read_text(encoding="utf-8")
    observations = []
    global_payload = bytearray()
    for row in rows:
        observation, payload = inspect_file(root / row["path"], row, ome_schema, expected_ome)
        observations.append(observation)
        global_payload.extend(payload)

    mutations = json.loads(mutations_path.read_text(encoding="utf-8"))
    if mutations.get("schema") != "mirante4d-source-fixture-bound-mutations":
        raise ValueError("bound mutation manifest has an unexpected schema")
    negative = run_negative_cases(
        root,
        rows_by_path,
        mutations["recipes"],
        ome_schema,
        expected_ome,
    )
    return {
        "schema": "mirante4d-source-fixture-independent-reader-report",
        "schema_version": 1,
        "status": "passed",
        "lineage_id": "SRC-READER-001",
        "pillow": pillow_version,
        "lxml": ".".join(map(str, etree.LXML_VERSION)),
        "ome_xsd_sha256": sha256(xsd_path.read_bytes()),
        "files": observations,
        "logical_value_digest_algorithm": "sha256",
        "logical_value_sha256": sha256(bytes(global_payload)),
        "logical_voxel_bytes": len(global_payload),
        "negative_cases": negative,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    probe = subparsers.add_parser("probe")
    probe.add_argument("--report", type=Path, required=True)
    inspect = subparsers.add_parser("observe")
    inspect.add_argument("--root", type=Path, required=True)
    inspect.add_argument("--spec", type=Path, required=True)
    inspect.add_argument("--ome-xml", type=Path, required=True)
    inspect.add_argument("--xsd", type=Path, required=True)
    inspect.add_argument("--mutations", type=Path, required=True)
    inspect.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()

    if args.command == "probe":
        result = run_capability_probe()
    else:
        result = observe(args.root, args.spec, args.ome_xml, args.xsd, args.mutations)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_bytes(canonical_json(result))


if __name__ == "__main__":
    main()
