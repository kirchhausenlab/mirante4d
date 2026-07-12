#!/usr/bin/env python3
"""Independent full-package observations through pinned zarr-python.

This reader is a separate implementation lineage. It consumes only extracted
package bytes and released Zarr behavior; it does not import Mirante, the
fixture producer, or the scientific fact oracle.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import stat
import struct
import sys
from typing import Any

import google_crc32c
import numpy as np
import zarr


SCHEMA = "mirante4d-target-t1-independent-reader-case"
READER_ID = "TGT-READER-001"
LOCK = Path(__file__).with_name("requirements-linux-x86_64-py312.lock")
MISSING = (1 << 64) - 1
MAX_FILES = 64
MAX_DIRECTORIES = 64
MAX_FILE_BYTES = 32 * 1024 * 1024
MAX_PIXEL_BYTES = 64 * 1024 * 1024
MAX_PATH_BYTES = 240

EXPECTED_REJECTIONS = {
    "axis-name-mismatch",
    "declared-object-digest-mismatch",
    "declared-object-length-mismatch",
    "inner-crc32c-mismatch",
    "missing-declared-object",
    "noncanonical-shard-index",
    "nonfinite-float",
    "shard-index-crc32c-mismatch",
    "transform-mismatch",
    "unexpected-object",
    "unsupported-codec",
    "unsupported-dtype",
    "unsupported-storage-layout",
}


class ReaderRejection(RuntimeError):
    def __init__(self, code: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.detail = detail


def reject(code: str, detail: str) -> None:
    raise ReaderRejection(code, detail)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65_536), b""):
            digest.update(block)
    return digest.hexdigest()


def json_bytes(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n").encode(
        "utf-8"
    )


def read_json(path: Path, *, detail: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReaderRejection("unsupported-storage-layout", detail) from error
    if not isinstance(value, dict):
        reject("unsupported-storage-layout", detail)
    return value


def checked_relative(value: Any) -> str:
    if not isinstance(value, str) or not value or not value.isascii():
        reject("unsupported-storage-layout", "package path is not canonical ASCII")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or "." in path.parts
        or ".." in path.parts
        or "\\" in value
        or len(value.encode("ascii")) > MAX_PATH_BYTES
    ):
        reject("unsupported-storage-layout", "package path escaped its root")
    return value


def package_path(root: Path, relative: str) -> Path:
    relative = checked_relative(relative)
    candidate = root.joinpath(*PurePosixPath(relative).parts)
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise ReaderRejection("missing-declared-object", "declared object is absent") from error
    if root not in resolved.parents:
        reject("unexpected-object", "package object escaped its root")
    return resolved


def digest_from_field(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value.startswith("sha256:")
        or len(value) != 71
        or any(character not in "0123456789abcdef" for character in value[7:])
    ):
        reject("unsupported-storage-layout", "manifest digest is malformed")
    return value[7:]


def positive_decimal(value: Any) -> int:
    if not isinstance(value, str) or not value.isascii() or not value.isdigit():
        reject("unsupported-storage-layout", "manifest length is malformed")
    result = int(value)
    if result < 0 or result > MAX_FILE_BYTES:
        reject("unsupported-storage-layout", "manifest length exceeds the reader bound")
    return result


def inventory(root: Path) -> tuple[list[str], list[str]]:
    files: list[str] = []
    directories: list[str] = []
    for current, directory_names, file_names in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        for name in sorted(directory_names):
            path = current_path / name
            metadata = path.lstat()
            if not stat.S_ISDIR(metadata.st_mode) or path.is_symlink():
                reject("unexpected-object", "package contains a non-directory ancestor")
            relative = path.relative_to(root).as_posix()
            checked_relative(relative)
            directories.append(relative)
        for name in sorted(file_names):
            path = current_path / name
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode) or path.is_symlink() or metadata.st_nlink != 1:
                reject("unexpected-object", "package contains a non-regular object")
            if metadata.st_size > MAX_FILE_BYTES:
                reject("unsupported-storage-layout", "package object exceeds the reader bound")
            relative = path.relative_to(root).as_posix()
            checked_relative(relative)
            files.append(relative)
    files.sort()
    directories.sort()
    if len(files) > MAX_FILES or len(directories) > MAX_DIRECTORIES:
        reject("unexpected-object", "package inventory exceeds the corpus bound")
    return files, directories


def validate_manifest(root: Path) -> dict[str, Any]:
    root_relative = "m4d/manifest/root.json"
    root_path = package_path(root, root_relative)
    root_bytes = root_path.read_bytes()
    root_document = read_json(root_path, detail="manifest root is invalid JSON")
    references = root_document.get("pages")
    if not isinstance(references, list) or not references:
        reject("unsupported-storage-layout", "manifest root has no pages")

    page_paths: list[str] = []
    descriptors: dict[str, dict[str, Any]] = {}
    for reference in references:
        if not isinstance(reference, dict):
            reject("unsupported-storage-layout", "manifest page reference is invalid")
        relative = checked_relative(reference.get("path"))
        page_paths.append(relative)
        page_path = package_path(root, relative)
        page_bytes = page_path.read_bytes()
        expected_bytes = positive_decimal(reference.get("bytes"))
        if len(page_bytes) != expected_bytes:
            reject("declared-object-length-mismatch", "manifest page length changed")
        if sha256(page_bytes) != digest_from_field(reference.get("digest")):
            reject("declared-object-digest-mismatch", "manifest page digest changed")
        page = read_json(page_path, detail="manifest page is invalid JSON")
        entries = page.get("entries")
        if not isinstance(entries, list) or not entries:
            reject("unsupported-storage-layout", "manifest page has no entries")
        for descriptor in entries:
            if not isinstance(descriptor, dict):
                reject("unsupported-storage-layout", "manifest descriptor is invalid")
            object_relative = checked_relative(descriptor.get("path"))
            if object_relative in descriptors:
                reject("unexpected-object", "manifest contains a duplicate object")
            descriptors[object_relative] = descriptor

    actual_files, directories = inventory(root)
    expected_files = sorted([root_relative, *page_paths, *descriptors])
    missing = sorted(set(expected_files) - set(actual_files))
    if missing:
        reject("missing-declared-object", "manifest-declared object is absent")
    extra = sorted(set(actual_files) - set(expected_files))
    if extra:
        reject("unexpected-object", "package contains an undeclared object")

    for relative, descriptor in sorted(descriptors.items()):
        path = package_path(root, relative)
        metadata = path.stat()
        expected_bytes = positive_decimal(descriptor.get("bytes"))
        if metadata.st_size != expected_bytes:
            reject("declared-object-length-mismatch", "declared object length changed")
        if sha256_file(path) != digest_from_field(descriptor.get("digest")):
            reject("declared-object-digest-mismatch", "declared object digest changed")

    root_digest = sha256(root_bytes)
    return {
        "directories": len(directories),
        "files": len(actual_files),
        "manifest_descriptors": len(descriptors),
        "manifest_pages": len(page_paths),
        "manifest_root_bytes": len(root_bytes),
        "manifest_root_sha256": root_digest,
        "observed_package_id": f"m4d-package-v1-sha256:{root_digest}",
    }


def require_list(value: Any, *, length: int | None = None) -> list[Any]:
    if not isinstance(value, list) or (length is not None and len(value) != length):
        reject("unsupported-storage-layout", "metadata array has an invalid shape")
    return value


def integer_list(value: Any, *, length: int | None = None) -> list[int]:
    values = require_list(value, length=length)
    if not all(isinstance(item, int) and not isinstance(item, bool) and item > 0 for item in values):
        reject("unsupported-storage-layout", "metadata dimensions are invalid")
    return values


def validate_codec(metadata: dict[str, Any], role: str) -> tuple[list[int], list[int]]:
    if metadata.get("zarr_format") != 3 or metadata.get("node_type") != "array":
        reject("unsupported-storage-layout", "array is not Zarr v3 metadata")
    if metadata.get("chunk_key_encoding") != {
        "name": "default",
        "configuration": {"separator": "/"},
    }:
        reject("unsupported-storage-layout", "chunk-key encoding is unsupported")
    dtype = metadata.get("data_type")
    if role == "pixel" and dtype not in {"uint8", "uint16", "float32"}:
        reject("unsupported-dtype", "pixel dtype is outside the frozen subset")
    if role in {"validity", "packed-index"} and dtype != "uint8":
        reject("unsupported-dtype", "auxiliary dtype is outside the frozen subset")

    codecs = require_list(metadata.get("codecs"), length=1)
    sharding = codecs[0]
    if not isinstance(sharding, dict) or sharding.get("name") != "sharding_indexed":
        reject("unsupported-codec", "outer codec is not indexed sharding")
    configuration = sharding.get("configuration")
    if not isinstance(configuration, dict):
        reject("unsupported-codec", "indexed-sharding configuration is absent")
    inner_codecs = configuration.get("codecs")
    index_codecs = configuration.get("index_codecs")
    expected_inner = [
        {"name": "bytes", "configuration": {"endian": "little"}},
        {"name": "zstd", "configuration": {"level": 3, "checksum": False}},
        {"name": "crc32c"},
    ]
    expected_index = [
        {"name": "bytes", "configuration": {"endian": "little"}},
        {"name": "crc32c"},
    ]
    if (
        inner_codecs != expected_inner
        or index_codecs != expected_index
        or configuration.get("index_location") != "end"
    ):
        reject("unsupported-codec", "codec pipeline is outside the frozen subset")

    chunk_grid = metadata.get("chunk_grid")
    if not isinstance(chunk_grid, dict) or chunk_grid.get("name") != "regular":
        reject("unsupported-storage-layout", "array does not use a regular outer grid")
    grid_configuration = chunk_grid.get("configuration")
    if not isinstance(grid_configuration, dict):
        reject("unsupported-storage-layout", "outer chunk shape is absent")
    outer = integer_list(grid_configuration.get("chunk_shape"))
    inner = integer_list(configuration.get("chunk_shape"), length=len(outer))

    shape = integer_list(metadata.get("shape"), length=len(outer))
    if role == "pixel":
        expected = (
            ([1, 1, 1, 256, 256], [1, 1, 1, 1024, 1024])
            if shape[2] == 1
            else ([1, 1, 64, 64, 64], [1, 1, 256, 256, 256])
        )
    elif role == "validity":
        expected = ([1, 1, 64, 64, 8], [1, 1, 256, 256, 32])
        if metadata.get("dimension_names") != ["t", "c", "z", "y", "x_byte"]:
            reject("unsupported-storage-layout", "validity dimension names are unsupported")
    else:
        expected = ([256, 64], [16_384, 64])
        if shape[1] != 64 or metadata.get("dimension_names") is not None:
            reject("unsupported-storage-layout", "packed-index dimensions are unsupported")
    if inner != expected[0] or outer != expected[1]:
        reject("unsupported-storage-layout", "chunk or shard geometry is unsupported")
    return inner, outer


def crc32c(data: bytes) -> int:
    return google_crc32c.value(data)


def validate_shards(
    array_root: Path, metadata: dict[str, Any], inner: list[int], outer: list[int]
) -> list[dict[str, Any]]:
    ratios = []
    for outer_dimension, inner_dimension in zip(outer, inner, strict=True):
        if outer_dimension % inner_dimension != 0:
            reject("unsupported-storage-layout", "outer shape is not divisible by inner shape")
        ratios.append(outer_dimension // inner_dimension)
    slots = math.prod(ratios)
    index_bytes = slots * 16
    trailer_bytes = index_bytes + 4
    shard_root = array_root / "c"
    if not shard_root.exists():
        return []
    result = []
    for path in sorted(candidate for candidate in shard_root.rglob("*") if candidate.is_file()):
        relative = path.relative_to(array_root).as_posix()
        data = path.read_bytes()
        if len(data) < trailer_bytes:
            reject("shard-index-crc32c-mismatch", "shard is shorter than its end index")
        payload = data[:-trailer_bytes]
        index = data[-trailer_bytes:-4]
        stored_index_crc = struct.unpack("<I", data[-4:])[0]
        if crc32c(index) != stored_index_crc:
            reject("shard-index-crc32c-mismatch", "shard end-index CRC32C changed")
        cursor = 0
        present = 0
        for slot in range(slots):
            offset, length = struct.unpack_from("<QQ", index, slot * 16)
            offset_missing = offset == MISSING
            length_missing = length == MISSING
            if offset_missing or length_missing:
                if not (offset_missing and length_missing):
                    reject("noncanonical-shard-index", "missing shard slot is not canonical")
                continue
            if length < 4 or offset != cursor or offset + length > len(payload):
                reject("noncanonical-shard-index", "present shard slots are not tightly packed")
            encoded = payload[offset : offset + length]
            stored_inner_crc = struct.unpack("<I", encoded[-4:])[0]
            if crc32c(encoded[:-4]) != stored_inner_crc:
                reject("inner-crc32c-mismatch", "inner payload CRC32C changed")
            cursor += length
            present += 1
        if cursor != len(payload):
            reject("noncanonical-shard-index", "shard payload has unindexed bytes")
        result.append(
            {
                "bytes": len(data),
                "key": relative,
                "present_inner_chunks": present,
                "sha256": sha256(data),
            }
        )
    return result


def open_array(array_root: Path) -> zarr.Array:
    try:
        return zarr.open_array(array_root, mode="r")
    except Exception as error:
        raise ReaderRejection("unsupported-storage-layout", "zarr-python could not open the array") from error


def read_array(array: zarr.Array) -> np.ndarray[Any, Any]:
    try:
        value = np.asarray(array[:])
    except Exception as error:
        raise ReaderRejection("unsupported-storage-layout", "zarr-python could not read the array") from error
    if value.nbytes > MAX_PIXEL_BYTES:
        reject("unsupported-storage-layout", "decoded array exceeds the reader bound")
    return np.ascontiguousarray(value)


def little_endian_bytes(value: np.ndarray[Any, Any]) -> bytes:
    dtype = value.dtype
    target = dtype if dtype.itemsize == 1 else dtype.newbyteorder("<")
    return np.ascontiguousarray(value.astype(target, copy=False)).tobytes(order="C")


def encoded_scalar(value: np.generic[Any], dtype: np.dtype[Any]) -> dict[str, Any]:
    if dtype == np.dtype("float32"):
        bits = np.asarray([value], dtype=np.dtype("<f4")).view(np.dtype("<u4"))[0]
        return {"f32_bits": f"{int(bits):08x}"}
    return {"integer": int(value)}


def sample_points(value: np.ndarray[Any, Any]) -> list[dict[str, Any]]:
    flat = value.reshape(-1)
    indexes = sorted({0, 1, len(flat) // 4, len(flat) // 2, 3 * len(flat) // 4, len(flat) - 2, len(flat) - 1})
    result = []
    for index in indexes:
        coordinate = [int(part) for part in np.unravel_index(index, value.shape)]
        result.append(
            {
                "coordinate_tczyx": coordinate,
                "value": encoded_scalar(flat[index], value.dtype),
            }
        )
    return result


def basic_array_observation(
    array: zarr.Array,
    value: np.ndarray[Any, Any],
    metadata: dict[str, Any],
    shards: list[dict[str, Any]],
) -> dict[str, Any]:
    return {
        "chunks": list(array.chunks),
        "decoded_bytes": value.nbytes,
        "dimension_names": list(array.metadata.dimension_names)
        if array.metadata.dimension_names is not None
        else None,
        "dtype": str(value.dtype),
        "fill_value": metadata.get("fill_value"),
        "full_array_c_order_le_sha256": sha256(little_endian_bytes(value)),
        "read_elements": value.size,
        "shape": list(value.shape),
        "shards": list(array.shards) if array.shards is not None else None,
        "stored_shards": shards,
    }


def decode_f64_bits(value: str) -> float:
    if not isinstance(value, str) or len(value) != 16:
        reject("transform-mismatch", "scientific transform bit string is malformed")
    try:
        decoded = struct.unpack(">d", bytes.fromhex(value))[0]
    except (ValueError, struct.error) as error:
        raise ReaderRejection("transform-mismatch", "scientific transform bits are malformed") from error
    if not math.isfinite(decoded):
        reject("transform-mismatch", "scientific transform is non-finite")
    return decoded


def f64_bits(value: Any) -> str:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        reject("transform-mismatch", "OME transform value is not numeric")
    decoded = float(value)
    if not math.isfinite(decoded):
        reject("transform-mismatch", "OME transform value is non-finite")
    if decoded == 0.0:
        decoded = 0.0
    return struct.pack(">d", decoded).hex()


def observe_transforms(
    profile: dict[str, Any], science: dict[str, Any], ome: dict[str, Any]
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    attributes = ome.get("attributes")
    if not isinstance(attributes, dict) or not isinstance(attributes.get("ome"), dict):
        reject("transform-mismatch", "OME image attributes are absent")
    multiscales = require_list(attributes["ome"].get("multiscales"), length=1)
    multiscale = multiscales[0]
    if not isinstance(multiscale, dict):
        reject("transform-mismatch", "OME multiscale is malformed")
    axes = require_list(multiscale.get("axes"), length=5)
    axis_names = []
    for axis in axes:
        if not isinstance(axis, dict) or not isinstance(axis.get("name"), str):
            reject("axis-name-mismatch", "OME axis is malformed")
        axis_names.append(axis["name"])
    if axis_names != ["t", "c", "z", "y", "x"]:
        reject("axis-name-mismatch", "OME axes do not use t,c,z,y,x order")

    layers = require_list(science.get("layers"))
    if not layers or not isinstance(layers[0], dict):
        reject("transform-mismatch", "scientific layers are absent")
    first_layer = layers[0]
    temporal = first_layer.get("temporal_calibration")
    if not isinstance(temporal, dict):
        reject("transform-mismatch", "temporal calibration is absent")
    temporal_step = decode_f64_bits(temporal.get("step_seconds_f64_bits"))
    grid_bits = require_list(first_layer.get("grid_to_world_micrometer_f64_bits"), length=16)
    grid = [decode_f64_bits(value) for value in grid_bits]
    for layer in layers[1:]:
        if not isinstance(layer, dict) or layer.get("temporal_calibration") != temporal or layer.get("grid_to_world_micrometer_f64_bits") != grid_bits:
            reject("transform-mismatch", "scientific layer transforms disagree")

    images = require_list(profile.get("images"), length=1)
    if not isinstance(images[0], dict):
        reject("transform-mismatch", "profile image is malformed")
    levels = require_list(images[0].get("levels"))
    datasets = require_list(multiscale.get("datasets"), length=len(levels))
    io_base = profile.get("ome_interoperability_base")
    observed_datasets = []
    for ordinal, (level, dataset) in enumerate(zip(levels, datasets, strict=True)):
        if not isinstance(level, dict) or not isinstance(dataset, dict):
            reject("transform-mismatch", "OME dataset is malformed")
        if dataset.get("path") != f"s{ordinal:02d}" or level.get("pixel_path") != f"images/i00000000/s{ordinal:02d}":
            reject("transform-mismatch", "OME and profile level paths disagree")
        transformations = require_list(dataset.get("coordinateTransformations"))
        normalized = []
        scale_values: list[Any] | None = None
        translation_values: list[Any] | None = None
        for transformation in transformations:
            if not isinstance(transformation, dict):
                reject("transform-mismatch", "OME transformation is malformed")
            kind = transformation.get("type")
            if kind == "scale":
                scale_values = require_list(transformation.get("scale"), length=5)
                normalized.append({"type": "scale", "f64_bits": [f64_bits(value) for value in scale_values]})
            elif kind == "translation":
                translation_values = require_list(transformation.get("translation"), length=5)
                normalized.append({"type": "translation", "f64_bits": [f64_bits(value) for value in translation_values]})
            else:
                reject("transform-mismatch", "OME transformation type is unsupported")
        if scale_values is None:
            reject("transform-mismatch", "OME scale is absent")
        if io_base == "IO-2":
            off_diagonal = [grid[1], grid[2], grid[4], grid[6], grid[8], grid[9]]
            if any(value != 0.0 for value in off_diagonal) or grid[15] != 1.0:
                reject("transform-mismatch", "IO-2 scientific transform is not diagonal affine")
            factor = 1 << ordinal
            expected_scale = [temporal_step, 1.0, grid[10] * factor, grid[5] * factor, grid[0] * factor]
            expected_translation = [0.0, 0.0, grid[11], grid[7], grid[3]]
        elif io_base == "IO-1":
            expected_scale = [temporal_step, 1.0, 1.0, 1.0, 1.0]
            expected_translation = [0.0] * 5
        else:
            reject("transform-mismatch", "OME interoperability classification is unsupported")
        observed_scale_bits = [f64_bits(value) for value in scale_values]
        expected_scale_bits = [f64_bits(value) for value in expected_scale]
        observed_translation = translation_values if translation_values is not None else [0.0] * 5
        if observed_scale_bits != expected_scale_bits or [f64_bits(value) for value in observed_translation] != [f64_bits(value) for value in expected_translation]:
            reject("transform-mismatch", "OME transform contradicts scientific metadata")
        observed_datasets.append({"path": dataset["path"], "transformations": normalized})
    return axes, observed_datasets


def logical_mapping(profile: dict[str, Any]) -> list[int]:
    images = require_list(profile.get("images"), length=1)
    if not isinstance(images[0], dict):
        reject("unsupported-storage-layout", "profile image is malformed")
    rows = require_list(images[0].get("logical_layers"))
    mapping: dict[int, int] = {}
    for row in rows:
        if not isinstance(row, dict):
            reject("unsupported-storage-layout", "logical layer mapping is malformed")
        try:
            logical = int(row.get("logical_layer_ordinal"))
            physical = int(row.get("physical_channel"))
        except (TypeError, ValueError) as error:
            raise ReaderRejection("unsupported-storage-layout", "logical layer mapping is malformed") from error
        if logical in mapping:
            reject("unsupported-storage-layout", "logical layer mapping is duplicated")
        mapping[logical] = physical
    result = [mapping[index] for index in range(len(mapping))] if set(mapping) == set(range(len(mapping))) else []
    if not result or sorted(result) != list(range(len(result))):
        reject("unsupported-storage-layout", "physical channel mapping is not a permutation")
    return result


def observe_case(case_id: str, package: Path) -> dict[str, Any]:
    if zarr.__version__ != "3.2.1" or np.__version__ != "2.5.1":
        reject("unsupported-storage-layout", "reader environment does not match its lock")
    if not case_id or not case_id.isascii():
        reject("unsupported-storage-layout", "case id is not canonical ASCII")
    try:
        root = package.resolve(strict=True)
    except OSError as error:
        raise ReaderRejection("missing-declared-object", "package root is absent") from error
    if not root.is_dir() or root.is_symlink():
        reject("unexpected-object", "package root is not a plain directory")

    package_observation = validate_manifest(root)
    profile = read_json(package_path(root, "m4d/profile.json"), detail="profile is invalid JSON")
    science = read_json(package_path(root, "m4d/science.json"), detail="science is invalid JSON")
    ome = read_json(package_path(root, "images/i00000000/zarr.json"), detail="OME image metadata is invalid JSON")
    axes, ome_datasets = observe_transforms(profile, science, ome)
    physical_for_logical = logical_mapping(profile)

    profile_scientific_id = profile.get("scientific_content_id")
    science_scientific_id = science.get("scientific_content_id")
    if not isinstance(profile_scientific_id, str) or profile_scientific_id != science_scientific_id:
        reject("unsupported-storage-layout", "declared scientific identities disagree")

    images = require_list(profile.get("images"), length=1)
    image = images[0]
    assert isinstance(image, dict)
    levels = require_list(image.get("levels"))
    level_observations = []
    total_pixel_bytes = 0
    for ordinal, level in enumerate(levels):
        if not isinstance(level, dict) or level.get("scale_ordinal") != str(ordinal):
            reject("unsupported-storage-layout", "profile level ordinal is invalid")
        pixel_relative = checked_relative(level.get("pixel_path"))
        pixel_root = package_path(root, pixel_relative)
        pixel_metadata = read_json(pixel_root / "zarr.json", detail="pixel metadata is invalid JSON")
        pixel_inner, pixel_outer = validate_codec(pixel_metadata, "pixel")
        pixel_dimension_names = pixel_metadata.get("dimension_names")
        axis_names = [axis.get("name") for axis in axes]
        if pixel_dimension_names != axis_names:
            reject("axis-name-mismatch", "pixel dimension names contradict OME axes")
        pixel_shards = validate_shards(pixel_root, pixel_metadata, pixel_inner, pixel_outer)
        pixel_array = open_array(pixel_root)
        physical_pixels = read_array(pixel_array)
        total_pixel_bytes += physical_pixels.nbytes
        if total_pixel_bytes > MAX_PIXEL_BYTES:
            reject("unsupported-storage-layout", "case pixel bytes exceed the reader bound")
        if physical_pixels.ndim != 5 or physical_pixels.shape[1] != len(physical_for_logical):
            reject("unsupported-storage-layout", "pixel shape contradicts channel mapping")
        if physical_pixels.dtype == np.dtype("float32") and not bool(np.isfinite(physical_pixels).all()):
            reject("nonfinite-float", "float pixel array contains a non-finite value")
        logical_pixels = np.ascontiguousarray(physical_pixels[:, physical_for_logical, ...])

        validity_mode = level.get("validity_mode")
        validity_array_observation: dict[str, Any] | None = None
        if validity_mode == "all_valid":
            logical_validity = np.ones(logical_pixels.shape, dtype=np.uint8)
            physical_validity = np.ones(physical_pixels.shape, dtype=np.uint8)
        elif validity_mode == "explicit":
            validity_relative = checked_relative(level.get("validity_path"))
            validity_root = package_path(root, validity_relative)
            validity_metadata = read_json(validity_root / "zarr.json", detail="validity metadata is invalid JSON")
            validity_inner, validity_outer = validate_codec(validity_metadata, "validity")
            validity_shards = validate_shards(validity_root, validity_metadata, validity_inner, validity_outer)
            validity_array = open_array(validity_root)
            packed_validity = read_array(validity_array)
            expected_prefix = physical_pixels.shape[:-1]
            if packed_validity.shape[:-1] != expected_prefix or packed_validity.shape[-1] != math.ceil(physical_pixels.shape[-1] / 8):
                reject("unsupported-storage-layout", "validity shape contradicts pixels")
            unpacked = np.unpackbits(packed_validity, axis=-1, bitorder="little")
            if bool(unpacked[..., physical_pixels.shape[-1] :].any()):
                reject("unsupported-storage-layout", "validity padding bits are nonzero")
            physical_validity = np.ascontiguousarray(unpacked[..., : physical_pixels.shape[-1]])
            logical_validity = np.ascontiguousarray(physical_validity[:, physical_for_logical, ...])
            validity_array_observation = basic_array_observation(
                validity_array, packed_validity, validity_metadata, validity_shards
            )
        else:
            reject("unsupported-storage-layout", "validity mode is unsupported")

        canonical = logical_pixels.copy()
        canonical[logical_validity == 0] = 0
        packed_index_relative = checked_relative(level.get("packed_index_path"))
        packed_index_root = package_path(root, packed_index_relative)
        packed_index_metadata = read_json(packed_index_root / "zarr.json", detail="packed-index metadata is invalid JSON")
        packed_inner, packed_outer = validate_codec(packed_index_metadata, "packed-index")
        packed_shards = validate_shards(packed_index_root, packed_index_metadata, packed_inner, packed_outer)
        packed_array = open_array(packed_index_root)
        packed_values = read_array(packed_array)

        layer_observations = []
        packed_validity_layers: list[bytes] = []
        for logical, physical in enumerate(physical_for_logical):
            layer_pixels = logical_pixels[:, logical, ...]
            layer_validity = logical_validity[:, logical, ...]
            layer_canonical = canonical[:, logical, ...]
            packed_layer_validity = np.packbits(
                layer_validity.reshape(-1), bitorder="little"
            ).tobytes()
            packed_validity_layers.append(packed_layer_validity)
            layer_observations.append(
                {
                    "canonical_values_c_order_le_sha256": sha256(little_endian_bytes(layer_canonical)),
                    "logical_layer": logical,
                    "physical_channel": physical,
                    "raw_values_c_order_le_sha256": sha256(little_endian_bytes(layer_pixels)),
                    "valid_count": int(layer_validity.sum(dtype=np.uint64)),
                    "validity_packed_lsb0_sha256": sha256(packed_layer_validity),
                    "validity_u8_c_order_sha256": sha256(layer_validity.tobytes(order="C")),
                }
            )

        layer_major_pixels = np.ascontiguousarray(
            logical_pixels.transpose(1, 0, 2, 3, 4)
        )
        layer_major_canonical = np.ascontiguousarray(
            canonical.transpose(1, 0, 2, 3, 4)
        )
        pixel_observation = basic_array_observation(
            pixel_array, physical_pixels, pixel_metadata, pixel_shards
        )
        pixel_observation.update(
            {
                "canonical_logical_c_order_le_sha256": sha256(little_endian_bytes(canonical)),
                "canonical_logical_layer_major_ctzyx_le_sha256": sha256(
                    little_endian_bytes(layer_major_canonical)
                ),
                "logical_c_order_le_sha256": sha256(little_endian_bytes(logical_pixels)),
                "logical_layer_major_ctzyx_le_sha256": sha256(
                    little_endian_bytes(layer_major_pixels)
                ),
                "logical_sample_points": sample_points(logical_pixels),
                "physical_c_order_le_sha256": sha256(little_endian_bytes(physical_pixels)),
            }
        )
        if logical_pixels.dtype == np.dtype("float32"):
            bits = logical_pixels.view(np.dtype("<u4")).reshape(-1)
            unique, counts = np.unique(bits, return_counts=True)
            pixel_observation["f32_bit_pattern_counts"] = [
                {"count": int(count), "f32_bits": f"{int(value):08x}"}
                for value, count in zip(unique, counts, strict=True)
            ]
        else:
            pixel_observation["integer_min"] = int(logical_pixels.min())
            pixel_observation["integer_max"] = int(logical_pixels.max())
            pixel_observation["integer_sum"] = int(logical_pixels.sum(dtype=np.uint64))

        level_observations.append(
            {
                "layers": layer_observations,
                "ordinal": ordinal,
                "packed_index": basic_array_observation(
                    packed_array, packed_values, packed_index_metadata, packed_shards
                ),
                "pixel": pixel_observation,
                "validity": {
                    "array": validity_array_observation,
                    "false_count": int(logical_validity.size - logical_validity.sum(dtype=np.uint64)),
                    "logical_layer_packed_lsb0_sha256": sha256(
                        b"".join(packed_validity_layers)
                    ),
                    "logical_tczyx_packed_lsb0_sha256": sha256(
                        np.packbits(
                            logical_validity.reshape(-1), bitorder="little"
                        ).tobytes()
                    ),
                    "logical_u8_c_order_sha256": sha256(logical_validity.tobytes(order="C")),
                    "mode": validity_mode,
                    "physical_u8_c_order_sha256": sha256(physical_validity.tobytes(order="C")),
                    "shape_tczyx": list(logical_validity.shape),
                    "true_count": int(logical_validity.sum(dtype=np.uint64)),
                },
            }
        )

    science_layers = require_list(science.get("layers"), length=len(physical_for_logical))
    science_observation = []
    for ordinal, layer in enumerate(science_layers):
        if not isinstance(layer, dict) or layer.get("logical_layer_ordinal") != str(ordinal):
            reject("unsupported-storage-layout", "science layer order is invalid")
        science_observation.append(
            {
                "base_shape_tzyx": layer.get("base_shape_tzyx"),
                "dtype": layer.get("dtype"),
                "grid_to_world_micrometer_f64_bits": layer.get("grid_to_world_micrometer_f64_bits"),
                "logical_layer_ordinal": ordinal,
                "temporal_step_f64_bits": layer.get("temporal_calibration", {}).get("step_seconds_f64_bits")
                if isinstance(layer.get("temporal_calibration"), dict)
                else None,
            }
        )
    base_pixel = level_observations[0]["pixel"]
    base_shape = base_pixel["shape"]
    for layer in science_observation:
        expected_shape = [str(base_shape[0]), str(base_shape[2]), str(base_shape[3]), str(base_shape[4])]
        if layer["base_shape_tzyx"] != expected_shape or layer["dtype"] != base_pixel["dtype"]:
            reject("unsupported-storage-layout", "science layer shape or dtype contradicts pixels")

    return {
        "case_id": case_id,
        "environment": {
            "lock_sha256": sha256_file(LOCK),
            "numpy_version": np.__version__,
            "python_version": ".".join(map(str, sys.version_info[:3])),
            "zarr_version": zarr.__version__,
        },
        "image": {
            "axes": axes,
            "levels": level_observations,
            "logical_to_physical_channels": physical_for_logical,
            "ome_datasets": ome_datasets,
            "science_layers": science_observation,
        },
        "package": {
            **package_observation,
            "compatibility": profile.get("compatibility"),
            "declared_scientific_content_id": profile_scientific_id,
            "ome_interoperability_base": profile.get("ome_interoperability_base"),
            "required_capabilities": profile.get("required_capabilities"),
        },
        "reader_id": READER_ID,
        "reader_source_sha256": sha256_file(Path(__file__)),
        "result": "passed",
        "schema": SCHEMA,
        "schema_version": 1,
        "status": "passed",
    }


def write_report(path: Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_bytes(json_bytes(report))
    os.replace(temporary, path)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--case-id", required=True)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--expect-rejection", choices=sorted(EXPECTED_REJECTIONS))
    arguments = parser.parse_args()
    if arguments.expect_rejection is None and arguments.report is None:
        parser.error("--report is required for a positive case")

    try:
        report = observe_case(arguments.case_id, arguments.package)
    except ReaderRejection as error:
        if arguments.expect_rejection != error.code:
            expected = arguments.expect_rejection or "a positive observation"
            print(
                f"independent reader failed: expected {expected}, observed {error.code}",
                file=sys.stderr,
            )
            raise SystemExit(1) from error
        rejection_report = {
            "case_id": arguments.case_id,
            "expected_rejection": arguments.expect_rejection,
            "observed_rejection": error.code,
            "reader_id": READER_ID,
            "result": "passed",
            "schema": "mirante4d-target-t1-independent-reader-rejection",
            "schema_version": 1,
            "status": "expected-rejection",
        }
        if arguments.report is not None:
            write_report(arguments.report, rejection_report)
        print(
            json.dumps(
                {
                    "case_id": arguments.case_id,
                    "observed_rejection": error.code,
                    "result": "passed",
                },
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return

    if arguments.expect_rejection is not None:
        print(
            f"independent reader failed: expected {arguments.expect_rejection}, observed acceptance",
            file=sys.stderr,
        )
        raise SystemExit(1)
    assert arguments.report is not None
    write_report(arguments.report, report)
    print(
        json.dumps(
            {"case_id": arguments.case_id, "result": "passed", "status": "observed"},
            sort_keys=True,
            separators=(",", ":"),
        )
    )


if __name__ == "__main__":
    main()
