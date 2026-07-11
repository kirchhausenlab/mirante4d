#!/usr/bin/env python3
"""Fail closed on non-public paths, text, binary files, or Git topology."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess


TEXT_FORBIDDEN = [
    re.compile(rb"/home/(?!user/)[A-Za-z0-9._-]+/"),
    re.compile(rb"/Users/(?!user/)[A-Za-z0-9._-]+/"),
    re.compile(rb"[A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@"),
    re.compile(b"foundation-" + b"handoff-ready"),
    re.compile(b"mirante4d-" + b"private-archive"),
]
ARCHIVE_SUFFIXES = (".tar", ".tar.gz", ".tgz", ".zip", ".7z")
PUBLIC_NAME = "Mirante4D Contributors"
PUBLIC_EMAIL = "contributors@mirante4d.invalid"
ROOT_MESSAGE = b"Initial public Mirante4D source snapshot\n"


def fail(message: str) -> "NoReturn":
    raise SystemExit(message)


def git(git_dir: Path, *args: str, data: bytes | None = None) -> bytes:
    completed = subprocess.run(
        ["git", f"--git-dir={git_dir}", *args],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        fail(completed.stderr.decode("utf-8", errors="replace"))
    return completed.stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--git-dir", type=Path, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--source-date-epoch", type=int, required=True)
    args = parser.parse_args()
    git_dir = args.git_dir.resolve()
    policy = json.loads(args.policy.read_text(encoding="utf-8"))
    allowed_executable = set(policy["allowed_executable_paths"])
    allowed_binary = set(policy["allowed_binary_paths"])
    allowed_archives = set(policy["allowed_archive_paths"])
    excluded = set(policy["excluded_paths"])

    commit = git(git_dir, "rev-parse", "--verify", "refs/heads/main^{commit}").decode().strip()
    tag = git(
        git_dir, "rev-parse", "--verify", "refs/tags/foundation-public-root-v1"
    ).decode().strip()
    if tag != commit:
        fail("public-root tag does not directly target main root commit")
    parents = git(git_dir, "rev-list", "--parents", "-n", "1", commit).decode().split()
    if parents != [commit]:
        fail("public root commit must have no parent")

    tree = git(git_dir, "rev-parse", f"{commit}^{{tree}}").decode().strip()
    expected_commit = (
        f"tree {tree}\n"
        f"author {PUBLIC_NAME} <{PUBLIC_EMAIL}> {args.source_date_epoch} +0000\n"
        f"committer {PUBLIC_NAME} <{PUBLIC_EMAIL}> {args.source_date_epoch} +0000\n"
        "\n"
    ).encode("utf-8") + ROOT_MESSAGE
    if git(git_dir, "cat-file", "commit", commit) != expected_commit:
        fail("public root commit identity, timestamp, message, or tree is not exact")

    refs = {
        tuple(line.split("\0"))
        for line in git(
            git_dir,
            "for-each-ref",
            "--format=%(refname)%00%(objecttype)%00%(objectname)",
        )
        .decode("utf-8")
        .splitlines()
        if line
    }
    expected_refs = {
        ("refs/heads/main", "commit", commit),
        ("refs/tags/foundation-public-root-v1", "commit", commit),
    }
    if refs != expected_refs:
        fail(f"public root has unexpected refs: {sorted(refs ^ expected_refs)}")

    raw = git(git_dir, "ls-tree", "-r", "-z", "--full-tree", commit)
    paths: set[str] = set()
    for item in raw.split(b"\0"):
        if not item:
            continue
        metadata, raw_path = item.split(b"\t", 1)
        path = raw_path.decode("utf-8", errors="strict")
        mode, kind, oid = metadata.decode("ascii").split(" ")
        expected_mode = "100755" if path in allowed_executable else "100644"
        if kind != "blob" or mode != expected_mode:
            fail(
                f"unsupported Git entry {mode} {kind} {path}; "
                f"expected blob {expected_mode}"
            )
        if path in paths or path in excluded:
            fail(f"duplicate or excluded path present: {path}")
        paths.add(path)
        if path.endswith(ARCHIVE_SUFFIXES) and path not in allowed_archives:
            fail(f"unapproved archive: {path}")
        content = git(git_dir, "cat-file", "blob", oid)
        try:
            content.decode("utf-8", errors="strict")
            is_binary = b"\0" in content
        except UnicodeDecodeError:
            is_binary = True
        if is_binary and path not in allowed_binary:
            fail(f"unapproved binary file: {path}")
        if not is_binary:
            for pattern in TEXT_FORBIDDEN:
                if pattern.search(content):
                    fail(f"non-public text pattern in {path}: {pattern.pattern!r}")

    missing_executable = allowed_executable - paths
    missing_binary = allowed_binary - paths
    missing_archive = allowed_archives - paths
    if missing_executable or missing_binary or missing_archive:
        fail(
            f"path policy names missing public files: "
            f"executable={sorted(missing_executable)} "
            f"binary={sorted(missing_binary)} archive={sorted(missing_archive)}"
        )
    print(json.dumps({"commit": commit, "paths": len(paths), "result": "passed"}))


if __name__ == "__main__":
    main()
