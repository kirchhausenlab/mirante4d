#!/usr/bin/env python3
"""M4D-T1-PRODUCER-001: independently emit exact target-package bytes.

This producer is intentionally an operational byte writer, not a scientific
fact authority.  It reads the approved declarative case table and opaque
ScientificContentId strings produced by the independent fact oracle.  It does
not import Mirante4D, NumPy, zarr-python, or either other T1 lineage.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import math
from pathlib import Path, PurePosixPath
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
from typing import Any, Iterable


PRODUCER_ID = "M4D-T1-PRODUCER-001"
REPORT_SCHEMA = "mirante4d-target-t1-producer-operational-report"
EXPECTED_CASE_IDS = [
    "m4d-t1-u8-2d-sparse",
    "m4d-t1-u16-3d-multiscale",
    "m4d-t1-f32-3d-validity",
]
EXPECTED_HEADER = [
    "spec_version",
    "case_id",
    "dtype",
    "t",
    "c",
    "z",
    "y",
    "x",
    "levels",
    "validity",
    "physical_channels",
    "temporal_step_f64_bits",
    "grid_to_world_f64_bits",
    "ome_projection",
    "ome_level_scale_zyx_f64_bits",
    "ome_level_translation_zyx_f64_bits",
    "value_rule",
    "value_parameters",
    "validity_rule",
    "validity_parameters",
]

ZSTD = Path("/usr/bin/zstd")
EXPECTED_ZSTD_SHA256 = "7c5468b370f7c47eda07281e3437fafc568f95d10420051e3aa522709f9342c5"
EXPECTED_ZSTD_VERSION_FRAGMENT = "v1.5.5"

CAPABILITIES = [
    "m4d.bit-validity.v1",
    "m4d.identity.v1",
    "m4d.packed-index.v1",
    "m4d.strict-profile.v1",
    "zarr.sharding-indexed.v1",
]
MISSING = (1 << 64) - 1
MANIFEST_PAGE_BYTES_MAX = 1_048_576

FLAG_OCCUPIED = 1 << 0
FLAG_PIXEL_PAYLOAD_PRESENT = 1 << 1
FLAG_EXPLICIT_VALIDITY = 1 << 2
FLAG_ALL_VALID = 1 << 3
FLAG_ALL_INVALID = 1 << 4
FLAG_NUMERIC_RANGE_PRESENT = 1 << 5

ARCHIVE_BYTES_MAX = 16 * 1024 * 1024
COMBINED_ARCHIVE_BYTES_MAX = 32 * 1024 * 1024
COMBINED_REGULAR_FILE_BYTES_MAX = 64 * 1024 * 1024
COMBINED_LOGICAL_VOXEL_BYTES_MAX = 64 * 1024 * 1024
FILE_COUNT_MAX = 64
DIRECTORY_COUNT_MAX = 64
DIRECTORY_DEPTH_MAX = 8
FAN_OUT_MAX = 64
PATH_BYTES_MAX = 240
INDIVIDUAL_FILE_BYTES_MAX = 32 * 1024 * 1024
COMPRESSION_RATIO_MAX = 16

REPOSITORY_ROOT = Path(__file__).resolve().parents[4]
CANDIDATE_ROOT = REPOSITORY_ROOT / "target/mirante4d/fixture-candidates"

GROUP_BYTES = b'{"zarr_format":3,"node_type":"group"}'

KIND_FACTS = {
    "zarr_root": ("application/vnd.zarr+json", "zarr.root"),
    "zarr_images_group": ("application/vnd.zarr+json", "zarr.images-group"),
    "zarr_validity_group": ("application/vnd.zarr+json", "zarr.validity-group"),
    "zarr_indexes_group": ("application/vnd.zarr+json", "zarr.indexes-group"),
    "zarr_image_group": ("application/vnd.zarr+json", "zarr.image-group"),
    "zarr_pixel_array": ("application/vnd.zarr+json", "zarr.pixel-array"),
    "zarr_validity_array": ("application/vnd.zarr+json", "zarr.validity-array"),
    "zarr_packed_index_array": (
        "application/vnd.zarr+json",
        "zarr.packed-index-array",
    ),
    "pixel_shard": ("application/vnd.zarr.shard", "pixel.shard"),
    "validity_shard": ("application/vnd.zarr.shard", "validity.shard"),
    "packed_index_shard": ("application/vnd.zarr.shard", "packed-index.shard"),
    "profile": ("application/vnd.mirante4d.profile+json", "m4d.profile"),
    "science": ("application/vnd.mirante4d.science+json", "m4d.science"),
    "display": (
        "application/vnd.mirante4d.display-defaults+json",
        "m4d.display-defaults",
    ),
}


class ProducerError(RuntimeError):
    """A deterministic producer input or output failure."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ProducerError(message)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_json(value: Any) -> bytes:
    """Encode the restricted control-object value set as canonical JCS bytes."""
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def record_json(value: Any) -> bytes:
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        )
        + "\n"
    ).encode("utf-8")


def semantic_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def parse_positive_int(value: str, field: str) -> int:
    require(value.isascii() and value.isdigit(), f"{field} must be one ASCII integer")
    parsed = int(value)
    require(parsed > 0, f"{field} must be positive")
    return parsed


def parse_hex(value: str, digits: int, field: str) -> int:
    require(
        len(value) == digits
        and value == value.lower()
        and all(character in "0123456789abcdef" for character in value),
        f"{field} must contain exactly {digits} lowercase hexadecimal digits",
    )
    return int(value, 16)


def parse_f64_bits(value: str, field: str) -> tuple[str, float]:
    bits = parse_hex(value, 16, field)
    decoded = struct.unpack(">d", bits.to_bytes(8, "big"))[0]
    require(math.isfinite(decoded), f"{field} must be finite")
    if decoded == 0.0:
        decoded = 0.0
        value = "0000000000000000"
    return value, decoded


def split_exact(value: str, separator: str, count: int, field: str) -> list[str]:
    parts = value.split(separator)
    require(len(parts) == count, f"{field} must contain exactly {count} values")
    return parts


class CaseSpec:
    def __init__(self, row: dict[str, str]) -> None:
        require(row["spec_version"] == "1", "only case specification version 1 is accepted")
        self.case_id = row["case_id"]
        require(self.case_id in EXPECTED_CASE_IDS, f"unexpected case id {self.case_id!r}")
        self.dtype = row["dtype"]
        require(self.dtype in {"uint8", "uint16", "float32"}, "unsupported dtype")
        self.t = parse_positive_int(row["t"], "t")
        self.c = parse_positive_int(row["c"], "c")
        self.z = parse_positive_int(row["z"], "z")
        self.y = parse_positive_int(row["y"], "y")
        self.x = parse_positive_int(row["x"], "x")
        self.levels = parse_positive_int(row["levels"], "levels")
        require(self.levels <= 7, "levels exceed the frozen profile")
        self.validity = row["validity"]
        require(self.validity in {"all_valid", "explicit"}, "unsupported validity mode")

        self.physical_channels = [
            int(value)
            for value in split_exact(
                row["physical_channels"], ",", self.c, "physical_channels"
            )
        ]
        require(
            sorted(self.physical_channels) == list(range(self.c)),
            "physical_channels must be a permutation of zero through c-1",
        )
        self.logical_for_physical = [0] * self.c
        for logical, physical in enumerate(self.physical_channels):
            self.logical_for_physical[physical] = logical

        self.temporal_step_bits, self.temporal_step = parse_f64_bits(
            row["temporal_step_f64_bits"], "temporal_step_f64_bits"
        )
        require(self.temporal_step > 0.0, "temporal step must be positive")

        transform_fields = split_exact(
            row["grid_to_world_f64_bits"], ",", 16, "grid_to_world_f64_bits"
        )
        parsed_transform = [
            parse_f64_bits(value, "grid_to_world_f64_bits") for value in transform_fields
        ]
        self.grid_bits = [value[0] for value in parsed_transform]
        self.grid_values = [value[1] for value in parsed_transform]

        self.ome_projection = row["ome_projection"]
        require(
            self.ome_projection in {"diagonal_micrometer", "unitless_identity"},
            "unsupported OME projection",
        )
        scale_rows = split_exact(
            row["ome_level_scale_zyx_f64_bits"],
            ";",
            self.levels,
            "ome_level_scale_zyx_f64_bits",
        )
        translation_rows = split_exact(
            row["ome_level_translation_zyx_f64_bits"],
            ";",
            self.levels,
            "ome_level_translation_zyx_f64_bits",
        )
        self.ome_scale_bits: list[list[str]] = []
        self.ome_scales: list[list[float]] = []
        self.ome_translation_bits: list[list[str]] = []
        self.ome_translations: list[list[float]] = []
        for ordinal, encoded in enumerate(scale_rows):
            parsed = [
                parse_f64_bits(value, f"OME level {ordinal} scale")
                for value in split_exact(encoded, ",", 3, "OME scale row")
            ]
            self.ome_scale_bits.append([value[0] for value in parsed])
            self.ome_scales.append([value[1] for value in parsed])
        for ordinal, encoded in enumerate(translation_rows):
            parsed = [
                parse_f64_bits(value, f"OME level {ordinal} translation")
                for value in split_exact(encoded, ",", 3, "OME translation row")
            ]
            self.ome_translation_bits.append([value[0] for value in parsed])
            self.ome_translations.append([value[1] for value in parsed])

        self.value_rule = row["value_rule"]
        self.validity_rule = row["validity_rule"]
        self.sparse_values: dict[tuple[int, int, int, int, int], int] = {}
        self.affine_parameters: list[int] = []
        self.f32_cycle: list[int] = []
        if self.value_rule == "sparse_points":
            for encoded in row["value_parameters"].split(";"):
                fields = split_exact(encoded, ",", 6, "sparse point")
                coordinate = tuple(int(value) for value in fields[:5])
                sample = int(fields[5])
                require(coordinate not in self.sparse_values, "duplicate sparse point")
                require(
                    0 <= coordinate[0] < self.t
                    and 0 <= coordinate[1] < self.c
                    and 0 <= coordinate[2] < self.z
                    and 0 <= coordinate[3] < self.y
                    and 0 <= coordinate[4] < self.x,
                    "sparse point is outside the case shape",
                )
                require(0 <= sample <= 255, "sparse uint8 value is outside range")
                self.sparse_values[coordinate] = sample
        elif self.value_rule == "affine_mod_decimate":
            fields = split_exact(
                row["value_parameters"], ",", 6, "affine_mod_decimate parameters"
            )
            expected_names = ["t", "c", "z", "y", "x", "mod"]
            self.affine_parameters = []
            for expected, field in zip(expected_names, fields, strict=True):
                name, separator, value = field.partition("=")
                require(separator == "=" and name == expected, "affine parameter labels drifted")
                self.affine_parameters.append(int(value))
            require(self.dtype == "uint16", "affine_mod_decimate requires uint16")
            require(self.affine_parameters[-1] > 0, "affine modulus must be positive")
        elif self.value_rule == "f32_cycle":
            prefix = "cycle_bits="
            require(row["value_parameters"].startswith(prefix), "f32 cycle label drifted")
            self.f32_cycle = [
                parse_hex(value, 8, "f32_cycle value")
                for value in row["value_parameters"][len(prefix) :].split(",")
            ]
            require(self.dtype == "float32" and self.f32_cycle, "f32_cycle requires values")
            require(
                all(math.isfinite(bits_to_f32(bits)) for bits in self.f32_cycle),
                "the positive float case must contain only finite cycle values",
            )
        else:
            raise ProducerError(f"unsupported value rule {self.value_rule!r}")

        self.validity_parameters: list[int] = []
        if self.validity_rule == "all_valid":
            require(self.validity == "all_valid", "all_valid rule requires implicit validity")
            require(row["validity_parameters"] == "", "all_valid takes no parameters")
        elif self.validity_rule == "mixed_and_all_invalid":
            fields = split_exact(
                row["validity_parameters"],
                ",",
                3,
                "mixed_and_all_invalid parameters",
            )
            expected_names = ["all_invalid_channel", "mixed_modulus", "mixed_channel_stride"]
            self.validity_parameters = []
            for expected, field in zip(expected_names, fields, strict=True):
                name, separator, value = field.partition("=")
                require(separator == "=" and name == expected, "validity parameter labels drifted")
                self.validity_parameters.append(int(value))
            require(self.validity == "explicit", "mixed validity requires explicit storage")
            all_invalid_channel, modulus, _channel_stride = self.validity_parameters
            require(0 <= all_invalid_channel < self.c, "all-invalid channel is outside range")
            require(modulus > 1, "validity modulus must exceed one")
        else:
            raise ProducerError(f"unsupported validity rule {self.validity_rule!r}")

        logical_bytes = self.logical_voxel_bytes
        require(
            logical_bytes <= COMBINED_LOGICAL_VOXEL_BYTES_MAX,
            "case exceeds the combined logical-byte ceiling",
        )

    @property
    def sample_bytes(self) -> int:
        return {"uint8": 1, "uint16": 2, "float32": 4}[self.dtype]

    @property
    def logical_voxel_bytes(self) -> int:
        level_voxels = sum(product(self.level_shape(level)) for level in range(self.levels))
        return self.t * self.c * level_voxels * self.sample_bytes

    @property
    def is_2d(self) -> bool:
        return self.z == 1

    def level_shape(self, level: int) -> tuple[int, int, int]:
        require(0 <= level < self.levels, "level is outside the case")
        factor = 1 << level
        return tuple((dimension + factor - 1) // factor for dimension in (self.z, self.y, self.x))

    def value_bits(self, level: int, logical_c: int, t: int, z: int, y: int, x: int) -> int:
        factor = 1 << level
        base_z = min(factor * z, self.z - 1)
        base_y = min(factor * y, self.y - 1)
        base_x = min(factor * x, self.x - 1)
        if self.value_rule == "sparse_points":
            return self.sparse_values.get((t, logical_c, base_z, base_y, base_x), 0)
        if self.value_rule == "affine_mod_decimate":
            t_factor, c_factor, z_factor, y_factor, x_factor, modulus = self.affine_parameters
            return (
                t_factor * t
                + c_factor * logical_c
                + z_factor * base_z
                + y_factor * base_y
                + x_factor * base_x
            ) % modulus
        if self.value_rule == "f32_cycle":
            linear = (
                (((t * self.c + logical_c) * self.z + base_z) * self.y + base_y)
                * self.x
                + base_x
            )
            return self.f32_cycle[linear % len(self.f32_cycle)]
        raise AssertionError("value rule was validated during parsing")

    def is_valid(self, logical_c: int, z: int, y: int, x: int) -> bool:
        if self.validity_rule == "all_valid":
            return True
        all_invalid_channel, modulus, channel_stride = self.validity_parameters
        if logical_c == all_invalid_channel:
            return False
        spatial = z * self.y * self.x + y * self.x + x
        return (spatial + channel_stride * logical_c) % modulus != 0


def bits_to_f32(bits: int) -> float:
    return struct.unpack("<f", struct.pack("<I", bits))[0]


def load_specs(path: Path) -> list[CaseSpec]:
    with path.open(newline="", encoding="utf-8") as source:
        reader = csv.DictReader(source, delimiter="|")
        require(reader.fieldnames == EXPECTED_HEADER, "unexpected target case header")
        rows = list(reader)
    cases = [CaseSpec(row) for row in rows]
    require([case.case_id for case in cases] == EXPECTED_CASE_IDS, "case order or set drifted")
    return cases


def load_opaque_ids(path: Path) -> dict[str, str]:
    document = json.loads(path.read_text(encoding="utf-8"))
    require(isinstance(document, dict), "oracle facts must be one JSON object")
    rows = document.get("cases")
    require(isinstance(rows, list), "oracle facts must contain a cases array")
    result: dict[str, str] = {}
    for row in rows:
        require(isinstance(row, dict), "oracle case fact must be an object")
        case_id = row.get("case_id", row.get("id"))
        identity = row.get("scientific_content_id")
        require(isinstance(case_id, str) and isinstance(identity, str), "oracle ID row is invalid")
        require(case_id not in result, "oracle facts contain a duplicate case")
        require(identity != "", "oracle supplied an empty identity")
        result[case_id] = identity
    require(sorted(result) == sorted(EXPECTED_CASE_IDS), "oracle identity case set drifted")
    return result


def verify_zstd() -> dict[str, str]:
    metadata = ZSTD.lstat()
    require(stat.S_ISREG(metadata.st_mode), "/usr/bin/zstd must be one regular file")
    digest = sha256_file(ZSTD)
    require(digest == EXPECTED_ZSTD_SHA256, "/usr/bin/zstd digest drifted")
    result = subprocess.run(
        [str(ZSTD), "--version"],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    version = result.stdout.strip()
    require(EXPECTED_ZSTD_VERSION_FRAGMENT in version, "/usr/bin/zstd version drifted")
    return {"path": str(ZSTD), "version": version, "sha256": digest}


def encode_zstd(decoded: bytes) -> bytes:
    result = subprocess.run(
        [
            str(ZSTD),
            "-3",
            f"--stream-size={len(decoded)}",
            "--no-check",
            "--quiet",
            "--stdout",
        ],
        input=decoded,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
        timeout=60,
    )
    return result.stdout


def crc32c_table() -> tuple[int, ...]:
    table = []
    for byte in range(256):
        value = byte
        for _ in range(8):
            value = (value >> 1) ^ (0x82F63B78 if value & 1 else 0)
        table.append(value)
    return tuple(table)


CRC32C_TABLE = crc32c_table()


def crc32c(data: bytes) -> int:
    value = 0xFFFFFFFF
    for byte in data:
        value = CRC32C_TABLE[(value ^ byte) & 0xFF] ^ (value >> 8)
    return value ^ 0xFFFFFFFF


def encode_inner(decoded: bytes) -> bytes:
    compressed = encode_zstd(decoded)
    return compressed + struct.pack("<I", crc32c(compressed))


def encode_shard(encoded_slots: list[bytes | None]) -> bytes | None:
    if all(slot is None for slot in encoded_slots):
        return None
    payload = bytearray()
    index = bytearray()
    for slot in encoded_slots:
        if slot is None:
            index.extend(struct.pack("<QQ", MISSING, MISSING))
        else:
            offset = len(payload)
            payload.extend(slot)
            index.extend(struct.pack("<QQ", offset, len(slot)))
    return bytes(payload) + bytes(index) + struct.pack("<I", crc32c(index))


def ceil_div(value: int, divisor: int) -> int:
    return (value + divisor - 1) // divisor


def product(values: Iterable[int]) -> int:
    result = 1
    for value in values:
        result *= value
    return result


def array_metadata(
    *,
    shape: list[int],
    dtype: str,
    inner_shape: list[int],
    outer_shape: list[int],
    chunks_per_shard: int,
    dimension_names: list[str] | None,
) -> bytes:
    require(product(outer_shape) // product(inner_shape) == chunks_per_shard, "bad shard row")
    value: dict[str, Any] = {
        "zarr_format": 3,
        "node_type": "array",
        "shape": shape,
        "data_type": dtype,
        "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": outer_shape}},
        "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
        "fill_value": 0.0 if dtype == "float32" else 0,
        "codecs": [
            {
                "name": "sharding_indexed",
                "configuration": {
                    "chunk_shape": inner_shape,
                    "codecs": [
                        {"name": "bytes", "configuration": {"endian": "little"}},
                        {"name": "zstd", "configuration": {"level": 3, "checksum": False}},
                        {"name": "crc32c"},
                    ],
                    "index_codecs": [
                        {"name": "bytes", "configuration": {"endian": "little"}},
                        {"name": "crc32c"},
                    ],
                    "index_location": "end",
                },
            }
        ],
    }
    if dimension_names is not None:
        value["dimension_names"] = dimension_names
    return semantic_json(value)


def ome_metadata(case: CaseSpec) -> bytes:
    axes: list[dict[str, Any]] = [
        {"name": "t", "type": "time", "unit": "second"},
        {"name": "c", "type": "channel"},
        {"name": "z", "type": "space"},
        {"name": "y", "type": "space"},
        {"name": "x", "type": "space"},
    ]
    if case.ome_projection == "diagonal_micrometer":
        for axis in axes[2:]:
            axis["unit"] = "micrometer"

    datasets = []
    for ordinal in range(case.levels):
        if case.ome_projection == "diagonal_micrometer":
            spatial_scale = case.ome_scales[ordinal]
            spatial_translation = case.ome_translations[ordinal]
        else:
            spatial_scale = [1.0, 1.0, 1.0]
            spatial_translation = [0.0, 0.0, 0.0]
        scale = [case.temporal_step, 1.0, *spatial_scale]
        translation = [0.0, 0.0, *spatial_translation]
        transformations: list[dict[str, Any]] = [{"type": "scale", "scale": scale}]
        if any(value != 0.0 for value in translation):
            transformations.append({"type": "translation", "translation": translation})
        datasets.append(
            {
                "path": f"s{ordinal:02d}",
                "coordinateTransformations": transformations,
            }
        )
    return semantic_json(
        {
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [{"axes": axes, "datasets": datasets}],
                }
            },
        }
    )


def profile_bytes(case: CaseSpec, scientific_content_id: str) -> bytes:
    levels = []
    for ordinal in range(case.levels):
        suffix = f"i00000000-s{ordinal:02d}"
        levels.append(
            {
                "scale_ordinal": str(ordinal),
                "pixel_path": f"images/i00000000/s{ordinal:02d}",
                "validity_mode": case.validity,
                "validity_path": (
                    f"validity/{suffix}" if case.validity == "explicit" else None
                ),
                "packed_index_path": f"indexes/{suffix}",
            }
        )
    compatibility = {
        "format_family": "mirante4d",
        "lifecycle": "EXPERIMENTAL",
        "semantic_schema": "m4d-science-1.0",
        "storage_profile": "m4d-zarr3-local-1.0",
        "index_profile": "m4d-packed-index-1.0",
        "identity_profile": "m4d-id-1",
        "ome_metadata_version": "0.5",
        "ome_release": "0.5.2",
        "zarr_format": 3,
        "zarr_core": "3.0",
        "required_capabilities": CAPABILITIES,
        "unknown_major_or_required_capability": "reject",
        "compatibility_fallback": "forbidden",
    }
    return canonical_json(
        {
            "schema": "m4d-profile",
            "schema_version": 1,
            "compatibility": compatibility,
            "required_capabilities": CAPABILITIES,
            "scientific_content_id": scientific_content_id,
            "science_path": "m4d/science.json",
            "display_defaults_path": "m4d/display.json",
            "manifest_root_path": "m4d/manifest/root.json",
            "images": [
                {
                    "image_ordinal": "0",
                    "image_group_path": "images/i00000000",
                    "logical_layers": [
                        {
                            "logical_layer_ordinal": str(logical),
                            "physical_channel": str(physical),
                        }
                        for logical, physical in enumerate(case.physical_channels)
                    ],
                    "levels": levels,
                }
            ],
            "portable_record_paths": [],
            "ome_interoperability_base": (
                "IO-1"
                if case.validity == "explicit" or case.ome_projection == "unitless_identity"
                else "IO-2"
            ),
        }
    )


def science_bytes(case: CaseSpec, scientific_content_id: str) -> bytes:
    layers = []
    for logical in range(case.c):
        layers.append(
            {
                "logical_layer_ordinal": str(logical),
                "base_shape_tzyx": [str(case.t), str(case.z), str(case.y), str(case.x)],
                "dtype": case.dtype,
                "temporal_calibration": {
                    "type": "regular",
                    "step_seconds_f64_bits": case.temporal_step_bits,
                },
                "grid_to_world_micrometer_f64_bits": case.grid_bits,
            }
        )
    return canonical_json(
        {
            "schema": "m4d-science",
            "schema_version": 1,
            "semantic_schema": "m4d-science-1.0",
            "voxel_center_convention": "integer coordinates address voxel centers",
            "spatial_unit": "micrometer",
            "scientific_content_id": scientific_content_id,
            "layers": layers,
        }
    )


def display_bytes(case: CaseSpec) -> bytes:
    windows = {
        "uint8": ("00000000", "437f0000"),
        "uint16": ("00000000", "477fff00"),
        "float32": ("ff7fffff", "7f7fffff"),
    }
    colors = ["ffffff", "ff0000", "00ff00", "0000ff"]
    minimum, maximum = windows[case.dtype]
    return canonical_json(
        {
            "schema": "m4d-display-defaults",
            "schema_version": 1,
            "layers": [
                {
                    "logical_layer_ordinal": str(logical),
                    "visible": True,
                    "color_rgb": colors[logical],
                    "window_min_f32_bits": minimum,
                    "window_max_f32_bits": maximum,
                }
                for logical in range(case.c)
            ],
        }
    )


def pixel_row(case: CaseSpec) -> tuple[list[int], list[int], int, int]:
    if case.is_2d:
        inner = [1, 1, 1, 256, 256]
        outer = [1, 1, 1, 1024, 1024]
        return inner, outer, 16, 256
    inner = [1, 1, 64, 64, 64]
    outer = [1, 1, 256, 256, 256]
    return inner, outer, 64, 64


def validity_row(case: CaseSpec) -> tuple[list[int], list[int], int]:
    require(not case.is_2d, "the v1 explicit-validity case is three-dimensional")
    return [1, 1, 64, 64, 8], [1, 1, 256, 256, 32], 64


def sample_nonfill(case: CaseSpec, bits: int) -> bool:
    return bits != 0


def sample_order_value(case: CaseSpec, bits: int) -> float | int:
    if case.dtype != "float32":
        return bits
    # Match IEEE totalOrder as used by Rust f32::total_cmp.  This preserves the
    # distinction and order between negative and positive zero without relying
    # on the host language's numeric comparison semantics.
    return (~bits & 0xFFFFFFFF) if bits & 0x80000000 else bits | 0x80000000


def write_sample(buffer: bytearray, offset: int, case: CaseSpec, bits: int) -> None:
    if case.dtype == "uint8":
        buffer[offset] = bits
    elif case.dtype == "uint16":
        struct.pack_into("<H", buffer, offset, bits)
    else:
        struct.pack_into("<I", buffer, offset, bits)


def packed_record(
    *,
    scale: int,
    t: int,
    physical_c: int,
    z_chunk: int,
    y_chunk: int,
    x_chunk: int,
    capacity: int,
    valid_count: int,
    nonfill_count: int,
    minimum_bits: int | None,
    maximum_bits: int | None,
    pixel_present: bool,
    explicit_validity: bool,
) -> bytes:
    flags = 0
    if nonfill_count > 0:
        flags |= FLAG_OCCUPIED
    if pixel_present:
        flags |= FLAG_PIXEL_PAYLOAD_PRESENT
    if explicit_validity:
        flags |= FLAG_EXPLICIT_VALIDITY
    if valid_count == capacity:
        flags |= FLAG_ALL_VALID
    if valid_count == 0:
        flags |= FLAG_ALL_INVALID
    if valid_count > 0:
        flags |= FLAG_NUMERIC_RANGE_PRESENT
    return struct.pack(
        "<8I4Q",
        flags,
        0,
        scale,
        t,
        physical_c,
        z_chunk,
        y_chunk,
        x_chunk,
        valid_count,
        nonfill_count,
        0 if minimum_bits is None else minimum_bits,
        0 if maximum_bits is None else maximum_bits,
    )


def build_decoded_brick(
    case: CaseSpec,
    *,
    level: int,
    logical_c: int,
    t: int,
    z_chunk: int,
    y_chunk: int,
    x_chunk: int,
    inner_z: int,
    inner_y: int,
    inner_x: int,
) -> tuple[bytes | None, bytes | None, dict[str, Any]]:
    level_z, level_y, level_x = case.level_shape(level)
    origin_z = z_chunk * inner_z
    origin_y = y_chunk * inner_y
    origin_x = x_chunk * inner_x
    extent_z = min(inner_z, level_z - origin_z)
    extent_y = min(inner_y, level_y - origin_y)
    extent_x = min(inner_x, level_x - origin_x)
    capacity = extent_z * extent_y * extent_x
    pixel = bytearray(inner_z * inner_y * inner_x * case.sample_bytes)
    validity = bytearray(inner_z * inner_y * ceil_div(inner_x, 8)) if case.validity == "explicit" else None
    valid_count = 0
    nonfill_count = 0
    raw_nonfill_present = False
    minimum_value: float | int | None = None
    maximum_value: float | int | None = None
    minimum_bits: int | None = None
    maximum_bits: int | None = None

    for local_z in range(extent_z):
        z = origin_z + local_z
        for local_y in range(extent_y):
            y = origin_y + local_y
            for local_x in range(extent_x):
                x = origin_x + local_x
                bits = case.value_bits(level, logical_c, t, z, y, x)
                raw_nonfill_present = raw_nonfill_present or sample_nonfill(case, bits)
                pixel_index = (local_z * inner_y + local_y) * inner_x + local_x
                write_sample(pixel, pixel_index * case.sample_bytes, case, bits)
                is_valid = case.is_valid(logical_c, min((1 << level) * z, case.z - 1), min((1 << level) * y, case.y - 1), min((1 << level) * x, case.x - 1))
                if not is_valid:
                    continue
                valid_count += 1
                if validity is not None:
                    validity_index = (local_z * inner_y + local_y) * ceil_div(inner_x, 8) + local_x // 8
                    validity[validity_index] |= 1 << (local_x % 8)
                if sample_nonfill(case, bits):
                    nonfill_count += 1
                numeric = sample_order_value(case, bits)
                if minimum_value is None or numeric < minimum_value:
                    minimum_value = numeric
                    minimum_bits = bits
                if maximum_value is None or numeric > maximum_value:
                    maximum_value = numeric
                    maximum_bits = bits

    # The float authority deliberately retains its declared finite cycle bits
    # behind invalid samples.  Those bytes are not scientific values, and the
    # independent oracle canonicalizes them to +0 for identity purposes.
    pixel_present = nonfill_count > 0 or (
        case.value_rule == "f32_cycle" and raw_nonfill_present
    )
    validity_present = validity is not None and valid_count > 0
    return (
        bytes(pixel) if pixel_present else None,
        bytes(validity) if validity_present and validity is not None else None,
        {
            "capacity": capacity,
            "valid_count": valid_count,
            "nonfill_count": nonfill_count,
            "minimum_bits": minimum_bits,
            "maximum_bits": maximum_bits,
            "pixel_present": pixel_present,
        },
    )


class PackageBuilder:
    def __init__(self, case: CaseSpec, scientific_content_id: str) -> None:
        self.case = case
        self.scientific_content_id = scientific_content_id
        self.objects: dict[str, tuple[bytes, str]] = {}
        self.operational_levels: list[dict[str, int]] = []

    def add(self, path: str, data: bytes, kind: str) -> None:
        validate_package_path(path)
        require(kind in KIND_FACTS, f"unknown object kind {kind}")
        require(path not in self.objects, f"duplicate package object {path}")
        require(len(data) <= INDIVIDUAL_FILE_BYTES_MAX, f"package object is too large: {path}")
        self.objects[path] = (data, kind)

    def build(self) -> tuple[dict[str, bytes], dict[str, Any]]:
        self.add("zarr.json", GROUP_BYTES, "zarr_root")
        self.add("images/zarr.json", GROUP_BYTES, "zarr_images_group")
        self.add("validity/zarr.json", GROUP_BYTES, "zarr_validity_group")
        self.add("indexes/zarr.json", GROUP_BYTES, "zarr_indexes_group")
        self.add("m4d/profile.json", profile_bytes(self.case, self.scientific_content_id), "profile")
        self.add("m4d/science.json", science_bytes(self.case, self.scientific_content_id), "science")
        self.add("m4d/display.json", display_bytes(self.case), "display")
        self.add("images/i00000000/zarr.json", ome_metadata(self.case), "zarr_image_group")

        for level in range(self.case.levels):
            self.build_level(level)

        descriptors = [self.descriptor(path, data, kind) for path, (data, kind) in self.objects.items()]
        descriptors.sort(key=lambda value: value["path"])
        pages = pack_manifest_pages(descriptors)
        page_references = []
        for ordinal, page in enumerate(pages):
            path = f"m4d/manifest/pages/p{ordinal:08d}.json"
            page_bytes = canonical_json(
                {"schema": "m4d-manifest-page", "schema_version": 1, "entries": page}
            )
            self.objects[path] = (page_bytes, "__manifest_page__")
            page_references.append(
                {
                    "path": path,
                    "first_path": page[0]["path"],
                    "last_path": page[-1]["path"],
                    "entry_count": str(len(page)),
                    "bytes": str(len(page_bytes)),
                    "digest": f"sha256:{sha256(page_bytes)}",
                }
            )
        root_bytes = canonical_json(
            {"schema": "m4d-manifest-root", "schema_version": 1, "pages": page_references}
        )
        self.objects["m4d/manifest/root.json"] = (root_bytes, "__manifest_root__")
        package_id = f"m4d-package-v1-sha256:{sha256(root_bytes)}"
        files = {path: data for path, (data, _kind) in self.objects.items()}
        metrics = package_metrics(files)
        tree_rows = [
            {"path": path, "bytes": len(data), "sha256": sha256(data)}
            for path, data in sorted(files.items())
        ]
        return files, {
            "case_id": self.case.case_id,
            "injected_scientific_content_id": self.scientific_content_id,
            "package_id": package_id,
            "manifest_root_bytes": len(root_bytes),
            "manifest_root_sha256": sha256(root_bytes),
            "manifest_pages": len(pages),
            "manifest_descriptors": len(descriptors),
            "tree_sha256": sha256(record_json(tree_rows)),
            "metrics": metrics,
            "levels": self.operational_levels,
        }

    def descriptor(self, path: str, data: bytes, kind: str) -> dict[str, str]:
        media_type, logical_role = KIND_FACTS[kind]
        return {
            "path": path,
            "media_type": media_type,
            "logical_role": logical_role,
            "bytes": str(len(data)),
            "digest": f"sha256:{sha256(data)}",
        }

    def build_level(self, level: int) -> None:
        case = self.case
        level_z, level_y, level_x = case.level_shape(level)
        inner, outer, chunks_per_shard, inner_x = pixel_row(case)
        inner_z, inner_y = inner[2], inner[3]
        grid_z = ceil_div(level_z, inner_z)
        grid_y = ceil_div(level_y, inner_y)
        grid_x = ceil_div(level_x, inner_x)
        outer_grid_z = ceil_div(grid_z, 4)
        outer_grid_y = ceil_div(grid_y, 4)
        outer_grid_x = ceil_div(grid_x, 4)
        record_count = case.t * case.c * grid_z * grid_y * grid_x
        records: list[bytes | None] = [None] * record_count
        suffix = f"i00000000-s{level:02d}"
        pixel_base = f"images/i00000000/s{level:02d}"
        index_base = f"indexes/{suffix}"
        validity_base = f"validity/{suffix}"
        self.add(
            f"{pixel_base}/zarr.json",
            array_metadata(
                shape=[case.t, case.c, level_z, level_y, level_x],
                dtype=case.dtype,
                inner_shape=inner,
                outer_shape=outer,
                chunks_per_shard=chunks_per_shard,
                dimension_names=["t", "c", "z", "y", "x"],
            ),
            "zarr_pixel_array",
        )
        if case.validity == "explicit":
            validity_inner, validity_outer, validity_slots = validity_row(case)
            self.add(
                f"{validity_base}/zarr.json",
                array_metadata(
                    shape=[case.t, case.c, level_z, level_y, ceil_div(level_x, 8)],
                    dtype="uint8",
                    inner_shape=validity_inner,
                    outer_shape=validity_outer,
                    chunks_per_shard=validity_slots,
                    dimension_names=["t", "c", "z", "y", "x_byte"],
                ),
                "zarr_validity_array",
            )
        self.add(
            f"{index_base}/zarr.json",
            array_metadata(
                shape=[record_count, 64],
                dtype="uint8",
                inner_shape=[256, 64],
                outer_shape=[16_384, 64],
                chunks_per_shard=64,
                dimension_names=None,
            ),
            "zarr_packed_index_array",
        )

        actual_pixel_shards = 0
        actual_validity_shards = 0
        present_pixel_chunks = 0
        present_validity_chunks = 0
        for t in range(case.t):
            for physical_c in range(case.c):
                logical_c = case.logical_for_physical[physical_c]
                for outer_z in range(outer_grid_z):
                    for outer_y in range(outer_grid_y):
                        for outer_x in range(outer_grid_x):
                            pixel_slots: list[bytes | None] = [None] * chunks_per_shard
                            validity_slots_encoded: list[bytes | None] | None = (
                                [None] * chunks_per_shard if case.validity == "explicit" else None
                            )
                            for local_z in range(4 if not case.is_2d else 1):
                                z_chunk = outer_z * (4 if not case.is_2d else 1) + local_z
                                if z_chunk >= grid_z:
                                    continue
                                for local_y in range(4):
                                    y_chunk = outer_y * 4 + local_y
                                    if y_chunk >= grid_y:
                                        continue
                                    for local_x in range(4):
                                        x_chunk = outer_x * 4 + local_x
                                        if x_chunk >= grid_x:
                                            continue
                                        pixel_decoded, validity_decoded, facts = build_decoded_brick(
                                            case,
                                            level=level,
                                            logical_c=logical_c,
                                            t=t,
                                            z_chunk=z_chunk,
                                            y_chunk=y_chunk,
                                            x_chunk=x_chunk,
                                            inner_z=inner_z,
                                            inner_y=inner_y,
                                            inner_x=inner_x,
                                        )
                                        slot = (
                                            (local_z * 4 + local_y) * 4 + local_x
                                            if not case.is_2d
                                            else local_y * 4 + local_x
                                        )
                                        if pixel_decoded is not None:
                                            pixel_slots[slot] = encode_inner(pixel_decoded)
                                            present_pixel_chunks += 1
                                        if validity_decoded is not None:
                                            require(validity_slots_encoded is not None, "validity slot state")
                                            validity_slots_encoded[slot] = encode_inner(validity_decoded)
                                            present_validity_chunks += 1
                                        ordinal = (
                                            (((t * case.c + physical_c) * grid_z + z_chunk) * grid_y + y_chunk)
                                            * grid_x
                                            + x_chunk
                                        )
                                        records[ordinal] = packed_record(
                                            scale=level,
                                            t=t,
                                            physical_c=physical_c,
                                            z_chunk=z_chunk,
                                            y_chunk=y_chunk,
                                            x_chunk=x_chunk,
                                            capacity=facts["capacity"],
                                            valid_count=facts["valid_count"],
                                            nonfill_count=facts["nonfill_count"],
                                            minimum_bits=facts["minimum_bits"],
                                            maximum_bits=facts["maximum_bits"],
                                            pixel_present=facts["pixel_present"],
                                            explicit_validity=case.validity == "explicit",
                                        )
                            pixel_shard = encode_shard(pixel_slots)
                            coordinates = f"{t}/{physical_c}/{outer_z}/{outer_y}/{outer_x}"
                            if pixel_shard is not None:
                                self.add(f"{pixel_base}/c/{coordinates}", pixel_shard, "pixel_shard")
                                actual_pixel_shards += 1
                            if validity_slots_encoded is not None:
                                validity_shard = encode_shard(validity_slots_encoded)
                                if validity_shard is not None:
                                    self.add(
                                        f"{validity_base}/c/{coordinates}",
                                        validity_shard,
                                        "validity_shard",
                                    )
                                    actual_validity_shards += 1

        require(all(record is not None for record in records), "packed record coverage is incomplete")
        record_bytes = b"".join(record for record in records if record is not None)
        packed_chunks = []
        for offset in range(0, len(records), 256):
            chunk = bytearray(16_384)
            part = record_bytes[offset * 64 : min(offset + 256, len(records)) * 64]
            chunk[: len(part)] = part
            packed_chunks.append(encode_inner(bytes(chunk)))
        packed_outer_count = ceil_div(len(packed_chunks), 64)
        for outer_ordinal in range(packed_outer_count):
            slots: list[bytes | None] = [None] * 64
            for local in range(64):
                chunk_ordinal = outer_ordinal * 64 + local
                if chunk_ordinal < len(packed_chunks):
                    slots[local] = packed_chunks[chunk_ordinal]
            shard = encode_shard(slots)
            require(shard is not None, "packed-index shard cannot be empty")
            self.add(f"{index_base}/c/{outer_ordinal}/0", shard, "packed_index_shard")

        self.operational_levels.append(
            {
                "scale": level,
                "record_count": record_count,
                "addressed_pixel_shards": case.t
                * case.c
                * outer_grid_z
                * outer_grid_y
                * outer_grid_x,
                "actual_pixel_shards": actual_pixel_shards,
                "addressed_validity_shards": (
                    case.t * case.c * outer_grid_z * outer_grid_y * outer_grid_x
                    if case.validity == "explicit"
                    else 0
                ),
                "actual_validity_shards": actual_validity_shards,
                "packed_index_shards": packed_outer_count,
                "present_pixel_chunks": present_pixel_chunks,
                "present_validity_chunks": present_validity_chunks,
            }
        )


def validate_package_path(value: str) -> None:
    require(value.isascii(), "package paths must be ASCII")
    path = PurePosixPath(value)
    require(
        value != ""
        and not path.is_absolute()
        and ".." not in path.parts
        and "." not in path.parts
        and "\\" not in value
        and len(value.encode("ascii")) <= PATH_BYTES_MAX,
        f"unsafe package path {value!r}",
    )


def pack_manifest_pages(descriptors: list[dict[str, str]]) -> list[list[dict[str, str]]]:
    require(descriptors, "manifest cannot be empty")
    require(
        all(descriptors[index]["path"] < descriptors[index + 1]["path"] for index in range(len(descriptors) - 1)),
        "manifest paths must be strictly sorted",
    )
    empty_size = len(
        canonical_json({"schema": "m4d-manifest-page", "schema_version": 1, "entries": []})
    )
    pages: list[list[dict[str, str]]] = []
    current: list[dict[str, str]] = []
    current_size = empty_size
    for descriptor in descriptors:
        descriptor_size = len(canonical_json(descriptor))
        candidate = current_size + (1 if current else 0) + descriptor_size
        if candidate <= MANIFEST_PAGE_BYTES_MAX:
            current.append(descriptor)
            current_size = candidate
            continue
        require(current, "one manifest descriptor exceeds the page bound")
        pages.append(current)
        current = [descriptor]
        current_size = empty_size + descriptor_size
    if current:
        pages.append(current)
    require(len(pages) <= 6, "manifest requires too many pages")
    return pages


def package_metrics(files: dict[str, bytes]) -> dict[str, Any]:
    require(files, "package is empty")
    directories: set[str] = set()
    for path in files:
        validate_package_path(path)
        parent = PurePosixPath(path).parent
        while parent != PurePosixPath("."):
            directories.add(parent.as_posix())
            parent = parent.parent
    child_counts: dict[str, int] = {}
    for path in [*directories, *files.keys()]:
        parent = PurePosixPath(path).parent.as_posix()
        if parent == ".":
            parent = ""
        child_counts[parent] = child_counts.get(parent, 0) + 1
    metrics = {
        "files": len(files),
        "directories": len(directories),
        "regular_file_bytes": sum(len(data) for data in files.values()),
        "max_depth": max(len(PurePosixPath(path).parent.parts) for path in files),
        "max_fan_out": max(child_counts.values()),
        "max_path_bytes": max(len(path.encode("ascii")) for path in [*directories, *files.keys()]),
        "max_file_bytes": max(len(data) for data in files.values()),
    }
    require(metrics["files"] <= FILE_COUNT_MAX, "package exceeds archive file ceiling")
    require(metrics["directories"] <= DIRECTORY_COUNT_MAX, "package exceeds directory ceiling")
    require(
        metrics["regular_file_bytes"] <= COMBINED_REGULAR_FILE_BYTES_MAX,
        "package exceeds the combined regular-file byte ceiling",
    )
    require(metrics["max_depth"] <= DIRECTORY_DEPTH_MAX, "package depth ceiling exceeded")
    require(metrics["max_fan_out"] <= FAN_OUT_MAX, "package fan-out ceiling exceeded")
    require(metrics["max_path_bytes"] <= PATH_BYTES_MAX, "package path ceiling exceeded")
    require(metrics["max_file_bytes"] <= INDIVIDUAL_FILE_BYTES_MAX, "package file ceiling exceeded")
    metrics["directory_paths"] = sorted(directories)
    return metrics


def archive_bytes(files: dict[str, bytes], metrics: dict[str, Any]) -> bytes:
    destination = io.BytesIO()
    with tarfile.open(fileobj=destination, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for path in metrics["directory_paths"]:
            member = tarfile.TarInfo(f"{path}/")
            member.type = tarfile.DIRTYPE
            member.mode = 0o755
            member.uid = 0
            member.gid = 0
            member.uname = ""
            member.gname = ""
            member.mtime = 0
            member.size = 0
            archive.addfile(member)
        for path, data in sorted(files.items()):
            member = tarfile.TarInfo(path)
            member.type = tarfile.REGTYPE
            member.mode = 0o644
            member.uid = 0
            member.gid = 0
            member.uname = ""
            member.gname = ""
            member.mtime = 0
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    encoded = destination.getvalue()
    require(0 < len(encoded) <= ARCHIVE_BYTES_MAX, "archive byte ceiling exceeded")
    require(
        metrics["regular_file_bytes"] <= len(encoded) * COMPRESSION_RATIO_MAX,
        "archive compression-ratio ceiling exceeded",
    )
    require(len(encoded) % 512 == 0 and encoded[257:263] == b"ustar\0", "archive is not USTAR")
    return encoded


def python_tool_facts() -> dict[str, str]:
    executable = Path(sys.executable).resolve()
    return {
        "implementation": sys.implementation.name,
        "version": ".".join(map(str, sys.version_info[:3])),
        "executable": str(executable),
        "sha256": sha256_file(executable),
    }


def produce(spec_path: Path, facts_path: Path, output: Path) -> dict[str, Any]:
    CANDIDATE_ROOT.mkdir(parents=True, exist_ok=True)
    candidate_root = CANDIDATE_ROOT.resolve(strict=True)
    output = output.resolve(strict=False)
    require(
        output != candidate_root and candidate_root in output.parents,
        "producer output must be below the repository candidate root",
    )
    require(not output.exists(), "producer output must be a fresh absent directory")
    parent = output.parent
    parent.mkdir(parents=True, exist_ok=True)
    require(parent.is_dir() and not parent.is_symlink(), "producer output parent must be a directory")
    cases = load_specs(spec_path)
    identities = load_opaque_ids(facts_path)
    logical_voxel_bytes = sum(case.logical_voxel_bytes for case in cases)
    require(
        logical_voxel_bytes <= COMBINED_LOGICAL_VOXEL_BYTES_MAX,
        "corpus exceeds the combined logical-byte ceiling",
    )
    zstd = verify_zstd()
    output.mkdir(mode=0o755)
    try:
        archives = output / "archives"
        archives.mkdir(mode=0o755)
        rows = []
        combined_archive_bytes = 0
        combined_regular_file_bytes = 0
        for case in cases:
            files, package = PackageBuilder(case, identities[case.case_id]).build()
            encoded = archive_bytes(files, package["metrics"])
            archive_path = archives / f"{case.case_id}.tar"
            archive_path.write_bytes(encoded)
            archive_path.chmod(0o644)
            package["archive"] = {
                "path": archive_path.relative_to(output).as_posix(),
                "format": "ustar",
                "compression": "none",
                "bytes": len(encoded),
                "sha256": sha256(encoded),
            }
            package["metrics"].pop("directory_paths")
            combined_archive_bytes += len(encoded)
            combined_regular_file_bytes += package["metrics"]["regular_file_bytes"]
            rows.append(package)
        require(
            combined_archive_bytes <= COMBINED_ARCHIVE_BYTES_MAX,
            "corpus exceeds the combined archive-byte ceiling",
        )
        require(
            combined_regular_file_bytes <= COMBINED_REGULAR_FILE_BYTES_MAX,
            "corpus exceeds the combined regular-file byte ceiling",
        )
        report = {
            "schema": REPORT_SCHEMA,
            "schema_version": 1,
            "status": "operational-bytes-emitted",
            "authority": False,
            "producer_id": PRODUCER_ID,
            "source_sha256": sha256_file(Path(__file__)),
            "spec": {"name": spec_path.name, "sha256": sha256_file(spec_path)},
            "oracle_facts_input": {
                "name": facts_path.name,
                "sha256": sha256_file(facts_path),
                "use": "opaque ScientificContentId strings only",
            },
            "tools": {"python": python_tool_facts(), "zstd": zstd},
            "corpus_metrics": {
                "archives": len(rows),
                "archive_bytes": combined_archive_bytes,
                "regular_file_bytes": combined_regular_file_bytes,
                "logical_voxel_bytes": logical_voxel_bytes,
            },
            "cases": rows,
            "non_claims": [
                "not a scientific expected-fact or identity authority",
                "not an independent reader or conformance result",
                "not a target-fixture promotion",
                "not IO-3, product, import, performance, or stable-format evidence",
            ],
        }
        report_path = output / "producer-report.json"
        report_path.write_bytes(record_json(report))
        report_path.chmod(0o644)
        return report
    except BaseException:
        shutil.rmtree(output, ignore_errors=True)
        raise


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--spec", type=Path, required=True)
    parser.add_argument("--facts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    spec = arguments.spec.resolve(strict=True)
    facts = arguments.facts.resolve(strict=True)
    output = arguments.output.resolve(strict=False)
    report = produce(spec, facts, output)
    print(
        json.dumps(
            {
                "status": report["status"],
                "cases": [row["case_id"] for row in report["cases"]],
                "output": output.relative_to(REPOSITORY_ROOT).as_posix(),
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        ProducerError,
        ValueError,
        KeyError,
        json.JSONDecodeError,
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
    ) as error:
        print(f"target T1 byte producer failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
