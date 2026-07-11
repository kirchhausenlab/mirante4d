#!/usr/bin/env python3
"""SRC-PRODUCER-001: emit deterministic TIFF bytes, and no facts."""

from __future__ import annotations

import argparse
import csv
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
    if len(files) != 16 or any(row["kind"] not in {"family", "file"} for row in rows):
        raise ValueError("v1 specification must contain four families and sixteen files")
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
            else:
                raise ValueError(f"unsupported value rule {rule!r}")
            values[y, x] = value
    return values


def write_tiff(row: dict[str, str], destination: Path, ome_xml: str) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
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
