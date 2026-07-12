#!/usr/bin/env python3
"""Verify the static WP-10A-C hand vectors using only the Python stdlib."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import struct
import sys


HEX = set("0123456789abcdef")


class VectorError(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise VectorError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def reject_number(value):
    raise VectorError(f"non-integer JSON number is forbidden: {value}")


def obj(value, keys, label):
    require(type(value) is dict and set(value) == set(keys), f"{label} fields differ")
    return value


def seq(value, length, label):
    require(type(value) is list and len(value) == length, f"{label} length differs")
    return value


def uint(value, label, maximum=(1 << 64) - 1):
    require(type(value) is int and 0 <= value <= maximum, f"{label} is not an unsigned integer")
    return value


def string(value, label):
    require(type(value) is str and value, f"{label} is not nonempty text")
    return value


def hex_bytes(value, label, length=None):
    value = string(value, label)
    require(len(value) % 2 == 0 and set(value) <= HEX, f"{label} is not lowercase hex")
    decoded = bytes.fromhex(value)
    require(length is None or len(decoded) == length, f"{label} byte length differs")
    return decoded


def digest(value, label):
    value = string(value, label)
    require(len(value) == 64 and set(value) <= HEX, f"{label} is not SHA-256")
    return value


def check_hash(data, expected, label):
    expected = digest(expected, label)
    require(hashlib.sha256(data).hexdigest() == expected, f"{label} mismatch")
    return expected


def canonical(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":")).encode()


def be32(value):
    return struct.pack(">I", value)


def be64(value):
    return struct.pack(">Q", value)


def verify_scientific(value):
    value = obj(value, {"tile", "layer", "dataset"}, "scientific")
    tile = obj(
        value["tile"],
        {"domain_ascii_nul", "layer_ordinal", "dtype", "dtype_tag", "origin_tzyx", "extent_tzyx", "validity_hex", "values_hex", "preimage_hex", "expected_sha256"},
        "scientific tile",
    )
    require(tile["domain_ascii_nul"] == "M4D-SC-V1-TILE\0" and tile["dtype"] == "uint8", "tile profile drifted")
    ordinal = uint(tile["layer_ordinal"], "tile ordinal", (1 << 32) - 1)
    dtype = uint(tile["dtype_tag"], "tile dtype", 255)
    origin = [uint(item, "tile origin") for item in seq(tile["origin_tzyx"], 4, "tile origin")]
    extent = [uint(item, "tile extent") for item in seq(tile["extent_tzyx"], 4, "tile extent")]
    validity = hex_bytes(tile["validity_hex"], "tile validity", 1)
    samples = hex_bytes(tile["values_hex"], "tile samples", 1)
    require((ordinal, dtype, origin, extent, validity, samples) == (0, 1, [0] * 4, [1] * 4, b"\x01", b"\x07"), "one-voxel tile drifted")
    tile_preimage = (
        tile["domain_ascii_nul"].encode("ascii") + be32(ordinal) + bytes([dtype])
        + b"".join(map(be64, origin)) + b"".join(map(be64, extent))
        + be64(len(validity)) + validity + be64(len(samples)) + samples
    )
    require(hex_bytes(tile["preimage_hex"], "tile preimage") == tile_preimage, "tile preimage drifted")
    tile_hash = check_hash(tile_preimage, tile["expected_sha256"], "tile digest")

    layer = obj(
        value["layer"],
        {"domain_ascii_nul", "layer_ordinal", "dtype_tag", "shape_tzyx", "temporal_tag", "grid_to_world_f64_hex", "tile_count", "tree_root_sha256", "preimage_hex", "expected_sha256"},
        "scientific layer",
    )
    shape = [uint(item, "layer shape") for item in seq(layer["shape_tzyx"], 4, "layer shape")]
    transform = seq(layer["grid_to_world_f64_hex"], 16, "grid-to-world")
    identity = ["3ff0000000000000" if index in (0, 5, 10, 15) else "0000000000000000" for index in range(16)]
    require(
        layer["domain_ascii_nul"] == "M4D-SC-V1-LAYER\0"
        and uint(layer["layer_ordinal"], "layer ordinal", (1 << 32) - 1) == 0
        and uint(layer["dtype_tag"], "layer dtype", 255) == 1
        and shape == [1] * 4
        and uint(layer["temporal_tag"], "temporal tag", 255) == 0
        and transform == identity
        and uint(layer["tile_count"], "tile count") == 1
        and digest(layer["tree_root_sha256"], "tree root") == tile_hash,
        "one-voxel layer inputs drifted",
    )
    layer_preimage = (
        layer["domain_ascii_nul"].encode("ascii") + be32(0) + b"\x01" + b"".join(map(be64, shape)) + b"\x00"
        + b"".join(item.encode("ascii") for item in transform) + be64(1) + bytes.fromhex(tile_hash)
    )
    require(hex_bytes(layer["preimage_hex"], "layer preimage") == layer_preimage, "layer preimage drifted")
    layer_hash = check_hash(layer_preimage, layer["expected_sha256"], "layer digest")

    dataset = obj(
        value["dataset"],
        {"domain_ascii_nul", "version_bytes_hex", "layer_count", "layers", "preimage_hex", "expected_sha256", "typed_id"},
        "scientific dataset",
    )
    version = hex_bytes(dataset["version_bytes_hex"], "dataset version", 4)
    binding = obj(seq(dataset["layers"], 1, "dataset layers")[0], {"ordinal", "root_sha256"}, "dataset layer")
    require(
        dataset["domain_ascii_nul"] == "M4D-SC-V1-DATASET\0"
        and version == b"\x01" * 4
        and uint(dataset["layer_count"], "dataset layer count", (1 << 32) - 1) == 1
        and uint(binding["ordinal"], "dataset layer ordinal", (1 << 32) - 1) == 0
        and digest(binding["root_sha256"], "dataset layer root") == layer_hash,
        "one-layer dataset inputs drifted",
    )
    dataset_preimage = dataset["domain_ascii_nul"].encode("ascii") + version + be32(1) + be32(0) + bytes.fromhex(layer_hash)
    require(hex_bytes(dataset["preimage_hex"], "dataset preimage") == dataset_preimage, "dataset preimage drifted")
    dataset_hash = check_hash(dataset_preimage, dataset["expected_sha256"], "dataset digest")
    require(dataset["typed_id"] == f"m4d-sc-v1-sha256:{dataset_hash}", "ScientificContentId drifted")


def merkle_root(count, domain, arity):
    nodes = [hashlib.sha256(be64(index)).digest() for index in range(count)]
    level = 0
    while len(nodes) > 1:
        level += 1
        nodes = [
            hashlib.sha256(domain + be32(level) + be32(len(group)) + b"".join(group)).digest()
            for group in (nodes[offset : offset + arity] for offset in range(0, len(nodes), arity))
        ]
    return nodes[0]


def verify_merkle(value):
    value = obj(value, {"arity", "leaf_preimage", "leaf_hash", "node_domain_ascii_nul", "node_level_encoding", "node_child_count_encoding", "cases"}, "Merkle vectors")
    require(
        uint(value["arity"], "Merkle arity", (1 << 32) - 1) == 1024
        and value["leaf_preimage"] == "u64-big-endian sequential index starting at zero"
        and value["leaf_hash"] == "sha256"
        and value["node_domain_ascii_nul"] == "M4D-SC-V1-NODE\0"
        and value["node_level_encoding"] == value["node_child_count_encoding"] == "u32-big-endian",
        "Merkle framing drifted",
    )
    for row, count in zip(seq(value["cases"], 4, "Merkle cases"), (1, 1023, 1024, 1025), strict=True):
        row = obj(row, {"leaf_count", "expected_root_sha256"}, "Merkle case")
        require(uint(row["leaf_count"], "leaf count") == count, "Merkle boundary count drifted")
        actual = merkle_root(count, value["node_domain_ascii_nul"].encode("ascii"), 1024).hex()
        require(actual == digest(row["expected_root_sha256"], "Merkle root"), f"Merkle {count} root mismatch")


def verify_packed(value):
    value = obj(
        value,
        {"encoding", "dtype", "logical_brick_capacity", "coordinates", "valid_voxel_count", "nonfill_valid_voxel_count", "numeric_range_bits", "pixel_payload_present", "explicit_validity", "flags_u32", "bytes_hex", "expected_sha256"},
        "packed index",
    )
    capacity = uint(value["logical_brick_capacity"], "brick capacity")
    coordinates = [uint(item, "packed coordinate", (1 << 32) - 1) for item in seq(value["coordinates"], 7, "packed coordinates")]
    valid = uint(value["valid_voxel_count"], "valid count")
    nonfill = uint(value["nonfill_valid_voxel_count"], "nonfill count")
    numeric = [uint(item, "numeric range") for item in seq(value["numeric_range_bits"], 2, "numeric range")]
    require(value["encoding"] == "64-byte little-endian m4d-packed-index-1.0" and value["dtype"] == "uint16", "packed profile drifted")
    require(type(value["pixel_payload_present"]) is bool and type(value["explicit_validity"]) is bool, "packed booleans are invalid")
    require(0 < capacity and nonfill <= valid <= capacity and numeric[0] <= numeric[1], "packed counts are invalid")
    flags = (1 if nonfill else 0) | (2 if value["pixel_payload_present"] else 0) | (4 if value["explicit_validity"] else 0)
    flags |= (8 if valid == capacity else 0) | (16 if valid == 0 else 0) | 32
    require(uint(value["flags_u32"], "packed flags", (1 << 32) - 1) == flags, "packed flags drifted")
    encoded = struct.pack("<8I4Q", flags, *coordinates, valid, nonfill, *numeric)
    require(hex_bytes(value["bytes_hex"], "packed bytes", 64) == encoded, "packed bytes drifted")
    check_hash(encoded, value["expected_sha256"], "packed digest")


def verify_manifest(value):
    value = obj(value, {"descriptor", "page", "root"}, "manifest")
    descriptor = obj(value["descriptor"], {"fields", "canonical_utf8", "expected_sha256"}, "descriptor")
    fields = obj(descriptor["fields"], {"bytes", "digest", "logical_role", "media_type", "path"}, "descriptor fields")
    require(fields == {"bytes": "2", "digest": "sha256:" + "0" * 64, "logical_role": "m4d.profile", "media_type": "application/vnd.mirante4d.profile+json", "path": "m4d/profile.json"}, "descriptor inputs drifted")
    descriptor_bytes = canonical(fields)
    require(descriptor["canonical_utf8"].encode() == descriptor_bytes, "descriptor bytes drifted")
    check_hash(descriptor_bytes, descriptor["expected_sha256"], "descriptor digest")

    page = obj(value["page"], {"canonical_utf8", "expected_sha256"}, "manifest page")
    page_bytes = canonical({"entries": [fields], "schema": "m4d-manifest-page", "schema_version": 1})
    require(page["canonical_utf8"].encode() == page_bytes, "page bytes drifted")
    page_hash = check_hash(page_bytes, page["expected_sha256"], "page digest")

    root = obj(value["root"], {"page_path", "canonical_utf8", "expected_sha256", "package_id"}, "manifest root")
    require(root["page_path"] == "m4d/manifest/pages/p00000000.json", "page path drifted")
    root_bytes = canonical({
        "pages": [{"bytes": str(len(page_bytes)), "digest": f"sha256:{page_hash}", "entry_count": "1", "first_path": fields["path"], "last_path": fields["path"], "path": root["page_path"]}],
        "schema": "m4d-manifest-root",
        "schema_version": 1,
    })
    require(root["canonical_utf8"].encode() == root_bytes, "root bytes drifted")
    root_hash = check_hash(root_bytes, root["expected_sha256"], "root digest")
    require(root["package_id"] == f"m4d-package-v1-sha256:{root_hash}", "PackageId drifted")


BODY_KEYS = {
    "recipe": {"determinism", "operation_registry_digest", "operations"},
    "derivation_record": {"exactness", "implementation", "inputs", "outcome", "outputs", "recipe_id", "scope"},
    "release": {"citation", "creators", "dataset_series_uuid", "derivation_record_ids", "evidence", "funders", "institutions", "license_spdx", "package_id", "portable_record_digests", "published_at", "recipe_ids", "release_ordinal", "rights_holders", "schema_profiles", "scientific_content_id", "supersedes"},
    "artifact_non_admissible": {"role", "schema"},
}


def verify_framed(value):
    domains = {
        "recipe": ("M4D-RECIPE-V1\0", "m4d-recipe-v1-sha256:"),
        "derivation_record": ("M4D-DERIVATION-RECORD-V1\0", "m4d-derivation-record-v1-sha256:"),
        "release": ("M4D-RELEASE-V1\0", "m4d-release-v1-sha256:"),
        "artifact_non_admissible": ("M4D-ARTIFACT-V1\0", "m4d-artifact-v1-sha256:"),
    }
    value = obj(value, set(domains), "framed identities")
    common = {"domain_ascii_nul", "length_encoding", "body", "body_bytes", "body_sha256", "expected_sha256", "typed_id"}
    for name, (domain, prefix) in domains.items():
        keys = common | ({"production_admissible"} if name == "artifact_non_admissible" else set())
        row = obj(value[name], keys, name)
        body = obj(row["body"], BODY_KEYS[name], f"{name} body")
        if name == "recipe":
            operation = obj(seq(body["operations"], 1, "recipe operations")[0], {"inputs", "name", "node", "numeric_policy", "output_roles", "parameter_schema", "parameters", "semantic_version"}, "recipe operation")
            obj(operation["numeric_policy"], {"boundary", "dtype", "interpolation", "kernel", "no_data", "ordering", "precision", "reduction", "rng", "rounding"}, "numeric policy")
        elif name == "derivation_record":
            obj(body["implementation"], {"build", "name", "version"}, "derivation implementation")
            obj(seq(body["inputs"], 1, "derivation inputs")[0], {"id", "role"}, "derivation input")
            obj(seq(body["outputs"], 1, "derivation outputs")[0], {"id", "role"}, "derivation output")
        elif name == "artifact_non_admissible":
            require(body == {"role": "spec.vector-only", "schema": "m4d-artifact-vector-1"} and row["production_admissible"] is False, "artifact non-admission drifted")
        require(row["domain_ascii_nul"] == domain and row["length_encoding"] == "u64-big-endian", f"{name} framing drifted")
        body_bytes = canonical(body)
        require(uint(row["body_bytes"], f"{name} body length") == len(body_bytes), f"{name} body length drifted")
        check_hash(body_bytes, row["body_sha256"], f"{name} body digest")
        framed_hash = check_hash(domain.encode("ascii") + be64(len(body_bytes)) + body_bytes, row["expected_sha256"], f"{name} framed digest")
        require(row["typed_id"] == prefix + framed_hash, f"{name} typed id drifted")


def verify(document):
    document = obj(document, {"schema", "schema_version", "status", "purpose", "scientific", "merkle", "packed_index", "manifest", "framed_identities"}, "document")
    require(document["schema"] == "mirante4d-wp10a-c-hand-vectors" and uint(document["schema_version"], "schema version") == 1, "schema drifted")
    require(document["status"] == "static-unpromoted", "status drifted")
    require(document["purpose"] == "Critical hand vectors only; not a fixture generator, corpus promotion, or product claim.", "purpose drifted")
    verify_scientific(document["scientific"])
    verify_merkle(document["merkle"])
    verify_packed(document["packed_index"])
    verify_manifest(document["manifest"])
    verify_framed(document["framed_identities"])


def main():
    require(len(sys.argv) == 3 and sys.argv[1] == "--vectors", "usage: verify_hand_vectors.py --vectors PATH")
    vectors = Path(sys.argv[2])
    require(vectors.is_file() and not vectors.is_symlink(), "vector authority is absent or symlinked")
    document = json.loads(
        vectors.read_text(encoding="utf-8"),
        object_pairs_hook=unique_object,
        parse_float=reject_number,
        parse_constant=reject_number,
    )
    verify(document)
    print("WP-10A-C static hand vectors: PASS")


if __name__ == "__main__":
    try:
        main()
    except (OSError, UnicodeError, json.JSONDecodeError, VectorError, ValueError, struct.error) as error:
        print(f"hand-vector verification failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
