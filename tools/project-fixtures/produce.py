#!/usr/bin/env python3
"""Independent deterministic producer for the WP-10B project-store fixture."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import struct
import tarfile
import uuid


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUTPUT = ROOT / "target/mirante4d/fixture-candidates/project-store-v1/candidate"
FINAL_ARCHIVE = "fixtures/project/project-store-v1.tar.gz"
CONTRACT_PATH = "architecture/wp10b-project-store-contract.json"
PAGE_BYTES = 16 * 1024 * 1024
REF_MAGIC = b"M4DREF1\0"
REF_DOMAIN = b"M4D-PROJECT-REF-V1\0"
GENERATION_DOMAIN = b"M4D-PROJECT-GENERATION-V1\0"
GENERATION_PREFIX = "m4d-project-generation-v1-sha256:"

SCIENCE_ID = "m4d-sc-v1-sha256:" + "13" * 32
PROJECT_A = "11111111-2222-4333-8444-555555555555"
PROJECT_B = "66666666-7777-4888-8999-aaaaaaaaaaaa"
PROJECT_C = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff"
PROJECT_D = "12345678-9abc-4def-8123-456789abcdef"
ANNOTATION_HANDLE = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
TABLE_HANDLE = "01234567-89ab-4cde-8fab-0123456789ab"


def canonical_json(value: object) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    ).encode("utf-8")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def exact_digest(data: bytes) -> str:
    return "sha256:" + sha256(data)


def f32(value: float) -> str:
    return struct.pack(">f", value).hex()


def f64(value: float) -> str:
    return struct.pack(">d", value).hex()


def generation_id(encoded: bytes) -> str:
    preimage = GENERATION_DOMAIN + len(encoded).to_bytes(8, "big") + encoded
    return GENERATION_PREFIX + sha256(preimage)


def generation_path(generation: str) -> str:
    digest = generation.removeprefix(GENERATION_PREFIX)
    return f"generations/sha256/{digest[:2]}/{digest[2:]}.json"


def object_path(digest: str) -> str:
    raw = digest.removeprefix("sha256:")
    return f"objects/sha256/{raw[:2]}/{raw[2:]}"


def raw_descriptor(data: bytes, media_type: str, role: str) -> dict[str, object]:
    return {
        "byte_length": str(len(data)),
        "digest": exact_digest(data),
        "media_type": media_type,
        "role": role,
    }


def physical_descriptor(data: bytes) -> dict[str, str]:
    return {"byte_length": str(len(data)), "digest": exact_digest(data)}


def pack_ref(
    kind: int,
    project_id: str,
    current: str,
    previous: str | None = None,
    base: str | None = None,
) -> bytes:
    presence = (1 if previous is not None else 0) | (2 if base is not None else 0)
    header = REF_MAGIC + (1).to_bytes(2, "big") + bytes([kind, presence]) + (160).to_bytes(4, "big")

    def raw(value: str | None) -> bytes:
        return bytes(32) if value is None else bytes.fromhex(value.removeprefix(GENERATION_PREFIX))

    body = header + uuid.UUID(project_id).bytes + raw(current) + raw(previous) + raw(base)
    assert len(body) == 128
    return body + hashlib.sha256(REF_DOMAIN + body).digest()


def transfer(color: tuple[float, float, float], gamma: float | None, invert: bool = False) -> dict[str, object]:
    curve: dict[str, object]
    if gamma is None:
        curve = {"kind": "linear"}
    else:
        curve = {"kind": "gamma", "value": f32(gamma)}
    return {
        "color_rgb": [f32(value) for value in color],
        "curve": curve,
        "invert": invert,
        "opacity": f32(0.75),
        "window": {"high": f32(4095.0), "low": f32(-0.0)},
    }


def render(mode: str, ordinal: int) -> dict[str, object]:
    sampling = "voxel_exact" if ordinal % 2 else "smooth_linear"
    if mode == "mip":
        return {"mode": "mip", "sampling": sampling}
    if mode == "isosurface":
        return {
            "display_level": f32(0.5),
            "mode": "isosurface",
            "sampling": sampling,
            "shading": "flat",
        }
    return {
        "density_scale": f64(1.25),
        "mode": "dvr",
        "opacity_transfer": {
            "curve": {"kind": "gamma", "value": f32(0.75)},
            "window": {"high": f32(1.0), "low": f32(0.0)},
        },
        "sampling": sampling,
    }


def view_state(revision: int, modes: tuple[str, str]) -> dict[str, object]:
    quaternion = [
        "bfcbc80719f38873",
        "3fe7c0dc6e9a5f8d",
        "bfe3eb28a469d27b",
        "3fbec418bb9ad9b8",
    ]
    layers = []
    entries = []
    for ordinal, mode in enumerate(modes):
        layer = {
            "layer": ordinal,
            "render": render(mode, ordinal),
            "transfer": transfer((1.0 - ordinal * 0.5, ordinal * 0.5, 0.25), None if ordinal == 0 else 1.5),
            "visible": True,
        }
        layers.append(layer)
        entries.append({key: value for key, value in layer.items()})
    return {
        "channel_presets": [
            {"entries": entries, "id": "primary", "label": "Cafe\u0301 preset"}
        ],
        "view": {
            "active_layer": revision % 2,
            "camera": {
                "orientation_xyzw": quaternion,
                "orthographic_world_per_screen_point": f64(0.25),
                "perspective_focal_length_screen_points": f64(320.0),
                "perspective_view_distance_world": f64(40.0),
                "projection": "orthographic" if revision % 2 == 0 else "perspective",
                "target": [f64(1.0), f64(2.0), f64(3.0)],
            },
            "cross_section": {
                "center": [f64(4.0), f64(5.0), f64(6.0)],
                "depth_world": f64(2.0),
                "orientation_xyzw": quaternion,
                "scale_world_per_screen_point": f64(0.5),
            },
            "iso_light": {"kind": "detached_screen", "x": f32(0.25), "y": f32(-0.5)},
            "layers": layers,
            "layout": "four_panel" if revision % 2 else "single3d",
            "timepoint": str(revision),
        },
    }


def annotation_payload() -> bytes:
    return canonical_json({"kind": "annotation", "text": "independent fixture"})


def table_payload() -> bytes:
    pattern = b"0123456789abcdef"
    total = PAGE_BYTES + 4096
    return (pattern * ((total + len(pattern) - 1) // len(pattern)))[:total]


def make_artifacts(files: dict[str, bytes], include_table: bool) -> tuple[list[dict[str, object]], list[dict[str, str]]]:
    note = annotation_payload()
    note_logical = raw_descriptor(
        note,
        "application/vnd.mirante4d.annotation-v1+json",
        "artifact.annotation.v1",
    )
    files[object_path(str(note_logical["digest"]))] = note
    artifacts: list[dict[str, object]] = [
        {
            "completeness": "complete",
            "content_id": "m4d-artifact-v1-sha256:" + "21" * 32,
            "derivation_id": None,
            "handle_id": ANNOTATION_HANDLE,
            "label": "Independent note",
            "logical_object": note_logical,
            "recipe_id": None,
            "recoverability": "non_regenerable",
            "schema": "annotation.v1",
            "source_layers": [0],
            "storage": {
                "kind": "direct",
                "object": physical_descriptor(note),
            },
            "visible": True,
        }
    ]
    closure = [physical_descriptor(note)]
    if not include_table:
        return artifacts, closure

    payload = table_payload()
    logical = raw_descriptor(
        payload,
        "application/vnd.mirante4d.analysis-table-v1+json",
        "artifact.analysis-table.v1",
    )
    pages = []
    page_descriptors = []
    for ordinal, offset in enumerate(range(0, len(payload), PAGE_BYTES)):
        page = payload[offset : offset + PAGE_BYTES]
        descriptor = physical_descriptor(page)
        files[object_path(descriptor["digest"])] = page
        pages.append(
            {
                "byte_length": descriptor["byte_length"],
                "digest": descriptor["digest"],
                "offset": str(offset),
                "ordinal": ordinal,
            }
        )
        page_descriptors.append(descriptor)
    binding = canonical_json(
        {
            "logical_descriptor": logical,
            "pages": pages,
            "schema": "mirante4d-project-logical-object-binding",
            "schema_version": 1,
        }
    )
    binding_descriptor = raw_descriptor(
        binding,
        "application/vnd.mirante4d.project-object-binding-v1+json",
        "project.object-binding.v1",
    )
    files[object_path(str(binding_descriptor["digest"]))] = binding
    artifacts.append(
        {
            "completeness": "complete",
            "content_id": "m4d-artifact-v1-sha256:" + "34" * 32,
            "derivation_id": "m4d-derivation-record-v1-sha256:" + "55" * 32,
            "handle_id": TABLE_HANDLE,
            "label": "Paged table",
            "logical_object": logical,
            "recipe_id": "m4d-recipe-v1-sha256:" + "89" * 32,
            "recoverability": "regenerable",
            "schema": "analysis-table.v1",
            "source_layers": [0, 1],
            "storage": {"binding_manifest": binding_descriptor, "kind": "paged"},
            "visible": True,
        }
    )
    closure.extend([physical_descriptor(binding), *page_descriptors])
    closure.sort(key=lambda row: (row["digest"], int(row["byte_length"])))
    return artifacts, closure


def dataset(package_byte: str, locator: str) -> dict[str, object]:
    return {
        "locator_hint": locator,
        "package_id": "m4d-package-v1-sha256:" + package_byte * 64,
        "release_id": None,
        "scientific_content_id": SCIENCE_ID,
    }


def add_generation(
    files: dict[str, bytes],
    *,
    project_id: str,
    kind: str,
    sequence: int,
    revision: int,
    parent: str | None,
    base: str | None,
    forked_from: dict[str, str] | None,
    include_table: bool,
    package_byte: str,
    locator: str,
    modes: tuple[str, str],
) -> str:
    artifacts, closure = make_artifacts(files, include_table)
    generation = {
        "artifacts": artifacts,
        "base_manual_generation_id": base,
        "dataset": dataset(package_byte, locator),
        "forked_from": forked_from,
        "generation_kind": kind,
        "generation_sequence": str(sequence),
        "parent_generation_id": parent,
        "project_id": project_id,
        "reachable_objects": closure,
        "revision_high_water_sequence": str(revision),
        "revision_sequence": str(revision),
        "schema": "mirante4d-project-generation",
        "schema_version": 1,
        "state": view_state(revision, modes),
    }
    encoded = canonical_json(generation)
    identity = generation_id(encoded)
    files[generation_path(identity)] = encoded
    return identity


def envelope(project_id: str) -> bytes:
    return canonical_json(
        {
            "profile": "mirante4d-project-store-v1",
            "project_id": project_id,
            "schema": "mirante4d-project-store-envelope",
            "schema_version": 1,
        }
    )


def build_recoverable() -> tuple[dict[str, bytes], dict[str, object]]:
    files: dict[str, bytes] = {"project.json": envelope(PROJECT_A)}
    g1 = add_generation(
        files,
        project_id=PROJECT_A,
        kind="manual",
        sequence=0,
        revision=1,
        parent=None,
        base=None,
        forked_from=None,
        include_table=False,
        package_byte="a",
        locator="./data/Cafe\u0301-a.m4d",
        modes=("mip", "isosurface"),
    )
    g2 = add_generation(
        files,
        project_id=PROJECT_A,
        kind="manual",
        sequence=1,
        revision=2,
        parent=g1,
        base=None,
        forked_from=None,
        include_table=True,
        package_byte="b",
        locator="../relocated/Cafe\u0301-b.m4d",
        modes=("mip", "dvr"),
    )
    a1 = add_generation(
        files,
        project_id=PROJECT_A,
        kind="autosave",
        sequence=2,
        revision=3,
        parent=None,
        base=g2,
        forked_from=None,
        include_table=True,
        package_byte="b",
        locator="../relocated/Cafe\u0301-b.m4d",
        modes=("isosurface", "dvr"),
    )
    a2 = add_generation(
        files,
        project_id=PROJECT_A,
        kind="autosave",
        sequence=3,
        revision=4,
        parent=a1,
        base=g2,
        forked_from=None,
        include_table=True,
        package_byte="b",
        locator="../relocated/Cafe\u0301-b.m4d",
        modes=("dvr", "mip"),
    )
    orphan = add_generation(
        files,
        project_id=PROJECT_A,
        kind="manual",
        sequence=4,
        revision=5,
        parent=g2,
        base=None,
        forked_from=None,
        include_table=True,
        package_byte="b",
        locator="../relocated/Cafe\u0301-b.m4d",
        modes=("dvr", "isosurface"),
    )
    files.update(
        {
            "refs/head": pack_ref(1, PROJECT_A, g2, g1),
            "refs/recovery": pack_ref(2, PROJECT_A, g1),
            "refs/autosave-head": pack_ref(3, PROJECT_A, a2, a1, g2),
            "refs/autosave-recovery": pack_ref(4, PROJECT_A, a1),
            "refs/pins/checkpoint-a": pack_ref(5, PROJECT_A, g1),
        }
    )
    facts = {
        "autosave": "newer",
        "autosave_head": a2,
        "autosave_head_previous": a1,
        "autosave_recovery": a1,
        "head": g2,
        "head_previous": g1,
        "manual_recovery": g1,
        "project_id": PROJECT_A,
        "recovery_candidates": [orphan],
        "roots": sorted({g1, g2, a1, a2}),
    }
    return files, facts


def build_divergent(source_generation: str) -> tuple[dict[str, bytes], dict[str, object]]:
    files: dict[str, bytes] = {"project.json": envelope(PROJECT_B)}
    provenance = {"generation_id": source_generation, "project_id": PROJECT_A}
    common = dict(
        files=files,
        project_id=PROJECT_B,
        forked_from=provenance,
        include_table=False,
        package_byte="c",
        locator="./forked/data.m4d",
    )
    g1 = add_generation(kind="manual", sequence=0, revision=1, parent=None, base=None, modes=("mip", "dvr"), **common)
    g2 = add_generation(kind="manual", sequence=1, revision=2, parent=g1, base=None, modes=("isosurface", "dvr"), **common)
    g3 = add_generation(kind="manual", sequence=2, revision=3, parent=g2, base=None, modes=("dvr", "mip"), **common)
    a1 = add_generation(kind="autosave", sequence=3, revision=4, parent=None, base=g2, modes=("mip", "isosurface"), **common)
    files.update(
        {
            "refs/head": pack_ref(1, PROJECT_B, g3, g2),
            "refs/recovery": pack_ref(2, PROJECT_B, g2),
            "refs/autosave-head": pack_ref(3, PROJECT_B, a1, None, g2),
        }
    )
    facts = {
        "autosave": "divergent",
        "autosave_head": a1,
        "autosave_head_previous": None,
        "autosave_recovery": None,
        "head": g3,
        "head_previous": g2,
        "manual_recovery": g2,
        "project_id": PROJECT_B,
        "recovery_candidates": [g1],
        "roots": sorted({g2, g3, a1}),
    }
    return files, facts


def build_stale() -> tuple[dict[str, bytes], dict[str, object]]:
    files: dict[str, bytes] = {"project.json": envelope(PROJECT_C)}
    common = dict(
        files=files,
        project_id=PROJECT_C,
        forked_from=None,
        include_table=False,
        package_byte="d",
        locator="./stale/data.m4d",
        modes=("mip", "dvr"),
    )
    manual = add_generation(kind="manual", sequence=0, revision=5, parent=None, base=None, **common)
    autosave = add_generation(kind="autosave", sequence=1, revision=5, parent=None, base=manual, **common)
    files.update(
        {
            "refs/head": pack_ref(1, PROJECT_C, manual),
            "refs/autosave-head": pack_ref(3, PROJECT_C, autosave, None, manual),
        }
    )
    return files, {
        "autosave": "stale",
        "autosave_head": autosave,
        "autosave_head_previous": None,
        "autosave_recovery": None,
        "head": manual,
        "head_previous": None,
        "manual_recovery": None,
        "project_id": PROJECT_C,
        "recovery_candidates": [],
        "roots": sorted({manual, autosave}),
    }


def build_provisional() -> tuple[dict[str, bytes], dict[str, object]]:
    files: dict[str, bytes] = {"project.json": envelope(PROJECT_D)}
    autosave = add_generation(
        files,
        project_id=PROJECT_D,
        kind="autosave",
        sequence=0,
        revision=1,
        parent=None,
        base=None,
        forked_from=None,
        include_table=False,
        package_byte="e",
        locator="./provisional/data.m4d",
        modes=("isosurface", "dvr"),
    )
    files["refs/autosave-head"] = pack_ref(3, PROJECT_D, autosave)
    return files, {
        "autosave": "provisional",
        "autosave_head": autosave,
        "autosave_head_previous": None,
        "autosave_recovery": None,
        "head": None,
        "head_previous": None,
        "manual_recovery": None,
        "project_id": PROJECT_D,
        "recovery_candidates": [],
        "roots": [autosave],
    }


def prefixed(root: str, files: dict[str, bytes]) -> dict[str, bytes]:
    return {f"{root}/{path}": data for path, data in files.items()}


def archive_bytes(files: dict[str, bytes]) -> bytes:
    directories: set[str] = set()
    for path in files:
        for parent in PurePosixPath(path).parents:
            if parent == PurePosixPath("."):
                break
            directories.add(parent.as_posix())
    raw = io.BytesIO()
    with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0, compresslevel=9) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for directory in sorted(directories, key=lambda value: (len(PurePosixPath(value).parts), value)):
                info = tarfile.TarInfo(directory + "/")
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                info.mtime = 0
                archive.addfile(info)
            for path in sorted(files):
                data = files[path]
                info = tarfile.TarInfo(path)
                info.size = len(data)
                info.mode = 0o644
                info.mtime = 0
                archive.addfile(info, io.BytesIO(data))
    return raw.getvalue()


def source_binding(relative: str) -> dict[str, str]:
    path = ROOT / relative
    if not path.is_file():
        raise RuntimeError(f"required independent tool is missing: {relative}")
    return {"path": relative, "sha256": sha256(path.read_bytes())}


def tree_digest(files: dict[str, bytes]) -> str:
    digest = hashlib.sha256()
    for path, data in sorted(files.items()):
        digest.update(path.encode("ascii") + b"\0" + bytes.fromhex(sha256(data)) + b"\0")
        digest.update(str(len(data)).encode("ascii") + b"\n")
    return digest.hexdigest()


def produce(output: Path) -> dict[str, object]:
    if output.exists():
        raise RuntimeError(f"output already exists: {output}")
    recoverable, recoverable_facts = build_recoverable()
    divergent, divergent_facts = build_divergent(str(recoverable_facts["head"]))
    stale, stale_facts = build_stale()
    provisional, provisional_facts = build_provisional()
    files = {
        **prefixed("recoverable.m4dproj", recoverable),
        **prefixed("divergent.m4dproj", divergent),
        **prefixed("stale.m4dproj", stale),
        **prefixed("provisional.m4dproj", provisional),
    }
    encoded_archive = archive_bytes(files)
    archive_sha = sha256(encoded_archive)
    mutations = [
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
    regular_bytes = sum(map(len, files.values()))
    manifest: dict[str, object] = {
        "approvals": {
            "repository_owner": {
                "approved_by": "Mirante4D repository owner",
                "approved_on": "2026-07-12",
                "reference": "WP10B-PREAUTH-2026-07-12",
                "state": "approved",
            }
        },
        "archive": {
            "archive_bytes": len(encoded_archive),
            "compression": "gzip",
            "file_count": len(files),
            "max_file_bytes": max(map(len, files.values())),
            "path": FINAL_ARCHIVE,
            "regular_file_bytes": regular_bytes,
            "sha256": archive_sha,
            "tree_sha256": tree_digest(files),
        },
        "contract": {
            "path": CONTRACT_PATH,
            "schema": "mirante4d-wp10b-project-store-contract",
            "schema_version": 1,
        },
        "fixture_id": "project-store-v1",
        "limits": {
            "archive_bytes_max": 4 * 1024 * 1024,
            "files_max": 128,
            "path_bytes_max": 240,
            "regular_file_bytes_max": 32 * 1024 * 1024,
        },
        "lineages": {
            "producer": source_binding("tools/project-fixtures/produce.py"),
            "reproducer": source_binding("tools/project-fixtures/reproduce.py"),
            "validator": source_binding("tools/project-fixtures/validate.py"),
        },
        "mutations": [
            {"expected_fault": fault, "id": mutation, "store": "recoverable.m4dproj"}
            for mutation, fault in mutations
        ],
        "schema": "mirante4d-foundation-project-fixture-manifest",
        "schema_version": 1,
        "status": "independently_validated",
        "stores": [
            {"root": "recoverable.m4dproj", **recoverable_facts},
            {"root": "divergent.m4dproj", **divergent_facts},
            {"root": "stale.m4dproj", **stale_facts},
            {"root": "provisional.m4dproj", **provisional_facts},
        ],
    }
    output.mkdir(parents=True)
    (output / "project-store-v1.tar.gz").write_bytes(encoded_archive)
    (output / "manifest.json").write_bytes(canonical_json(manifest) + b"\n")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    manifest = produce(args.output.resolve())
    print(
        json.dumps(
            {
                "archive_bytes": manifest["archive"]["archive_bytes"],
                "archive_sha256": manifest["archive"]["sha256"],
                "output": str(args.output),
                "result": "produced",
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
