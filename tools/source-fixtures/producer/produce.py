#!/usr/bin/env python3
"""SRC-PRODUCER-001: emit deterministic TIFF bytes, and no facts."""

from __future__ import annotations

import argparse
import csv
import struct
import zlib
from pathlib import Path

import numpy as np
import tifffile


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

FINITE_F32_BITS = [
    0xBFC00000,
    0x00000000,
    0x3E800000,
    0x3F800000,
    0x40000000,
    0x40400000,
    0x41200000,
    0x41380000,
    0x41440000,
    0x41500000,
    0x41680000,
    0x417C0000,
]
NONFINITE_F32_BITS = [
    0x00000000,
    0x80000000,
    0x3F800000,
    0x7FC00000,
    0x7F800000,
    0xFF800000,
]


def parse_spec(path: Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as source:
        reader = csv.DictReader(source, delimiter="|")
        if reader.fieldnames != EXPECTED_HEADER:
            raise ValueError(f"unexpected specification header: {reader.fieldnames!r}")
        rows = list(reader)
    files = [row for row in rows if row["kind"] == "file"]
    if len(files) != 21 or any(row["kind"] not in {"family", "file"} for row in rows):
        raise ValueError("v1 specification must contain five families and twenty-one files")
    if len({row["path"] for row in files}) != len(files):
        raise ValueError("v1 specification contains duplicate file paths")
    return files


def page_values(row: dict[str, str], page: int) -> np.ndarray:
    height = int(row["height"])
    width = int(row["width"])
    t = int(row["t"])
    c = int(row["c"])
    z = int(row["z_start"]) + page
    rule = row["value_rule"]

    if rule == "spec004_f32_finite":
        values = np.asarray(FINITE_F32_BITS, dtype="<u4").view("<f4")
        return values.reshape(int(row["pages"]), height, width)[page]
    if rule == "spec004_f32_nonfinite":
        values = np.asarray(NONFINITE_F32_BITS, dtype="<u4").view("<f4")
        return values.reshape(1, height, width)[0]

    dtype = {
        "u8": np.dtype("u1"),
        "u16": np.dtype("<u2"),
        "u32": np.dtype("<u4"),
    }.get(row["dtype"])
    if dtype is None:
        raise ValueError(f"unsupported producer dtype {row['dtype']!r}")

    values = np.empty((height, width), dtype=dtype)
    for y in range(height):
        for x in range(width):
            if rule == "spec001_u16":
                value = 10 * z + 3 * y + x
            elif rule == "spec002_u16":
                value = 100 * t + 20 * z + 4 * y + x
            elif rule == "spec003_u8":
                value = 100 * c + 20 * z + 4 * y + x
            elif rule == "spec004_u8_no_data":
                value = 255 if (z, y, x) == (0, 0, 0) else 9 * z + 3 * y + x
            elif rule == "spec004_u16_striped":
                value = 100 * z + 10 * y + x
            elif rule == "spec004_u16_zero":
                value = 0
            elif rule == "spec004_u32_sequence":
                value = y * width + x
            elif rule == "spec005_u16_portable":
                value = 100 * y + x
            elif rule == "spec005_u8_portable":
                value = (17 * y + 3 * x) % 251
            else:
                raise ValueError(f"unsupported value rule {rule!r}")
            values[y, x] = value
    return values


def packbits_encode(payload: bytes) -> bytes:
    """Encode deterministic literal-only PackBits packets."""
    encoded = bytearray()
    for start in range(0, len(payload), 128):
        chunk = payload[start : start + 128]
        encoded.append(len(chunk) - 1)
        encoded.extend(chunk)
    return bytes(encoded)


def lzw_encode(payload: bytes) -> bytes:
    """Emit valid TIFF LZW using bounded 9-bit literal runs.

    A clear code every 200 literals keeps the dictionary below the first code
    width transition, making this tiny fixture encoder deliberately simple and
    independent from both tifffile's optional codecs and the production TIFF
    implementation.
    """
    codes: list[int] = []
    for start in range(0, len(payload), 200):
        codes.append(256)
        codes.extend(payload[start : start + 200])
    codes.append(257)
    encoded = bytearray()
    accumulator = 0
    bits = 0
    for code in codes:
        accumulator = (accumulator << 9) | code
        bits += 9
        while bits >= 8:
            bits -= 8
            encoded.append((accumulator >> bits) & 0xFF)
    if bits:
        encoded.append((accumulator << (8 - bits)) & 0xFF)
    return bytes(encoded)


def portable_container(row: dict[str, str]) -> tuple[str, int, int, str]:
    path = row["path"]
    if path.endswith("uncompressed-big-endian-u16.tif"):
        return ">", 42, 1, "strips"
    if path.endswith("lzw-u8-striped.tif"):
        return "<", 42, 5, "strips"
    if path.endswith("deflate-u8-tiled.tif"):
        return "<", 42, 8, "tiles"
    if path.endswith("old-deflate-u8-striped.tif"):
        return "<", 42, 32946, "strips"
    if path.endswith("packbits-u8-bigtiff.tif"):
        return "<", 43, 32773, "strips"
    raise ValueError(f"unknown portable container fixture {path!r}")


def encode_portable_tiff(row: dict[str, str]) -> bytes:
    endian, version, compression, layout = portable_container(row)
    logical = page_values(row, 0)
    if row["dtype"] == "u16":
        payload = b"".join(
            struct.pack(f"{endian}H", int(value)) for value in logical.flat
        )
        bits_per_sample = 16
    else:
        payload = bytes(int(value) for value in logical.flat)
        bits_per_sample = 8

    if compression == 1:
        encoded_payload = payload
    elif compression == 5:
        encoded_payload = lzw_encode(payload)
    elif compression in {8, 32946}:
        encoded_payload = zlib.compress(payload, level=9)
    elif compression == 32773:
        encoded_payload = packbits_encode(payload)
    else:
        raise AssertionError(compression)

    width = int(row["width"])
    height = int(row["height"])
    tags: list[tuple[int, int, int, int]] = [
        (256, 4, 1, width),
        (257, 4, 1, height),
        (258, 3, 1, bits_per_sample),
        (259, 3, 1, compression),
        (262, 3, 1, 1),
        (277, 3, 1, 1),
        (284, 3, 1, 1),
        (339, 3, 1, 1),
    ]
    if layout == "tiles":
        tags.extend([(322, 4, 1, 16), (323, 4, 1, 16)])
        offset_tag, byte_count_tag = 324, 325
    else:
        tags.append((278, 4, 1, height))
        offset_tag, byte_count_tag = 273, 279
    tags.extend([(offset_tag, 16 if version == 43 else 4, 1, 0), (byte_count_tag, 16 if version == 43 else 4, 1, len(encoded_payload))])
    tags.sort()

    marker = b"II" if endian == "<" else b"MM"
    if version == 42:
        data_offset = 8 + 2 + 12 * len(tags) + 4
        tags = [
            (tag, kind, count, data_offset if tag == offset_tag else value)
            for tag, kind, count, value in tags
        ]
        output = bytearray(marker + struct.pack(f"{endian}HI", 42, 8))
        output.extend(struct.pack(f"{endian}H", len(tags)))
        for tag, kind, count, value in tags:
            output.extend(struct.pack(f"{endian}HHI", tag, kind, count))
            if kind == 3:
                output.extend(struct.pack(f"{endian}H", value) + b"\0\0")
            elif kind == 4:
                output.extend(struct.pack(f"{endian}I", value))
            else:
                raise AssertionError(kind)
        output.extend(struct.pack(f"{endian}I", 0))
    else:
        data_offset = 16 + 8 + 20 * len(tags) + 8
        tags = [
            (tag, kind, count, data_offset if tag == offset_tag else value)
            for tag, kind, count, value in tags
        ]
        output = bytearray(marker + struct.pack(f"{endian}HHH", 43, 8, 0) + struct.pack(f"{endian}Q", 16))
        output.extend(struct.pack(f"{endian}Q", len(tags)))
        for tag, kind, count, value in tags:
            output.extend(struct.pack(f"{endian}HHQ", tag, kind, count))
            if kind == 3:
                output.extend(struct.pack(f"{endian}H", value) + b"\0" * 6)
            elif kind == 4:
                output.extend(struct.pack(f"{endian}I", value) + b"\0" * 4)
            elif kind == 16:
                output.extend(struct.pack(f"{endian}Q", value))
            else:
                raise AssertionError(kind)
        output.extend(struct.pack(f"{endian}Q", 0))
    output.extend(encoded_payload)
    return bytes(output)


def write_tiff(row: dict[str, str], destination: Path, ome_xml: str) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if row["spec_id"] == "SRC-TIFF-SPEC-005":
        destination.write_bytes(encode_portable_tiff(row))
        return
    pages = int(row["pages"])
    rows_per_strip = (
        int(row["height"])
        if row["rows_per_strip"] == "full"
        else int(row["rows_per_strip"])
    )
    with tifffile.TiffWriter(destination, bigtiff=False, byteorder="<") as writer:
        for page in range(pages):
            description = (
                ome_xml
                if row["spec_id"] == "SRC-TIFF-SPEC-001" and page == 0
                else None
            )
            writer.write(
                page_values(row, page),
                photometric="minisblack",
                planarconfig="contig",
                compression=None,
                rowsperstrip=rows_per_strip,
                metadata=None,
                description=description,
                software=False,
                datetime=None,
                contiguous=False,
                align=2,
            )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--spec", type=Path, required=True)
    parser.add_argument("--ome-xml", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    if args.output.exists() and any(args.output.iterdir()):
        raise SystemExit(f"producer output must be empty: {args.output}")
    args.output.mkdir(parents=True, exist_ok=True)
    ome_xml = args.ome_xml.read_text(encoding="utf-8")
    if "2016-06" not in ome_xml or 'SizeZ="2"' not in ome_xml:
        raise SystemExit("approved OME-XML specification was not supplied")

    rows = parse_spec(args.spec)
    for row in sorted(rows, key=lambda item: item["path"]):
        write_tiff(row, args.output / row["path"], ome_xml)

    actual = sorted(
        path.relative_to(args.output).as_posix()
        for path in args.output.rglob("*")
        if path.is_file()
    )
    expected = sorted(row["path"] for row in rows)
    if actual != expected:
        raise SystemExit("producer emitted an unexpected path set")


if __name__ == "__main__":
    main()
