#!/usr/bin/env python3
"""Validate cross-record invariants in the pre-foundation disposition."""

from __future__ import annotations

from collections import Counter
import argparse
import json
from pathlib import Path


def fail(message: str) -> "NoReturn":
    raise SystemExit(message)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    document = json.loads(args.manifest.read_text(encoding="utf-8"))
    records = document.get("records")
    post_ids = document.get("post_wp02_new_record_ids")
    if not isinstance(records, list) or not isinstance(post_ids, list):
        fail("disposition records and post-WP-02 IDs must be arrays")

    ids = [record.get("id") for record in records]
    if any(not isinstance(record_id, str) or not record_id for record_id in ids):
        fail("every disposition record must have a non-empty string ID")
    if len(ids) != len(set(ids)):
        fail("disposition record IDs are not unique")
    if len(post_ids) != len(set(post_ids)) or post_ids != sorted(post_ids):
        fail("post-WP-02 record IDs must be unique and sorted")
    overlap = set(ids).intersection(post_ids)
    if overlap:
        fail(f"post-WP-02 IDs overlap predecessor records: {sorted(overlap)}")

    counts = Counter(record.get("disposition") for record in records)
    expected_summary = {
        "record_count": len(records),
        "deleted": counts["deleted"],
        "retained": counts["retained"],
        "rewritten": counts["rewritten"],
        "moved": counts["moved"],
        "post_wp02_new_records": len(post_ids),
    }
    if document.get("summary") != expected_summary:
        fail("disposition summary does not match the record partition")

    for record in records:
        record_id = record["id"]
        disposition = record.get("disposition")
        replacement = record.get("replacement_id")
        if disposition == "deleted" and replacement is not None:
            fail(f"deleted record has a replacement: {record_id}")
        if disposition == "retained" and replacement != record_id:
            fail(f"retained record does not retain its ID: {record_id}")
        if disposition == "moved" and replacement != record_id:
            fail(f"moved record does not preserve its ID: {record_id}")
        if disposition == "rewritten" and (
            not isinstance(replacement, str) or not replacement or replacement == record_id
        ):
            fail(f"rewritten record does not name a distinct replacement: {record_id}")

    print(
        json.dumps(
            {
                "records": len(records),
                "post_wp02_new_records": len(post_ids),
                "dispositions": dict(sorted(counts.items())),
                "result": "passed",
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
