#!/usr/bin/env python3
"""Build Mirante4D's deterministic one-commit public Git root."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
from typing import Any


ROOT_MESSAGE = b"Initial public Mirante4D source snapshot\n"
PUBLIC_NAME = "Mirante4D Contributors"
PUBLIC_EMAIL = "contributors@mirante4d.invalid"
PUBLIC_TAG = "foundation-public-root-v1"
SAFE_PATH = re.compile(r"^[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*$")


def fail(message: str) -> "NoReturn":
    raise SystemExit(message)


def run(
    args: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    data: bytes | None = None,
) -> bytes:
    completed = subprocess.run(
        args,
        cwd=cwd,
        env=env,
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        command = " ".join(args)
        stderr = completed.stderr.decode("utf-8", errors="replace").strip()
        fail(f"command failed ({completed.returncode}): {command}\n{stderr}")
    return completed.stdout


def git(source: Path, *args: str, data: bytes | None = None) -> bytes:
    return run(["git", "-C", str(source), *args], data=data)


def bare_git(git_dir: Path, *args: str, env: dict[str, str], data: bytes | None = None) -> bytes:
    return run(["git", f"--git-dir={git_dir}", *args], env=env, data=data)


def canonical_environment(home: Path) -> dict[str, str]:
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "LC_ALL": "C",
        "LANG": "C",
        "TZ": "UTC",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_ATTR_NOSYSTEM": "1",
    }
    return env


def load_policy(path: Path) -> dict[str, Any]:
    policy = json.loads(path.read_text(encoding="utf-8"))
    if policy.get("schema") != "mirante4d-public-path-policy":
        fail("unexpected public path policy schema")
    if policy.get("schema_version") != 1:
        fail("unsupported public path policy version")
    return policy


def validate_path(path: str) -> None:
    if not SAFE_PATH.fullmatch(path):
        fail(f"unsafe or non-portable repository path: {path!r}")
    pure = PurePosixPath(path)
    if pure.is_absolute() or ".." in pure.parts:
        fail(f"unsafe repository path: {path!r}")
    if any(ord(character) < 32 or ord(character) == 127 for character in path):
        fail(f"control character in repository path: {path!r}")


def read_source_entries(
    source: Path, revision: str, policy: dict[str, Any]
) -> tuple[str, list[dict[str, Any]]]:
    source_commit = git(source, "rev-parse", "--verify", f"{revision}^{{commit}}")
    source_commit_text = source_commit.decode("ascii").strip()
    excluded = set(policy["excluded_paths"])
    executables = set(policy["allowed_executable_paths"])

    for path in excluded | executables:
        validate_path(path)

    raw = git(source, "ls-tree", "-r", "-z", "--full-tree", source_commit_text)
    records: list[dict[str, Any]] = []
    seen: set[str] = set()
    for item in raw.split(b"\0"):
        if not item:
            continue
        metadata, raw_path = item.split(b"\t", 1)
        try:
            path = raw_path.decode("utf-8", errors="strict")
        except UnicodeDecodeError as error:
            fail(f"non-UTF-8 source path: {error}")
        validate_path(path)
        if path in excluded:
            continue
        mode, object_type, source_oid = metadata.decode("ascii").split(" ")
        expected_mode = "100755" if path in executables else "100644"
        if object_type != "blob" or mode != expected_mode:
            fail(
                f"unexpected type/mode for {path}: {object_type} {mode}; "
                f"expected blob {expected_mode}"
            )
        if path in seen:
            fail(f"duplicate path: {path}")
        seen.add(path)
        content = git(source, "cat-file", "blob", source_oid)
        records.append(
            {
                "path": path,
                "mode": mode,
                "sha256": hashlib.sha256(content).hexdigest(),
                "bytes": len(content),
                "source_blob_oid": source_oid,
                "content": content,
            }
        )

    missing_exclusions = excluded - {
        entry.decode("utf-8")
        for entry in [part.split(b"\t", 1)[1] for part in raw.split(b"\0") if part]
    }
    if missing_exclusions:
        fail(f"path policy names missing excluded paths: {sorted(missing_exclusions)}")
    records.sort(key=lambda record: record["path"].encode("utf-8"))
    return source_commit_text, records


def insert_tree(root: dict[str, Any], record: dict[str, Any]) -> None:
    parts = record["path"].split("/")
    node = root
    for part in parts[:-1]:
        child = node["directories"].setdefault(
            part, {"directories": {}, "files": {}}
        )
        if part in node["files"]:
            fail(f"path collides with file: {record['path']}")
        node = child
    name = parts[-1]
    if name in node["directories"] or name in node["files"]:
        fail(f"duplicate/colliding tree entry: {record['path']}")
    node["files"][name] = record


def write_tree(node: dict[str, Any], git_dir: Path, env: dict[str, str]) -> str:
    entries: list[bytes] = []
    for name, child in node["directories"].items():
        oid = write_tree(child, git_dir, env)
        entries.append(f"040000 tree {oid}\t{name}".encode("utf-8") + b"\0")
    for name, record in node["files"].items():
        entries.append(
            f"{record['mode']} blob {record['public_blob_oid']}\t{name}".encode("utf-8")
            + b"\0"
        )
    entries.sort(key=lambda item: item.split(b"\t", 1)[1])
    return bare_git(git_dir, "mktree", "-z", env=env, data=b"".join(entries)).decode(
        "ascii"
    ).strip()


def build(args: argparse.Namespace) -> dict[str, Any]:
    source = args.source.resolve()
    policy_path = args.policy.resolve()
    output = args.output.resolve()
    manifest_path = args.manifest.resolve()
    if output.exists():
        fail(f"output repository already exists: {output}")
    if manifest_path.exists():
        fail(f"manifest already exists: {manifest_path}")
    if args.source_date_epoch < 0:
        fail("SOURCE_DATE_EPOCH must be non-negative")

    policy = load_policy(policy_path)
    source_commit, records = read_source_entries(source, args.revision, policy)
    output.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    home = output.parent / f".{output.name}.home"
    if home.exists():
        fail(f"isolated HOME already exists: {home}")
    home.mkdir(mode=0o700)
    env = canonical_environment(home)
    try:
        run(
            ["git", "init", "--bare", "--object-format=sha1", str(output)],
            env=env,
        )
        bare_git(output, "config", "core.autocrlf", "false", env=env)
        bare_git(output, "config", "core.filemode", "true", env=env)
        bare_git(output, "config", "commit.gpgsign", "false", env=env)

        tree: dict[str, Any] = {"directories": {}, "files": {}}
        for record in records:
            public_oid = bare_git(
                output, "hash-object", "-w", "--stdin", env=env, data=record["content"]
            ).decode("ascii").strip()
            record["public_blob_oid"] = public_oid
            insert_tree(tree, record)
        tree_oid = write_tree(tree, output, env)

        commit_env = dict(env)
        date = f"{args.source_date_epoch} +0000"
        commit_env.update(
            {
                "GIT_AUTHOR_NAME": PUBLIC_NAME,
                "GIT_AUTHOR_EMAIL": PUBLIC_EMAIL,
                "GIT_COMMITTER_NAME": PUBLIC_NAME,
                "GIT_COMMITTER_EMAIL": PUBLIC_EMAIL,
                "GIT_AUTHOR_DATE": date,
                "GIT_COMMITTER_DATE": date,
            }
        )
        commit_oid = bare_git(
            output,
            "commit-tree",
            tree_oid,
            env=commit_env,
            data=ROOT_MESSAGE,
        ).decode("ascii").strip()
        bare_git(output, "symbolic-ref", "HEAD", "refs/heads/main", env=env)
        bare_git(
            output,
            "update-ref",
            "--no-deref",
            "refs/heads/main",
            commit_oid,
            "",
            env=env,
        )
        bare_git(
            output,
            "update-ref",
            "--no-deref",
            f"refs/tags/{PUBLIC_TAG}",
            commit_oid,
            "",
            env=env,
        )

        manifest = {
            "$schema": "public-root-manifest-v1.schema.json",
            "schema": "mirante4d-public-root-manifest",
            "schema_version": 1,
            "source_commit": source_commit,
            "source_date_epoch": args.source_date_epoch,
            "git_object_format": "sha1",
            "author_name": PUBLIC_NAME,
            "author_email": PUBLIC_EMAIL,
            "commit_message_utf8": ROOT_MESSAGE.decode("utf-8"),
            "tree_oid": tree_oid,
            "commit_oid": commit_oid,
            "tag_name": PUBLIC_TAG,
            "path_policy_sha256": hashlib.sha256(policy_path.read_bytes()).hexdigest(),
            "paths": [
                {
                    key: record[key]
                    for key in (
                        "path",
                        "mode",
                        "bytes",
                        "sha256",
                        "source_blob_oid",
                        "public_blob_oid",
                    )
                }
                for record in records
            ],
        }
        manifest_path.write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        os.chmod(manifest_path, 0o600)
        return manifest
    finally:
        shutil.rmtree(home, ignore_errors=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--source-date-epoch", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    result = build(parse_args())
    print(json.dumps({"commit": result["commit_oid"], "tree": result["tree_oid"]}))
