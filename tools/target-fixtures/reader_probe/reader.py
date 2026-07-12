#!/usr/bin/env python3
"""Observe the diagnostic array through the pinned external reader only."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import zarr


EXPECTED = np.asarray(
    [[0, 1, 2, 3], [4, 5, 6, 7], [8, 9, 10, 11], [12, 13, 14, 255]],
    dtype=np.uint8,
)


def observe(store: Path) -> dict[str, object]:
    if zarr.__version__ != "3.2.1" or np.__version__ != "2.5.1":
        raise ValueError("external reader environment does not match the accepted lock")
    array = zarr.open_array(store, mode="r")
    observed = np.asarray(array[:])[0, 0, 0]
    np.testing.assert_array_equal(observed, EXPECTED)
    facts = {
        "zarr_version": zarr.__version__,
        "numpy_version": np.__version__,
        "shape": list(array.shape),
        "chunks": list(array.chunks),
        "shards": list(array.shards) if array.shards is not None else None,
        "dtype": str(array.dtype),
        "observed": observed.tolist(),
        "result": "PASS",
    }
    expected_facts = {
        "shape": [1, 1, 1, 4, 4],
        "chunks": [1, 1, 1, 256, 256],
        "shards": [1, 1, 1, 1024, 1024],
        "dtype": "uint8",
    }
    for key, expected in expected_facts.items():
        if facts[key] != expected:
            raise ValueError(f"external reader reported unexpected {key}: {facts[key]!r}")
    return facts


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("store", type=Path)
    arguments = parser.parse_args()
    print(json.dumps(observe(arguments.store), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
