#!/usr/bin/env python3
"""Dependency-isolated read-only validator for the WP-10B project fixture."""

from __future__ import annotations

import argparse
import copy
import gzip
import hashlib
import io
import json
import math
from pathlib import Path, PurePosixPath
import re
import struct
import sys
import tarfile
import uuid
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
PAGE_BYTES = 16 * 1024 * 1024
GENERATION_PREFIX = "m4d-project-generation-v1-sha256:"
GENERATION_DOMAIN = b"M4D-PROJECT-GENERATION-V1\0"
REF_DOMAIN = b"M4D-PROJECT-REF-V1\0"
REF_MAGIC = b"M4DREF1\0"
GENERATION_PATH = re.compile(r"generations/sha256/([0-9a-f]{2})/([0-9a-f]{62})\.json\Z")
OBJECT_PATH = re.compile(r"objects/sha256/([0-9a-f]{2})/([0-9a-f]{62})\Z")
SHA256_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
GENERATION_ID = re.compile(r"m4d-project-generation-v1-sha256:[0-9a-f]{64}\Z")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z")
U64 = re.compile(r"(?:0|[1-9][0-9]*)\Z")
HEX32 = re.compile(r"[0-9a-f]{8}\Z")
HEX64 = re.compile(r"[0-9a-f]{16}\Z")

EXPECTED_MUTATIONS = [
    ("envelope-noncanonical", "noncanonical_json"),
    ("envelope-unknown-field", "unknown_field"),
    ("head-truncated", "ref_length"),
    ("head-checksum-flip", "ref_checksum"),
    ("head-target-missing", "missing_generation"),
    ("manual-recovery-not-previous", "recovery_mismatch"),
    ("autosave-recovery-not-previous", "recovery_mismatch"),
    ("generation-byte-flip", "generation_digest"),
    ("generation-unknown-field", "unknown_field"),
    ("generation-project-mismatch", "project_mismatch"),
    ("revision-above-high-water", "revision_invalid"),
    ("direct-object-missing", "missing_object"),
    ("page-truncated", "object_digest"),
    ("page-reordered", "page_order"),
    ("page-substituted", "page_substitution"),
    ("scientific-rebind", "scientific_rebind"),
]


class ValidationError(RuntimeError):
    def __init__(self, code: str, detail: str):
        super().__init__(f"{code}: {detail}")
        self.code = code


def fail(code: str, detail: str) -> None:
    raise ValidationError(code, detail)


def require(condition: bool, code: str, detail: str) -> None:
    if not condition:
        fail(code, detail)


def canonical_json(value: object) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def decode_json(encoded: bytes, code: str, label: str) -> Any:
    def pairs(rows: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in rows:
            require(key not in result, "duplicate_key", f"{label}: {key}")
            result[key] = value
        return result

    def reject_float(value: str) -> None:
        fail(code, f"{label}: JSON float is forbidden: {value}")

    def reject_constant(value: str) -> None:
        fail(code, f"{label}: JSON constant is forbidden: {value}")

    try:
        return json.loads(
            encoded,
            object_pairs_hook=pairs,
            parse_float=reject_float,
            parse_constant=reject_constant,
        )
    except ValidationError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValidationError(code, f"invalid JSON: {label}") from error


def canonical_document(encoded: bytes, label: str) -> dict[str, Any]:
    value = decode_json(encoded, "invalid_json", label)
    require(isinstance(value, dict), "invalid_json", f"{label}: root must be an object")
    require(canonical_json(value) == encoded, "noncanonical_json", label)
    return value


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    require(actual == expected, "unknown_field", f"{label}: expected {sorted(expected)}, got {sorted(actual)}")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65536), b""):
            digest.update(block)
    return digest.hexdigest()


def generation_identity(encoded: bytes) -> str:
    preimage = GENERATION_DOMAIN + len(encoded).to_bytes(8, "big") + encoded
    return GENERATION_PREFIX + sha256(preimage)


def generation_path(identity: str) -> str:
    digest = identity.removeprefix(GENERATION_PREFIX)
    return f"generations/sha256/{digest[:2]}/{digest[2:]}.json"


def object_path(identity: str) -> str:
    digest = identity.removeprefix("sha256:")
    return f"objects/sha256/{digest[:2]}/{digest[2:]}"


def checked_repository_path(value: Any, label: str) -> str:
    require(isinstance(value, str) and value and value.isascii(), "manifest", f"{label}: path must be ASCII")
    path = PurePosixPath(value)
    require(
        not path.is_absolute()
        and "\\" not in value
        and "." not in path.parts
        and ".." not in path.parts
        and len(value.encode("ascii")) <= 240,
        "manifest",
        f"unsafe path: {value!r}",
    )
    return value


def resolve_archive(manifest_path: Path, declared: str) -> Path:
    declared = checked_repository_path(declared, "archive")
    final = "fixtures/project/"
    if declared.startswith(final) and manifest_path.resolve() != (ROOT / "fixtures/project/manifest.json").resolve():
        return manifest_path.parent / declared.removeprefix(final)
    return ROOT / declared


def require_u64(value: Any, label: str) -> int:
    require(isinstance(value, str) and U64.fullmatch(value) is not None, "invalid_value", f"{label}: invalid u64")
    number = int(value)
    require(number <= (1 << 64) - 1, "invalid_value", f"{label}: u64 overflow")
    return number


def require_u32(value: Any, label: str) -> int:
    require(isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= 0xFFFF_FFFF, "invalid_value", f"{label}: invalid u32")
    return value


def require_id(value: Any, prefix: str, label: str) -> str:
    require(
        isinstance(value, str)
        and value.startswith(prefix)
        and re.fullmatch(r"[0-9a-f]{64}", value.removeprefix(prefix)) is not None,
        "invalid_value",
        f"{label}: invalid typed digest",
    )
    return value


def require_f32(value: Any, label: str) -> float:
    require(isinstance(value, str) and HEX32.fullmatch(value) is not None, "invalid_value", f"{label}: invalid f32 bits")
    decoded = struct.unpack(">f", bytes.fromhex(value))[0]
    require(math.isfinite(decoded), "invalid_value", f"{label}: non-finite f32")
    return decoded


def require_f64(value: Any, label: str) -> float:
    require(isinstance(value, str) and HEX64.fullmatch(value) is not None, "invalid_value", f"{label}: invalid f64 bits")
    decoded = struct.unpack(">d", bytes.fromhex(value))[0]
    require(math.isfinite(decoded), "invalid_value", f"{label}: non-finite f64")
    return decoded


def validate_quaternion(values: Any, label: str) -> None:
    require(isinstance(values, list) and len(values) == 4, "invalid_value", f"{label}: quaternion shape")
    decoded = [require_f64(value, label) for value in values]
    require(all(value != 0.0 or not value.hex().startswith("-") for value in decoded), "invalid_value", f"{label}: negative zero")
    x, y, z, w = decoded
    sign_negative = w < 0.0 or (w == 0.0 and (x < 0.0 or (x == 0.0 and (y < 0.0 or (y == 0.0 and z < 0.0)))))
    require(not sign_negative, "invalid_value", f"{label}: noncanonical quaternion sign")
    norm = sum(value * value for value in decoded)
    require(abs(norm - 1.0) <= 16.0 * sys.float_info.epsilon, "invalid_value", f"{label}: quaternion norm")


def validate_curve(value: Any, label: str) -> None:
    require(isinstance(value, dict), "invalid_value", label)
    kind = value.get("kind")
    if kind == "linear":
        exact_keys(value, {"kind"}, label)
    elif kind == "gamma":
        exact_keys(value, {"kind", "value"}, label)
        require_f32(value["value"], label)
    else:
        fail("invalid_value", f"{label}: curve kind")


def validate_window(value: Any, label: str) -> None:
    require(isinstance(value, dict), "invalid_value", label)
    exact_keys(value, {"high", "low"}, label)
    low = require_f32(value["low"], label)
    high = require_f32(value["high"], label)
    require(high > low, "invalid_value", f"{label}: window order")


def validate_transfer(value: Any, label: str) -> None:
    require(isinstance(value, dict), "invalid_value", label)
    exact_keys(value, {"color_rgb", "curve", "invert", "opacity", "window"}, label)
    require(isinstance(value["color_rgb"], list) and len(value["color_rgb"]) == 3, "invalid_value", label)
    for component in value["color_rgb"]:
        decoded = require_f32(component, label)
        require(0.0 <= decoded <= 1.0, "invalid_value", label)
    opacity = require_f32(value["opacity"], label)
    require(0.0 <= opacity <= 1.0 and isinstance(value["invert"], bool), "invalid_value", label)
    validate_curve(value["curve"], label)
    validate_window(value["window"], label)


def validate_render(value: Any, label: str) -> None:
    require(isinstance(value, dict), "invalid_value", label)
    mode = value.get("mode")
    require(value.get("sampling") in {"smooth_linear", "voxel_exact"}, "invalid_value", label)
    if mode == "mip":
        exact_keys(value, {"mode", "sampling"}, label)
    elif mode == "isosurface":
        exact_keys(value, {"display_level", "mode", "sampling", "shading"}, label)
        require(value["shading"] in {"gradient_lighting", "flat"}, "invalid_value", label)
        level = require_f32(value["display_level"], label)
        require(0.0 <= level <= 1.0, "invalid_value", label)
    elif mode == "dvr":
        exact_keys(value, {"density_scale", "mode", "opacity_transfer", "sampling"}, label)
        require(require_f64(value["density_scale"], label) > 0.0, "invalid_value", label)
        opacity = value["opacity_transfer"]
        require(isinstance(opacity, dict), "invalid_value", label)
        exact_keys(opacity, {"curve", "window"}, label)
        validate_curve(opacity["curve"], label)
        validate_window(opacity["window"], label)
    else:
        fail("invalid_value", f"{label}: render mode")


def validate_layer(value: Any, label: str) -> int:
    require(isinstance(value, dict), "invalid_value", label)
    exact_keys(value, {"layer", "render", "transfer", "visible"}, label)
    ordinal = require_u32(value["layer"], label)
    require(isinstance(value["visible"], bool), "invalid_value", label)
    validate_transfer(value["transfer"], label)
    validate_render(value["render"], label)
    return ordinal


def validate_state(value: Any) -> None:
    require(isinstance(value, dict), "invalid_value", "state")
    exact_keys(value, {"channel_presets", "view"}, "state")
    view = value["view"]
    require(isinstance(view, dict), "invalid_value", "view")
    exact_keys(view, {"active_layer", "camera", "cross_section", "iso_light", "layers", "layout", "timepoint"}, "view")
    require(isinstance(view["layers"], list) and view["layers"], "invalid_value", "layers")
    layer_keys = [validate_layer(layer, "layer") for layer in view["layers"]]
    require(len(layer_keys) == len(set(layer_keys)), "invalid_value", "duplicate layer")
    require(require_u32(view["active_layer"], "active layer") in layer_keys, "invalid_value", "active layer")
    require_u64(view["timepoint"], "timepoint")
    require(view["layout"] in {"single3d", "four_panel"}, "invalid_value", "layout")

    camera = view["camera"]
    require(isinstance(camera, dict), "invalid_value", "camera")
    exact_keys(camera, {"orientation_xyzw", "orthographic_world_per_screen_point", "perspective_focal_length_screen_points", "perspective_view_distance_world", "projection", "target"}, "camera")
    require(camera["projection"] in {"perspective", "orthographic"}, "invalid_value", "projection")
    validate_quaternion(camera["orientation_xyzw"], "camera quaternion")
    require(isinstance(camera["target"], list) and len(camera["target"]) == 3, "invalid_value", "camera target")
    for item in camera["target"]:
        require_f64(item, "camera target")
    for field in ["orthographic_world_per_screen_point", "perspective_focal_length_screen_points", "perspective_view_distance_world"]:
        require(require_f64(camera[field], field) > 0.0, "invalid_value", field)

    section = view["cross_section"]
    require(isinstance(section, dict), "invalid_value", "cross section")
    exact_keys(section, {"center", "depth_world", "orientation_xyzw", "scale_world_per_screen_point"}, "cross section")
    require(isinstance(section["center"], list) and len(section["center"]) == 3, "invalid_value", "center")
    for item in section["center"]:
        require_f64(item, "center")
    validate_quaternion(section["orientation_xyzw"], "section quaternion")
    require(require_f64(section["depth_world"], "depth") > 0.0, "invalid_value", "depth")
    require(require_f64(section["scale_world_per_screen_point"], "scale") > 0.0, "invalid_value", "scale")

    light = view["iso_light"]
    require(isinstance(light, dict), "invalid_value", "light")
    if light.get("kind") == "attached_camera":
        exact_keys(light, {"kind"}, "light")
    elif light.get("kind") == "detached_screen":
        exact_keys(light, {"kind", "x", "y"}, "light")
        x, y = require_f32(light["x"], "light"), require_f32(light["y"], "light")
        require(x * x + y * y <= 1.0, "invalid_value", "light disc")
    else:
        fail("invalid_value", "light kind")

    presets = value["channel_presets"]
    require(isinstance(presets, list), "invalid_value", "presets")
    ids: set[str] = set()
    for preset in presets:
        require(isinstance(preset, dict), "invalid_value", "preset")
        exact_keys(preset, {"entries", "id", "label"}, "preset")
        require(isinstance(preset["id"], str) and re.fullmatch(r"[A-Za-z0-9_-]+", preset["id"]), "invalid_value", "preset id")
        require(preset["id"] not in ids, "invalid_value", "duplicate preset")
        ids.add(preset["id"])
        require(isinstance(preset["label"], str) and not any(ord(char) < 32 for char in preset["label"]), "invalid_value", "preset label")
        require(isinstance(preset["entries"], list), "invalid_value", "preset entries")
        keys = [validate_layer(entry, "preset entry") for entry in preset["entries"]]
        require(set(keys) == set(layer_keys) and len(keys) == len(layer_keys), "invalid_value", "preset closure")


def validate_descriptor(value: Any, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), "invalid_value", label)
    exact_keys(value, {"byte_length", "digest", "media_type", "role"}, label)
    require_u64(value["byte_length"], label)
    require(isinstance(value["digest"], str) and SHA256_ID.fullmatch(value["digest"]), "invalid_value", label)
    require(isinstance(value["media_type"], str) and re.fullmatch(r"[a-z0-9!#$&^_.+-]+/[a-z0-9!#$&^_.+-]+", value["media_type"]), "invalid_value", label)
    require(isinstance(value["role"], str) and re.fullmatch(r"[a-z0-9][a-z0-9._-]*[a-z0-9]", value["role"]), "invalid_value", label)
    return value


def validate_physical(value: Any, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), "invalid_value", label)
    exact_keys(value, {"byte_length", "digest"}, label)
    require_u64(value["byte_length"], label)
    require(isinstance(value["digest"], str) and SHA256_ID.fullmatch(value["digest"]), "invalid_value", label)
    return value


def object_bytes(files: dict[str, bytes], descriptor: dict[str, Any]) -> bytes:
    path = object_path(descriptor["digest"])
    require(path in files, "missing_object", path)
    data = files[path]
    require(len(data) == require_u64(descriptor["byte_length"], path) and "sha256:" + sha256(data) == descriptor["digest"], "object_digest", path)
    return data


def validate_binding(files: dict[str, bytes], logical: dict[str, Any], descriptor: dict[str, Any]) -> list[dict[str, Any]]:
    require(descriptor["media_type"] == "application/vnd.mirante4d.project-object-binding-v1+json" and descriptor["role"] == "project.object-binding.v1", "invalid_value", "binding descriptor type")
    encoded = object_bytes(files, descriptor)
    binding = canonical_document(encoded, "binding manifest")
    exact_keys(binding, {"logical_descriptor", "pages", "schema", "schema_version"}, "binding manifest")
    require(binding["schema"] == "mirante4d-project-logical-object-binding" and binding["schema_version"] == 1, "invalid_value", "binding schema")
    validate_descriptor(binding["logical_descriptor"], "binding logical descriptor")
    require(binding["logical_descriptor"] == logical, "page_substitution", "binding logical descriptor mismatch")
    pages = binding["pages"]
    require(isinstance(pages, list) and pages, "page_order", "empty pages")
    digest = hashlib.sha256()
    total = 0
    physical = [{"byte_length": descriptor["byte_length"], "digest": descriptor["digest"]}]
    for index, page in enumerate(pages):
        require(isinstance(page, dict), "page_order", "page record")
        exact_keys(page, {"byte_length", "digest", "offset", "ordinal"}, "page")
        require(require_u32(page["ordinal"], "page ordinal") == index, "page_order", "page ordinal")
        require(require_u64(page["offset"], "page offset") == total, "page_order", "page offset")
        length = require_u64(page["byte_length"], "page length")
        if index + 1 < len(pages):
            require(length == PAGE_BYTES, "page_order", "non-final page size")
        else:
            require(0 < length <= PAGE_BYTES, "page_order", "final page size")
        page_descriptor = {"byte_length": page["byte_length"], "digest": page["digest"]}
        data = object_bytes(files, page_descriptor)
        digest.update(data)
        total += len(data)
        physical.append(page_descriptor)
    require(total == require_u64(logical["byte_length"], "logical length") and "sha256:" + digest.hexdigest() == logical["digest"], "page_substitution", "reconstructed logical object mismatch")
    return physical


def validate_artifacts(files: dict[str, bytes], artifacts: Any) -> list[dict[str, Any]]:
    require(isinstance(artifacts, list), "invalid_value", "artifacts")
    handles: set[str] = set()
    closure: list[dict[str, Any]] = []
    for artifact in artifacts:
        require(isinstance(artifact, dict), "invalid_value", "artifact")
        exact_keys(artifact, {"completeness", "content_id", "derivation_id", "handle_id", "label", "logical_object", "recipe_id", "recoverability", "schema", "source_layers", "storage", "visible"}, "artifact")
        require(isinstance(artifact["handle_id"], str) and UUID.fullmatch(artifact["handle_id"]) and artifact["handle_id"] not in handles, "invalid_value", "artifact handle")
        handles.add(artifact["handle_id"])
        require(artifact["schema"] in {"roi.v1", "track.v1", "annotation.v1", "measurement.v1", "analysis-table.v1", "analysis-plot.v1"}, "invalid_value", "artifact schema")
        require(artifact["completeness"] in {"partial", "complete"} and artifact["recoverability"] in {"regenerable", "non_regenerable"}, "invalid_value", "artifact state")
        require_id(artifact["content_id"], "m4d-artifact-v1-sha256:", "artifact content")
        for field, prefix in [("derivation_id", "m4d-derivation-record-v1-sha256:"), ("recipe_id", "m4d-recipe-v1-sha256:")]:
            if artifact[field] is not None:
                require_id(artifact[field], prefix, field)
        if artifact["recoverability"] == "regenerable":
            require(artifact["derivation_id"] is not None and artifact["recipe_id"] is not None, "invalid_value", "regenerable provenance")
        require(isinstance(artifact["source_layers"], list) and all(isinstance(item, int) and not isinstance(item, bool) for item in artifact["source_layers"]), "invalid_value", "artifact source layers")
        require(isinstance(artifact["label"], str) and isinstance(artifact["visible"], bool), "invalid_value", "artifact presentation")
        logical = validate_descriptor(artifact["logical_object"], "logical object")
        storage = artifact["storage"]
        require(isinstance(storage, dict), "invalid_value", "storage")
        if storage.get("kind") == "direct":
            exact_keys(storage, {"kind", "object"}, "direct storage")
            physical = validate_physical(storage["object"], "direct object")
            require(physical["digest"] == logical["digest"] and physical["byte_length"] == logical["byte_length"], "invalid_value", "direct binding")
            require(require_u64(logical["byte_length"], "direct length") <= PAGE_BYTES, "invalid_value", "direct object too large")
            object_bytes(files, physical)
            closure.append(physical)
        elif storage.get("kind") == "paged":
            exact_keys(storage, {"binding_manifest", "kind"}, "paged storage")
            require(require_u64(logical["byte_length"], "paged length") > PAGE_BYTES, "invalid_value", "small paged object")
            binding = validate_descriptor(storage["binding_manifest"], "binding descriptor")
            closure.extend(validate_binding(files, logical, binding))
        else:
            fail("invalid_value", "storage kind")
    closure.sort(key=lambda row: (row["digest"], int(row["byte_length"])))
    require(len({row["digest"] for row in closure}) == len(closure), "invalid_value", "duplicate reachable digest")
    return closure


def validate_generation(files: dict[str, bytes], identity: str, encoded: bytes, project_id: str) -> dict[str, Any]:
    require(generation_identity(encoded) == identity, "generation_digest", identity)
    value = canonical_document(encoded, identity)
    exact_keys(value, {"artifacts", "base_manual_generation_id", "dataset", "forked_from", "generation_kind", "generation_sequence", "parent_generation_id", "project_id", "reachable_objects", "revision_high_water_sequence", "revision_sequence", "schema", "schema_version", "state"}, "generation")
    require(value["schema"] == "mirante4d-project-generation" and value["schema_version"] == 1, "invalid_value", "generation schema")
    require(value["project_id"] == project_id, "project_mismatch", identity)
    require(value["generation_kind"] in {"manual", "autosave"}, "invalid_value", "generation kind")
    require_u64(value["generation_sequence"], "generation sequence")
    revision = require_u64(value["revision_sequence"], "revision")
    high = require_u64(value["revision_high_water_sequence"], "revision high water")
    require(revision <= high, "revision_invalid", identity)
    for field in ["parent_generation_id", "base_manual_generation_id"]:
        require(value[field] is None or (isinstance(value[field], str) and GENERATION_ID.fullmatch(value[field])), "invalid_value", field)
    require((value["generation_kind"] == "manual" and value["base_manual_generation_id"] is None) or value["generation_kind"] == "autosave", "invalid_value", "manual base")
    if value["forked_from"] is not None:
        require(isinstance(value["forked_from"], dict), "invalid_value", "fork provenance")
        exact_keys(value["forked_from"], {"generation_id", "project_id"}, "fork provenance")
        require(isinstance(value["forked_from"]["generation_id"], str) and GENERATION_ID.fullmatch(value["forked_from"]["generation_id"]), "invalid_value", "fork generation")
        require(isinstance(value["forked_from"]["project_id"], str) and UUID.fullmatch(value["forked_from"]["project_id"]), "invalid_value", "fork project")

    dataset = value["dataset"]
    require(isinstance(dataset, dict), "invalid_value", "dataset")
    exact_keys(dataset, {"locator_hint", "package_id", "release_id", "scientific_content_id"}, "dataset")
    require_id(dataset["scientific_content_id"], "m4d-sc-v1-sha256:", "science")
    if dataset["package_id"] is not None:
        require_id(dataset["package_id"], "m4d-package-v1-sha256:", "package")
    if dataset["release_id"] is not None:
        require_id(dataset["release_id"], "m4d-release-v1-sha256:", "release")
    require(dataset["locator_hint"] is None or isinstance(dataset["locator_hint"], str), "invalid_value", "locator")
    validate_state(value["state"])
    derived = validate_artifacts(files, value["artifacts"])
    reachable = value["reachable_objects"]
    require(isinstance(reachable, list), "invalid_value", "reachable objects")
    observed = [validate_physical(item, "reachable object") for item in reachable]
    require(observed == sorted(observed, key=lambda row: (row["digest"], int(row["byte_length"]))) and observed == derived, "invalid_value", "reachable closure")
    return value


def parse_ref(path: str, encoded: bytes, project_id: str) -> dict[str, Any]:
    require(len(encoded) == 160, "ref_length", path)
    require(encoded[:8] == REF_MAGIC and int.from_bytes(encoded[8:10], "big") == 1 and int.from_bytes(encoded[12:16], "big") == 160, "ref_header", path)
    require(encoded[16:32] == uuid.UUID(project_id).bytes, "project_mismatch", path)
    require(hashlib.sha256(REF_DOMAIN + encoded[:128]).digest() == encoded[128:], "ref_checksum", path)
    kind, presence = encoded[10], encoded[11]
    expected_kind = 5 if path.startswith("refs/pins/") else {"refs/head": 1, "refs/recovery": 2, "refs/autosave-head": 3, "refs/autosave-recovery": 4}.get(path)
    require(expected_kind is not None and kind == expected_kind, "ref_header", path)
    require(presence & ~3 == 0, "ref_header", path)
    allowed = {1: {0, 1}, 2: {0}, 3: {0, 1, 2, 3}, 4: {0}, 5: {0}}
    require(presence in allowed[kind], "ref_header", path)
    slots = [encoded[32:64], encoded[64:96], encoded[96:128]]
    previous_present, base_present = bool(presence & 1), bool(presence & 2)
    require(previous_present or slots[1] == bytes(32), "ref_header", path)
    require(base_present or slots[2] == bytes(32), "ref_header", path)

    def typed(raw: bytes, present: bool = True) -> str | None:
        return GENERATION_PREFIX + raw.hex() if present else None

    return {
        "base": typed(slots[2], base_present),
        "current": typed(slots[0]),
        "kind": kind,
        "previous": typed(slots[1], previous_present),
    }


def validate_recovery_pair(
    refs: dict[str, dict[str, Any]], head_path: str, recovery_path: str
) -> None:
    head = refs.get(head_path)
    recovery = refs.get(recovery_path)
    if head is None:
        require(recovery is None, "recovery_mismatch", recovery_path)
        return
    previous = head["previous"]
    require(
        (recovery is None) == (previous is None),
        "recovery_mismatch",
        recovery_path,
    )
    if recovery is not None:
        require(
            recovery["current"] == previous,
            "recovery_mismatch",
            recovery_path,
        )


def validate_store(store_files: dict[str, bytes], expected: dict[str, Any]) -> dict[str, Any]:
    require("project.json" in store_files, "invalid_store", "missing project.json")
    envelope = canonical_document(store_files["project.json"], "project.json")
    exact_keys(envelope, {"profile", "project_id", "schema", "schema_version"}, "envelope")
    require(envelope["schema"] == "mirante4d-project-store-envelope" and envelope["schema_version"] == 1 and envelope["profile"] == "mirante4d-project-store-v1", "invalid_value", "envelope")
    project_id = envelope["project_id"]
    require(isinstance(project_id, str) and UUID.fullmatch(project_id) and str(uuid.UUID(project_id)) == project_id, "invalid_value", "project ID")

    generations: dict[str, dict[str, Any]] = {}
    object_files: set[str] = set()
    refs: dict[str, dict[str, Any]] = {}
    for path, data in store_files.items():
        generation_match = GENERATION_PATH.fullmatch(path)
        object_match = OBJECT_PATH.fullmatch(path)
        if path == "project.json":
            continue
        if generation_match:
            identity = GENERATION_PREFIX + generation_match.group(1) + generation_match.group(2)
            generations[identity] = validate_generation(store_files, identity, data, project_id)
        elif object_match:
            expected_digest = object_match.group(1) + object_match.group(2)
            require(sha256(data) == expected_digest, "object_digest", path)
            object_files.add("sha256:" + expected_digest)
        elif path in {"refs/head", "refs/recovery", "refs/autosave-head", "refs/autosave-recovery"} or path.startswith("refs/pins/"):
            if path.startswith("refs/pins/"):
                name = path.removeprefix("refs/pins/")
                require(re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", name) is not None, "ref_header", path)
            refs[path] = parse_ref(path, data, project_id)
        else:
            fail("unknown_file", path)
    for path, ref in refs.items():
        for slot in ["current", "previous", "base"]:
            identity = ref[slot]
            if identity is not None:
                require(identity in generations, "missing_generation", f"{path}:{slot}")
        current = generations[ref["current"]]
        if ref["kind"] in {1, 2}:
            require(current["generation_kind"] == "manual", "invalid_store", path)
        elif ref["kind"] in {3, 4}:
            require(current["generation_kind"] == "autosave", "invalid_store", path)
        if ref["kind"] == 3:
            require(ref["base"] == current["base_manual_generation_id"], "invalid_store", "autosave base")

    head = refs.get("refs/head")
    autosave_ref = refs.get("refs/autosave-head")
    if head is None:
        require(autosave_ref is not None, "invalid_store", "missing all heads")
        require(
            autosave_ref["base"] is None
            and generations[autosave_ref["current"]]["base_manual_generation_id"] is None,
            "invalid_store",
            "provisional state",
        )
    else:
        require(
            head["previous"] is None
            or generations[head["current"]]["parent_generation_id"] == head["previous"],
            "invalid_store",
            "manual parent",
        )
        if autosave_ref is not None:
            require(
                autosave_ref["base"] is not None,
                "invalid_store",
                "established autosave base",
            )
    if autosave_ref is not None:
        require(
            autosave_ref["previous"] is None
            or generations[autosave_ref["current"]]["parent_generation_id"]
            == autosave_ref["previous"],
            "invalid_store",
            "autosave parent",
        )

    validate_recovery_pair(refs, "refs/head", "refs/recovery")
    validate_recovery_pair(refs, "refs/autosave-head", "refs/autosave-recovery")

    primary = head if head is not None else autosave_ref
    assert primary is not None
    scientific = generations[primary["current"]]["dataset"]["scientific_content_id"]
    require(all(generation["dataset"]["scientific_content_id"] == scientific for generation in generations.values()), "scientific_rebind", "generation science changed")
    fork_values = {json.dumps(generation["forked_from"], sort_keys=True) for generation in generations.values()}
    require(len(fork_values) == 1, "project_mismatch", "fork provenance changed")

    roots: set[str] = set()
    for ref in refs.values():
        roots.update(identity for identity in [ref["current"], ref["previous"], ref["base"]] if identity is not None)
    candidates = sorted(set(generations) - roots)
    autosave = "none"
    autosave_head = None
    if autosave_ref is not None:
        autosave_head = autosave_ref["current"]
        auto_generation = generations[autosave_head]
        if head is None:
            autosave = "provisional"
        elif auto_generation["base_manual_generation_id"] != head["current"]:
            autosave = "divergent"
        elif auto_generation["revision_sequence"] != generations[head["current"]]["revision_sequence"]:
            autosave = "newer"
        else:
            autosave = "stale"
    observed = {
        "autosave": autosave,
        "autosave_head": autosave_head,
        "autosave_head_previous": None if autosave_ref is None else autosave_ref["previous"],
        "autosave_recovery": refs.get("refs/autosave-recovery", {}).get("current"),
        "head": None if head is None else head["current"],
        "head_previous": None if head is None else head["previous"],
        "manual_recovery": refs.get("refs/recovery", {}).get("current"),
        "project_id": project_id,
        "recovery_candidates": candidates,
        "roots": sorted(roots),
    }
    for key, value in observed.items():
        require(expected.get(key) == value, "expected_facts", f"{expected.get('root')}:{key}")
    return {"generations": generations, "observed": observed, "refs": refs}


def tree_sha256(files: dict[str, bytes]) -> str:
    digest = hashlib.sha256()
    for path, data in sorted(files.items()):
        digest.update(path.encode("ascii") + b"\0" + bytes.fromhex(sha256(data)) + b"\0")
        digest.update(str(len(data)).encode("ascii") + b"\n")
    return digest.hexdigest()


def read_archive(encoded: bytes, limits: dict[str, Any]) -> dict[str, bytes]:
    archive_max = int(limits["archive_bytes_max"])
    regular_max = int(limits["regular_file_bytes_max"])
    files_max = int(limits["files_max"])
    require(0 < len(encoded) <= archive_max, "archive_limit", "archive bytes")
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(encoded), mode="rb") as compressed:
            raw = compressed.read(regular_max + 2 * 1024 * 1024 + 1)
    except (OSError, EOFError) as error:
        raise ValidationError("archive", "invalid gzip") from error
    require(len(raw) <= regular_max + 2 * 1024 * 1024, "archive_limit", "tar bytes")
    result: dict[str, bytes] = {}
    directories: set[str] = set()
    names: set[str] = set()
    folded: set[str] = set()
    regular_bytes = 0
    try:
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as archive:
            for member in archive.getmembers():
                name = member.name[:-1] if member.isdir() and member.name.endswith("/") else member.name
                checked_repository_path(name, "archive member")
                require(name not in names and name.casefold() not in folded, "archive", f"duplicate path {name}")
                require(not member.linkname and not member.pax_headers and (member.isfile() or member.isdir()), "archive", f"unsafe member {name}")
                names.add(name)
                folded.add(name.casefold())
                if member.isdir():
                    directories.add(name)
                    continue
                require(member.size >= 0 and member.size <= PAGE_BYTES, "archive_limit", name)
                source = archive.extractfile(member)
                require(source is not None, "archive", name)
                data = source.read(member.size + 1)
                require(len(data) == member.size, "archive", f"short member {name}")
                regular_bytes += len(data)
                require(regular_bytes <= regular_max, "archive_limit", "regular bytes")
                result[name] = data
    except (tarfile.TarError, OSError) as error:
        raise ValidationError("archive", "invalid USTAR") from error
    require(0 < len(result) <= files_max, "archive_limit", "file count")
    return result


def validate_manifest_document(manifest: dict[str, Any], manifest_path: Path, manifest_bytes: bytes) -> tuple[dict[str, bytes], list[dict[str, Any]]]:
    require(canonical_json(manifest) + b"\n" == manifest_bytes, "manifest", "manifest is not canonical plus newline")
    exact_keys(manifest, {"approvals", "archive", "contract", "fixture_id", "limits", "lineages", "mutations", "schema", "schema_version", "status", "stores"}, "manifest")
    require(manifest["schema"] == "mirante4d-foundation-project-fixture-manifest" and manifest["schema_version"] == 1 and manifest["fixture_id"] == "project-store-v1" and manifest["status"] == "independently_validated", "manifest", "identity")
    approval = manifest.get("approvals", {}).get("repository_owner", {})
    require(approval.get("state") == "approved" and approval.get("reference") == "WP10B-PREAUTH-2026-07-12", "manifest", "approval")
    contract = manifest["contract"]
    require(contract == {"path": "architecture/wp10b-project-store-contract.json", "schema": "mirante4d-wp10b-project-store-contract", "schema_version": 1}, "manifest", "contract binding")
    contract_value = decode_json((ROOT / contract["path"]).read_bytes(), "manifest", "contract")
    require(contract_value.get("schema") == contract["schema"] and contract_value.get("schema_version") == 1, "manifest", "contract identity")
    for lineage in ["producer", "reproducer", "validator"]:
        row = manifest.get("lineages", {}).get(lineage, {})
        relative = checked_repository_path(row.get("path"), f"{lineage} path")
        require(sha256_file(ROOT / relative) == row.get("sha256"), "manifest", f"{lineage} digest")
    mutation_facts = [(row.get("id"), row.get("expected_fault")) for row in manifest.get("mutations", [])]
    require(mutation_facts == EXPECTED_MUTATIONS, "manifest", "mutation inventory")
    archive_row = manifest["archive"]
    archive_path = resolve_archive(manifest_path, archive_row["path"])
    encoded = archive_path.read_bytes()
    require(len(encoded) == archive_row["archive_bytes"] and sha256(encoded) == archive_row["sha256"], "archive_digest", "archive identity")
    files = read_archive(encoded, manifest["limits"])
    require(len(files) == archive_row["file_count"] and sum(map(len, files.values())) == archive_row["regular_file_bytes"] and max(map(len, files.values())) == archive_row["max_file_bytes"] and tree_sha256(files) == archive_row["tree_sha256"], "archive_digest", "archive facts")
    stores = manifest["stores"]
    require(
        isinstance(stores, list)
        and [row.get("root") for row in stores]
        == [
            "recoverable.m4dproj",
            "divergent.m4dproj",
            "stale.m4dproj",
            "provisional.m4dproj",
        ],
        "manifest",
        "store inventory",
    )
    store_keys = {
        "autosave",
        "autosave_head",
        "autosave_head_previous",
        "autosave_recovery",
        "head",
        "head_previous",
        "manual_recovery",
        "project_id",
        "recovery_candidates",
        "root",
        "roots",
    }
    for store in stores:
        require(isinstance(store, dict), "manifest", "store facts")
        exact_keys(store, store_keys, "manifest store facts")
    return files, stores


def split_stores(files: dict[str, bytes], stores: list[dict[str, Any]]) -> None:
    expected_roots = {row["root"] for row in stores}
    observed_roots = {PurePosixPath(path).parts[0] for path in files}
    require(observed_roots == expected_roots, "archive", "store roots")
    for expected in stores:
        prefix = expected["root"] + "/"
        store_files = {path.removeprefix(prefix): data for path, data in files.items() if path.startswith(prefix)}
        validate_store(store_files, expected)


def store_subset(files: dict[str, bytes], root: str) -> dict[str, bytes]:
    prefix = root + "/"
    return {path.removeprefix(prefix): data for path, data in files.items() if path.startswith(prefix)}


def reseal_ref_current(encoded: bytes, digest_byte: int) -> bytes:
    value = bytearray(encoded)
    value[32:64] = bytes([digest_byte]) * 32
    value[128:160] = hashlib.sha256(REF_DOMAIN + value[:128]).digest()
    return bytes(value)


def reseal_ref_generation(encoded: bytes, generation: str) -> bytes:
    value = bytearray(encoded)
    value[32:64] = bytes.fromhex(generation.removeprefix(GENERATION_PREFIX))
    value[128:160] = hashlib.sha256(REF_DOMAIN + value[:128]).digest()
    return bytes(value)


def reseal_orphan(files: dict[str, bytes], expected: dict[str, Any], mutate: Any) -> tuple[str, dict[str, Any]]:
    old = expected["recovery_candidates"][0]
    path = generation_path(old)
    value = decode_json(files[path], "invalid_json", path)
    mutate(value)
    encoded = canonical_json(value)
    new = generation_identity(encoded)
    del files[path]
    files[generation_path(new)] = encoded
    return new, value


def paged_artifact(value: dict[str, Any]) -> dict[str, Any]:
    return next(artifact for artifact in value["artifacts"] if artifact["storage"]["kind"] == "paged")


def mutate_binding(files: dict[str, bytes], expected: dict[str, Any], operation: str) -> None:
    old = expected["recovery_candidates"][0]
    generation = decode_json(files[generation_path(old)], "invalid_json", "orphan")
    artifact = paged_artifact(generation)
    old_descriptor = artifact["storage"]["binding_manifest"]
    binding = decode_json(files[object_path(old_descriptor["digest"])], "invalid_json", "binding")
    if operation == "reorder":
        binding["pages"] = list(reversed(binding["pages"]))
    else:
        direct = next(item for item in generation["artifacts"] if item["storage"]["kind"] == "direct")
        binding["pages"][1]["digest"] = direct["logical_object"]["digest"]
        binding["pages"][1]["byte_length"] = direct["logical_object"]["byte_length"]
    binding_bytes = canonical_json(binding)
    new_descriptor = {
        "byte_length": str(len(binding_bytes)),
        "digest": "sha256:" + sha256(binding_bytes),
        "media_type": old_descriptor["media_type"],
        "role": old_descriptor["role"],
    }
    files[object_path(new_descriptor["digest"])] = binding_bytes
    artifact["storage"]["binding_manifest"] = new_descriptor
    generation["reachable_objects"] = [row for row in generation["reachable_objects"] if row["digest"] != old_descriptor["digest"]]
    generation["reachable_objects"].append({"byte_length": new_descriptor["byte_length"], "digest": new_descriptor["digest"]})
    generation["reachable_objects"].sort(key=lambda row: (row["digest"], int(row["byte_length"])))
    encoded = canonical_json(generation)
    del files[generation_path(old)]
    files[generation_path(generation_identity(encoded))] = encoded


def run_mutation(base_files: dict[str, bytes], expected: dict[str, Any], mutation: str) -> None:
    files = dict(base_files)
    if mutation == "envelope-noncanonical":
        files["project.json"] += b"\n"
    elif mutation == "envelope-unknown-field":
        value = decode_json(files["project.json"], "invalid_json", "envelope")
        value["unknown"] = True
        files["project.json"] = canonical_json(value)
    elif mutation == "head-truncated":
        files["refs/head"] = files["refs/head"][:-1]
    elif mutation == "head-checksum-flip":
        value = bytearray(files["refs/head"])
        value[-1] ^= 1
        files["refs/head"] = bytes(value)
    elif mutation == "head-target-missing":
        files["refs/head"] = reseal_ref_current(files["refs/head"], 0x11)
    elif mutation == "manual-recovery-not-previous":
        files["refs/recovery"] = reseal_ref_generation(
            files["refs/recovery"], expected["head"]
        )
    elif mutation == "autosave-recovery-not-previous":
        files["refs/autosave-recovery"] = reseal_ref_generation(
            files["refs/autosave-recovery"], expected["autosave_head"]
        )
    elif mutation == "generation-byte-flip":
        path = generation_path(expected["head"])
        value = bytearray(files[path])
        value[len(value) // 2] ^= 1
        files[path] = bytes(value)
    elif mutation == "generation-unknown-field":
        reseal_orphan(files, expected, lambda value: value.__setitem__("unknown", True))
    elif mutation == "generation-project-mismatch":
        reseal_orphan(files, expected, lambda value: value.__setitem__("project_id", "ffffffff-ffff-4fff-8fff-ffffffffffff"))
    elif mutation == "revision-above-high-water":
        def revision(value: dict[str, Any]) -> None:
            value["revision_sequence"] = "9"
            value["revision_high_water_sequence"] = "8"
        reseal_orphan(files, expected, revision)
    elif mutation == "direct-object-missing":
        head = decode_json(files[generation_path(expected["head"])], "invalid_json", "head")
        direct = next(item for item in head["artifacts"] if item["storage"]["kind"] == "direct")
        del files[object_path(direct["logical_object"]["digest"])]
    elif mutation == "page-truncated":
        head = decode_json(files[generation_path(expected["head"])], "invalid_json", "head")
        binding_descriptor = paged_artifact(head)["storage"]["binding_manifest"]
        binding = decode_json(files[object_path(binding_descriptor["digest"])], "invalid_json", "binding")
        path = object_path(binding["pages"][0]["digest"])
        files[path] = files[path][:-1]
    elif mutation == "page-reordered":
        mutate_binding(files, expected, "reorder")
    elif mutation == "page-substituted":
        mutate_binding(files, expected, "substitute")
    elif mutation == "scientific-rebind":
        def science(value: dict[str, Any]) -> None:
            original = value["dataset"]["scientific_content_id"]
            final = "0" if original[-1] != "0" else "1"
            value["dataset"]["scientific_content_id"] = original[:-1] + final
        reseal_orphan(files, expected, science)
    else:
        fail("self_test", f"unknown mutation {mutation}")
    validate_store(files, expected)


def validate_path(manifest_path: Path, self_test: bool) -> dict[str, Any]:
    manifest_bytes = manifest_path.read_bytes()
    require(manifest_bytes.endswith(b"\n"), "manifest", "missing final newline")
    manifest = decode_json(manifest_bytes[:-1], "manifest", str(manifest_path))
    require(isinstance(manifest, dict), "manifest", "root")
    files, stores = validate_manifest_document(manifest, manifest_path, manifest_bytes)
    split_stores(files, stores)
    mutation_count = 0
    if self_test:
        recoverable = store_subset(files, "recoverable.m4dproj")
        expected = stores[0]
        for mutation, expected_fault in EXPECTED_MUTATIONS:
            try:
                run_mutation(recoverable, expected, mutation)
            except ValidationError as error:
                require(error.code == expected_fault, "self_test", f"{mutation}: expected {expected_fault}, got {error.code}")
            else:
                fail("self_test", f"mutation passed: {mutation}")
            mutation_count += 1
        broken = copy.deepcopy(manifest)
        broken["archive"]["sha256"] = "0" * 64
        try:
            validate_manifest_document(broken, manifest_path, canonical_json(broken) + b"\n")
        except ValidationError as error:
            require(error.code == "archive_digest", "self_test", "manifest authority mutation")
        else:
            fail("self_test", "manifest authority mutation passed")
    return {
        "archive_sha256": manifest["archive"]["sha256"],
        "mutations": mutation_count,
        "result": "passed",
        "stores": len(stores),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        result = validate_path(args.manifest.resolve(), args.self_test)
    except (OSError, ValidationError) as error:
        print(f"project fixture validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
