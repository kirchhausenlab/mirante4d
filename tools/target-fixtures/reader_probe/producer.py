#!/usr/bin/env python3
"""Build one tiny Zarr-v3 shard without importing an array or Mirante codec."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


ZSTD = "/usr/bin/zstd"
MISSING = (1 << 64) - 1
EXPECTED = [
    [0, 1, 2, 3],
    [4, 5, 6, 7],
    [8, 9, 10, 11],
    [12, 13, 14, 255],
]


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def crc32c(data: bytes) -> int:
    """Standalone bitwise Castagnoli CRC, independent of the reader stack."""
    value = 0xFFFFFFFF
    for byte in data:
        value ^= byte
        for _ in range(8):
            value = (value >> 1) ^ (0x82F63B78 if value & 1 else 0)
    return value ^ 0xFFFFFFFF


def append_crc32c(data: bytes) -> bytes:
    return data + struct.pack("<I", crc32c(data))


def encode_zstd(data: bytes) -> bytes:
    result = subprocess.run(
        [ZSTD, "-3", "--no-check", "--quiet", "--stdout"],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
        timeout=30,
    )
    return result.stdout


def build(destination: Path) -> dict[str, object]:
    destination.mkdir(parents=True, exist_ok=False)
    shard_path = destination / "c" / "0" / "0" / "0" / "0" / "0"
    shard_path.parent.mkdir(parents=True)

    metadata = {
        "zarr_format": 3,
        "node_type": "array",
        "shape": [1, 1, 1, 4, 4],
        "data_type": "uint8",
        "chunk_grid": {
            "name": "regular",
            "configuration": {"chunk_shape": [1, 1, 1, 1024, 1024]},
        },
        "chunk_key_encoding": {
            "name": "default",
            "configuration": {"separator": "/"},
        },
        "fill_value": 0,
        "codecs": [
            {
                "name": "sharding_indexed",
                "configuration": {
                    "chunk_shape": [1, 1, 1, 256, 256],
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
        "dimension_names": ["t", "c", "z", "y", "x"],
    }
    metadata_bytes = json.dumps(metadata, separators=(",", ":")).encode("utf-8")
    (destination / "zarr.json").write_bytes(metadata_bytes)

    raw = bytearray(256 * 256)
    for row, values in enumerate(EXPECTED):
        raw[row * 256 : row * 256 + len(values)] = bytes(values)
    encoded_inner = append_crc32c(encode_zstd(bytes(raw)))
    entries = [(0, len(encoded_inner))] + [(MISSING, MISSING)] * 15
    raw_index = b"".join(struct.pack("<QQ", offset, length) for offset, length in entries)
    shard = encoded_inner + append_crc32c(raw_index)
    shard_path.write_bytes(shard)

    tree_digest = hashlib.sha256()
    for relative, data in (("c/0/0/0/0/0", shard), ("zarr.json", metadata_bytes)):
        tree_digest.update(relative.encode("ascii"))
        tree_digest.update(b"\0")
        tree_digest.update(len(data).to_bytes(8, "little"))
        tree_digest.update(data)
    return {
        "metadata_bytes": len(metadata_bytes),
        "metadata_sha256": sha256(metadata_bytes),
        "shard_bytes": len(shard),
        "shard_sha256": sha256(shard),
        "tree_sha256": tree_digest.hexdigest(),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    print(json.dumps(build(arguments.destination), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
