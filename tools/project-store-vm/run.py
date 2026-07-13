#!/usr/bin/env python3
"""Rootless WP-10B project-store power-cut qualification harness."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import re
import selectors
import shutil
import signal
import socket as socket_module
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


EVIDENCE_PREFIX = "mirante4d-project-store-vm-evidence:"
READY_PREFIX = "mirante4d-project-store-vm-ready:"
RESULT_PREFIX = "mirante4d-project-store-vm-result:"
TRACE_PREFIX = "mirante4d-project-store-vm-trace:"
FILESYSTEM_PREFIX = "mirante4d-project-store-vm-filesystem:"
DRIVER_EXIT_PREFIX = "mirante4d-project-store-vm-driver-exit:"
REAL_FILESYSTEM_POLICY_ENV = "MIRANTE4D_PROJECT_STORE_TEST_REAL_FILESYSTEM_POLICY"
REAL_FILESYSTEM_POLICY_EXPORT = f"export {REAL_FILESYSTEM_POLICY_ENV}=1"
NBD_SOCKET_PREFIX = "m4d-nbd-"
UNIX_SOCKET_PATH_BYTES_MAX = 107
INITRAMFS_BUSYBOX_APPLETS = (
    "awk",
    "cat",
    "grep",
    "gzip",
    "head",
    "mkdir",
    "mount",
    "poweroff",
    "sh",
    "sleep",
    "stat",
    "sync",
    "tar",
)

MANIFEST_SCHEMA = "mirante4d-wp10b-project-store-vm-manifest"
EVIDENCE_SCHEMA = "mirante4d-wp10b-project-store-lifecycle-evidence"
READY_SCHEMA = "mirante4d-wp10b-vm-transition-marker"
RESULT_SCHEMA = "mirante4d-wp10b-vm-guest-result"
TRACE_SCHEMA = "mirante4d-wp10b-vm-transition-trace"

HEX_64 = re.compile(r"^[0-9a-f]{64}$")
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
SAFE_TOKEN = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")
SAFE_TEXT = re.compile(r"^[A-Za-z0-9][A-Za-z0-9 ._:+~=/()-]{0,255}$")

EXPECTED_TRANSITIONS = {
    "object_file_sync",
    "object_publish_noreplace",
    "object_directory_sync",
    "generation_file_sync",
    "generation_publish_noreplace",
    "generation_directory_sync",
    "recovery_file_sync",
    "recovery_replace",
    "recovery_directory_sync",
    "head_file_sync",
    "head_replace",
    "head_directory_sync",
    "package_tree_sync",
    "package_install_noreplace",
    "destination_parent_sync",
    "pin_file_sync",
    "pin_replace",
    "pin_directory_sync",
    "unpin_remove",
    "unpin_directory_sync",
    "gc_trash_directory_create",
    "gc_trash_collision_file_sync",
    "gc_trash_move",
    "gc_active_deduplicate_remove",
    "gc_source_directory_sync",
    "gc_trash_directory_sync",
    "purge_remove",
    "purge_directory_sync",
}

ROOT_KEYS = {
    "schema",
    "schema_version",
    "fixture_id",
    "guest_driver",
    "pre_sequence",
    "constraints",
    "flows",
    "performance",
}
CONSTRAINT_KEYS = {
    "timeout_seconds",
    "retries",
    "qemu_package",
    "qemu_version",
    "kernel_package",
    "kernel_version",
    "kernel_deb_sha256",
    "busybox_package",
    "busybox_version",
    "busybox_sha256",
    "nbdkit_package",
    "nbdkit_version",
    "nbdkit_deb_sha256",
    "e2fsprogs_version",
    "guest_memory_bytes",
    "disk_count",
    "disk_bytes_each",
    "working_bytes_max",
    "filesystem",
    "mount_options",
}


class HarnessFailure(RuntimeError):
    """A sanitized, user-actionable harness failure."""


def exact_object(value: Any, keys: set[str], context: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise HarnessFailure(f"{context} fields drifted")
    return value


def safe_token(value: Any, context: str) -> str:
    if not isinstance(value, str) or SAFE_TOKEN.fullmatch(value) is None:
        raise HarnessFailure(f"{context} is not a safe token")
    return value


def safe_text(value: Any, context: str) -> str:
    if not isinstance(value, str) or SAFE_TEXT.fullmatch(value) is None:
        raise HarnessFailure(f"{context} is not sanitized text")
    return value


def unsigned(value: Any, context: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise HarnessFailure(f"{context} is not an unsigned integer")
    return value


def positive(value: Any, context: str) -> int:
    value = unsigned(value, context)
    if value == 0:
        raise HarnessFailure(f"{context} must be positive")
    return value


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise HarnessFailure("VM manifest is unreadable or invalid JSON") from error
    if not isinstance(value, dict):
        raise HarnessFailure("VM manifest root must be an object")
    return value


def validate_guest_init_text(text: str) -> None:
    lines = [line.strip() for line in text.splitlines()]
    policy_lines = [
        (index, line)
        for index, line in enumerate(lines)
        if REAL_FILESYSTEM_POLICY_ENV in line
    ]
    invocations = [
        index
        for index, line in enumerate(lines)
        if line.startswith("/mirante4d-project-store-tests")
    ]
    if (
        len(policy_lines) != 1
        or policy_lines[0][1] != REAL_FILESYSTEM_POLICY_EXPORT
        or len(invocations) != 1
        or policy_lines[0][0] >= invocations[0]
    ):
        raise HarnessFailure(
            "VM guest init does not force the real filesystem qualification detector"
        )


def validate_guest_init(repo: Path) -> Path:
    path = repo / "tools/project-store-vm/guest-init.sh"
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise HarnessFailure("VM guest init is unreadable") from error
    validate_guest_init_text(text)
    return path


def validate_manifest(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    exact_object(manifest, ROOT_KEYS, "VM manifest")
    if manifest["schema"] != MANIFEST_SCHEMA or manifest["schema_version"] != 1:
        raise HarnessFailure("VM manifest schema identity drifted")
    if manifest["fixture_id"] != "wp10b-hostile-lifecycle":
        raise HarnessFailure("VM manifest fixture identity drifted")

    driver = exact_object(
        manifest["guest_driver"],
        {"test", "fixture", "guest_fixture", "root_a", "root_b"},
        "VM guest driver",
    )
    expected_driver = {
        "test": "actor::tests::durability_tests::project_store_vm_guest_driver",
        "fixture": "fixtures/project/project-store-v1.tar.gz",
        "guest_fixture": "/fixtures/project-store-v1.tar.gz",
        "root_a": "/mnt/project-a",
        "root_b": "/mnt/project-b",
    }
    if driver != expected_driver:
        raise HarnessFailure("VM guest driver boundary drifted")

    pre_sequence = exact_object(
        manifest["pre_sequence"], {"case", "lane"}, "VM pre-sequence cut"
    )
    if pre_sequence != {"case": "save-as", "lane": "none"}:
        raise HarnessFailure("VM pre-sequence cut boundary drifted")

    constraints = exact_object(manifest["constraints"], CONSTRAINT_KEYS, "VM constraints")
    exact_constraints = {
        "timeout_seconds": 900,
        "retries": 0,
        "qemu_package": "qemu-system-x86",
        "qemu_version": "1:8.2.2+ds-0ubuntu1.17",
        "kernel_package": "linux-image-6.17.0-35-generic",
        "kernel_version": "6.17.0-35.35~24.04.1",
        "kernel_deb_sha256": "d5502a5dfa01203e16f6430e10236efe9e007cd29bd93bbed65ddf20ee6e9cfa",
        "busybox_package": "busybox-static",
        "busybox_version": "1:1.36.1-6ubuntu3.1",
        "busybox_sha256": "dbac288c29ba568459550a2da9e7ae0ded6b1fc728ee9fad3044c44e62d6ac14",
        "nbdkit_package": "nbdkit",
        "nbdkit_version": "1.36.3-1ubuntu10",
        "nbdkit_deb_sha256": "02ae094a32267be68516e1dedd26a2b83334a1a20303055ce765e2e9cf8580e2",
        "e2fsprogs_version": "1.47.0",
        "guest_memory_bytes": 268435456,
        "disk_count": 2,
        "disk_bytes_each": 134217728,
        "working_bytes_max": 671088640,
        "filesystem": "ext4",
        "mount_options": ["relatime", "rw"],
    }
    if constraints != exact_constraints:
        raise HarnessFailure("VM constraints drifted from the accepted WP-10B entry")

    flows = manifest["flows"]
    if not isinstance(flows, list) or len(flows) != 11:
        raise HarnessFailure("VM manifest must contain the eleven accepted flows")
    ids: set[str] = set()
    observed_names: set[str] = set()
    rows: list[dict[str, Any]] = []
    manual_ref_flows = 0
    autosave_ref_flows = 0
    for flow in flows:
        flow = exact_object(flow, {"id", "case", "lane", "transitions"}, "VM flow")
        flow_id = safe_token(flow["id"], "VM flow id")
        case_name = safe_token(flow["case"], "VM case")
        lane = flow["lane"]
        if lane not in {"none", "manual", "autosave"}:
            raise HarnessFailure("VM flow lane is invalid")
        if flow_id in ids:
            raise HarnessFailure("VM flow ids must be unique")
        ids.add(flow_id)
        transitions = flow["transitions"]
        if not isinstance(transitions, list) or not transitions:
            raise HarnessFailure("VM flow must contain transitions")
        flow_rows: set[tuple[str, int]] = set()
        for transition in transitions:
            transition = exact_object(
                transition, {"name", "occurrences"}, "VM transition"
            )
            name = safe_token(transition["name"], "VM transition name")
            occurrences = transition["occurrences"]
            if not isinstance(occurrences, list) or not occurrences:
                raise HarnessFailure("VM transition occurrences must be nonempty")
            parsed = [unsigned(item, "VM transition occurrence") for item in occurrences]
            if parsed != list(range(len(parsed))):
                raise HarnessFailure("VM transition occurrences must be contiguous from zero")
            observed_names.add(name)
            for occurrence in parsed:
                key = (name, occurrence)
                if key in flow_rows:
                    raise HarnessFailure("VM flow contains a duplicate transition occurrence")
                flow_rows.add(key)
                rows.append(
                    {
                        "flow": flow_id,
                        "case": case_name,
                        "transition": name,
                        "lane": lane,
                        "edge": "after",
                        "occurrence": occurrence,
                    }
                )
        if case_name in {"recovery-ref", "head-ref"}:
            manual_ref_flows += int(lane == "manual")
            autosave_ref_flows += int(lane == "autosave")
    if observed_names != EXPECTED_TRANSITIONS or len(rows) != 59:
        raise HarnessFailure("VM transition-name inventory drifted")
    if manual_ref_flows != 2 or autosave_ref_flows != 2:
        raise HarnessFailure("manual and autosave ref flows must remain distinct")

    performance = exact_object(
        manifest["performance"],
        {
            "case",
            "lane",
            "samples",
            "enqueue_poll_p99_ms_max",
            "incremental_unchanged_artifact_bytes_rewritten_max",
            "post_open_or_save_metadata_rss_bytes_max",
        },
        "VM performance case",
    )
    if performance != {
        "case": "performance",
        "lane": "none",
        "samples": 1000,
        "enqueue_poll_p99_ms_max": 5.0,
        "incremental_unchanged_artifact_bytes_rewritten_max": 0,
        "post_open_or_save_metadata_rss_bytes_max": 100663296,
    }:
        raise HarnessFailure("VM performance boundary drifted")
    return rows


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
    except OSError as error:
        raise HarnessFailure("a required harness input could not be hashed") from error
    return digest.hexdigest()


def run_checked(
    command: list[str],
    *,
    cwd: Path | None = None,
    timeout: float = 120.0,
    env: dict[str, str] | None = None,
) -> str:
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise HarnessFailure(f"required command {command[0]} could not complete") from error
    if completed.returncode != 0:
        raise HarnessFailure(f"required command {command[0]} failed")
    return completed.stdout


def command_path(name: str) -> Path:
    path = shutil.which(name)
    if path is None:
        raise HarnessFailure(f"required command {name} is not installed")
    return Path(path)


def package_version(package: str, expected: str) -> str:
    output = run_checked(
        ["dpkg-query", "-W", "-f=${Version}\t${Architecture}", package], timeout=10
    ).strip()
    if output != f"{expected}\tamd64":
        raise HarnessFailure(f"installed package {package} does not match its frozen version")
    return expected


def download_deb(
    package: str, version: str, expected_sha256: str, directory: Path, deadline: float
) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    for candidate in directory.glob("*.deb"):
        if sha256_file(candidate) == expected_sha256:
            return candidate
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise HarnessFailure("VM aggregate timeout expired during package preflight")
    run_checked(
        ["apt-get", "download", f"{package}={version}"],
        cwd=directory,
        timeout=min(120.0, remaining),
    )
    matches = [path for path in directory.glob("*.deb") if sha256_file(path) == expected_sha256]
    if len(matches) != 1:
        raise HarnessFailure(f"downloaded package {package} did not match its frozen digest")
    return matches[0]


def git_identity(repo: Path) -> dict[str, Any]:
    status = run_checked(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=repo, timeout=10
    )
    if status:
        raise HarnessFailure("project-store lifecycle evidence requires a clean revision")
    commit = run_checked(["git", "rev-parse", "HEAD"], cwd=repo, timeout=10).strip()
    tree = run_checked(
        ["git", "show", "-s", "--format=%T", "HEAD"], cwd=repo, timeout=10
    ).strip()
    if HEX_40.fullmatch(commit) is None or HEX_40.fullmatch(tree) is None:
        raise HarnessFailure("git identity was not a SHA-1 commit and tree")
    return {"commit": commit, "tree": tree, "clean": True}


def require_same_git_identity(initial: dict[str, Any], final: dict[str, Any]) -> None:
    expected = {name: initial.get(name) for name in ["commit", "tree", "clean"]}
    if expected != final or final.get("clean") is not True:
        raise HarnessFailure("git identity changed during project-store lifecycle evidence")


def qemu_package_version(expected: str) -> str:
    return package_version("qemu-system-x86", expected)


def e2fsprogs_version(expected: str) -> str:
    output = run_checked(["mkfs.ext4", "-V"], timeout=10)
    combined = output
    if expected not in combined:
        # mke2fs writes its version to stderr on some releases.
        try:
            completed = subprocess.run(
                ["mkfs.ext4", "-V"],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=10,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise HarnessFailure("mke2fs version could not be read") from error
        combined = completed.stdout
    if expected not in combined:
        raise HarnessFailure("mke2fs does not match the frozen version")
    return expected


def build_guest_test(repo: Path, deadline: float) -> Path:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise HarnessFailure("VM aggregate timeout expired before guest build")
    output = run_checked(
        [
            "cargo",
            "test",
            "--package",
            "mirante4d-project-store",
            "--lib",
            "--no-run",
            "--frozen",
            "--message-format=json",
        ],
        cwd=repo,
        timeout=min(300.0, remaining),
    )
    executables: list[Path] = []
    for line in output.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            event.get("reason") == "compiler-artifact"
            and event.get("target", {}).get("name") == "mirante4d_project_store"
            and event.get("profile", {}).get("test") is True
            and isinstance(event.get("executable"), str)
        ):
            executables.append(Path(event["executable"]))
    if len(executables) != 1 or not executables[0].is_file():
        raise HarnessFailure("cargo did not identify exactly one project-store guest test binary")
    listed = run_checked([str(executables[0]), "--list"], timeout=30)
    expected = (
        "actor::tests::durability_tests::project_store_vm_guest_driver: test"
    )
    if expected not in listed.splitlines():
        raise HarnessFailure("project-store VM guest driver test is missing")
    return executables[0]


def copy_dynamic_libraries(binary: Path, stage: Path) -> None:
    output = run_checked(["ldd", str(binary)], timeout=20)
    libraries: set[Path] = set()
    for line in output.splitlines():
        stripped = line.strip()
        if "=>" in stripped:
            candidate = stripped.split("=>", 1)[1].strip().split(" ", 1)[0]
        else:
            candidate = stripped.split(" ", 1)[0]
        if candidate.startswith("/"):
            libraries.add(Path(candidate))
    for library in sorted(libraries):
        destination = stage / library.relative_to("/")
        destination.parent.mkdir(parents=True, exist_ok=True)
        try:
            shutil.copy2(library.resolve(), destination)
        except OSError as error:
            raise HarnessFailure("a guest test runtime library could not be copied") from error


def build_initramfs(
    repo: Path,
    work: Path,
    test_binary: Path,
    busybox: Path,
    fixture: Path,
) -> Path:
    stage = work / "initramfs-root"
    stage.mkdir(parents=True)
    for directory in ["bin", "dev", "proc", "sys", "tmp", "mnt", "fixtures"]:
        (stage / directory).mkdir(parents=True, exist_ok=True)
    shutil.copy2(validate_guest_init(repo), stage / "init")
    (stage / "init").chmod(0o755)
    shutil.copy2(busybox, stage / "bin/busybox")
    (stage / "bin/busybox").chmod(0o755)
    for applet in INITRAMFS_BUSYBOX_APPLETS:
        (stage / "bin" / applet).symlink_to("busybox")
    shutil.copy2(test_binary, stage / "mirante4d-project-store-tests")
    (stage / "mirante4d-project-store-tests").chmod(0o755)
    shutil.copy2(fixture, stage / "fixtures/project-store-v1.tar.gz")
    copy_dynamic_libraries(test_binary, stage)

    initramfs = work / "project-store-vm-initramfs.cpio.gz"
    find = subprocess.Popen(
        ["find", ".", "-print0"], cwd=stage, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
    )
    assert find.stdout is not None
    cpio = subprocess.Popen(
        ["cpio", "--null", "--create", "--format=newc", "--quiet"],
        cwd=stage,
        stdin=find.stdout,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    find.stdout.close()
    assert cpio.stdout is not None
    with gzip.open(initramfs, "wb", compresslevel=6) as archive:
        shutil.copyfileobj(cpio.stdout, archive)
    cpio.stdout.close()
    if find.wait() != 0 or cpio.wait() != 0:
        raise HarnessFailure("guest initramfs construction failed")
    return initramfs


def allocated_bytes(root: Path) -> int:
    total = 0
    try:
        for directory, names, files in os.walk(root):
            for name in names + files:
                path = Path(directory) / name
                try:
                    info = path.lstat()
                except FileNotFoundError:
                    continue
                total += info.st_blocks * 512
    except OSError as error:
        raise HarnessFailure("VM working allocation could not be measured") from error
    return total


def filesystem_features(image: Path) -> list[str]:
    output = run_checked(["tune2fs", "-l", str(image)], timeout=20)
    for line in output.splitlines():
        if line.startswith("Filesystem features:"):
            features = sorted(line.split(":", 1)[1].split())
            if features:
                return features
    raise HarnessFailure("ext4 filesystem features could not be observed")


def prepare_disks(case_dir: Path, disk_bytes: int) -> tuple[list[Path], list[str]]:
    images = [case_dir / "project-a.img", case_dir / "project-b.img"]
    for index, image in enumerate(images):
        with image.open("wb") as handle:
            handle.truncate(disk_bytes)
        run_checked(
            [
                "mkfs.ext4",
                "-F",
                "-q",
                "-m",
                "0",
                "-E",
                "lazy_itable_init=0,lazy_journal_init=0",
                "-L",
                f"M4D_{index}",
                str(image),
            ],
            timeout=30,
        )
    first = filesystem_features(images[0])
    if filesystem_features(images[1]) != first:
        raise HarnessFailure("the two ext4 disk feature sets differ")
    return images, first


def validate_unix_socket_path(path: Path) -> None:
    if len(os.fsencode(path)) > UNIX_SOCKET_PATH_BYTES_MAX:
        raise HarnessFailure("VM socket path exceeds the Linux AF_UNIX limit")


def start_nbdkit(
    nbdkit: Path, image: Path, directory: Path, deadline: float
) -> tuple[subprocess.Popen[str], Path, Path]:
    directory.mkdir(parents=True)
    socket_root = Path(tempfile.mkdtemp(prefix=NBD_SOCKET_PREFIX, dir="/tmp"))
    socket = socket_root / "nbd.sock"
    try:
        validate_unix_socket_path(socket)
    except HarnessFailure:
        shutil.rmtree(socket_root, ignore_errors=True)
        raise
    environment = os.environ.copy()
    environment["TMPDIR"] = str(directory)
    try:
        process = subprocess.Popen(
            [
                str(nbdkit),
                "--foreground",
                "--unix",
                str(socket),
                "--filter=cache",
                "file",
                str(image),
                "cache=writeback",
                "cache-on-read=false",
            ],
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
            start_new_session=True,
        )
    except OSError as error:
        shutil.rmtree(socket_root, ignore_errors=True)
        raise HarnessFailure("nbdkit could not be started") from error
    while not socket.exists():
        if process.poll() is not None:
            shutil.rmtree(socket_root, ignore_errors=True)
            raise HarnessFailure("nbdkit failed before publishing its local socket")
        if time.monotonic() >= deadline:
            kill_process(process)
            shutil.rmtree(socket_root, ignore_errors=True)
            raise HarnessFailure("nbdkit socket startup exceeded the aggregate timeout")
        time.sleep(0.01)
    return process, socket, socket_root


def kill_process(process: subprocess.Popen[Any]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def require_sigkill_exit(was_running: bool, returncode: int, context: str) -> None:
    if not was_running or returncode != -signal.SIGKILL:
        raise HarnessFailure(f"deliberate {context} cut was not an observed SIGKILL")


def deliberate_power_cut(process: subprocess.Popen[Any], context: str) -> None:
    was_running = process.poll() is None
    if not was_running:
        require_sigkill_exit(False, process.returncode or 0, context)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError as error:
        raise HarnessFailure(f"deliberate {context} cut lost its live process") from error
    try:
        returncode = process.wait(timeout=5)
    except subprocess.TimeoutExpired as error:
        raise HarnessFailure(f"deliberate {context} cut did not terminate") from error
    require_sigkill_exit(was_running, returncode, context)


def stop_process(process: subprocess.Popen[Any]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        kill_process(process)


def parse_prefixed_json(line: str, prefix: str, context: str) -> dict[str, Any]:
    try:
        value = json.loads(line[len(prefix) :])
    except json.JSONDecodeError as error:
        raise HarnessFailure(f"{context} was not valid JSON") from error
    if not isinstance(value, dict):
        raise HarnessFailure(f"{context} must be an object")
    return value


def parse_ready(value: dict[str, Any], row: dict[str, Any]) -> None:
    exact_object(
        value,
        {
            "schema",
            "schema_version",
            "role",
            "case",
            "transition",
            "lane",
            "edge",
            "occurrence",
            "status",
        },
        "VM ready marker",
    )
    expected = {
        "schema": READY_SCHEMA,
        "schema_version": 1,
        "role": "exercise",
        "case": row["case"],
        "transition": row["transition"],
        "lane": row["lane"],
        "edge": row["edge"],
        "occurrence": row["occurrence"],
        "status": "ready",
    }
    if value != expected:
        raise HarnessFailure("VM ready marker did not match the requested cut")


def parse_trace(value: dict[str, Any]) -> dict[str, Any]:
    exact_object(
        value,
        {"schema", "schema_version", "transition", "lane", "edge", "occurrence"},
        "VM trace row",
    )
    if value["schema"] != TRACE_SCHEMA or value["schema_version"] != 1:
        raise HarnessFailure("VM trace schema identity drifted")
    safe_token(value["transition"], "VM trace transition")
    if value["lane"] not in {"none", "manual", "autosave"}:
        raise HarnessFailure("VM trace lane is invalid")
    if value["edge"] not in {"before", "after"}:
        raise HarnessFailure("VM trace edge is invalid")
    unsigned(value["occurrence"], "VM trace occurrence")
    return value


def parse_result(value: dict[str, Any], case_name: str) -> dict[str, int | float]:
    exact_object(
        value,
        {"schema", "schema_version", "role", "case", "status", "counters"},
        "VM guest result",
    )
    if (
        value["schema"] != RESULT_SCHEMA
        or value["schema_version"] != 1
        or value["role"] != "validate"
        or value["case"] != case_name
        or value["status"] != "passed"
        or not isinstance(value["counters"], dict)
    ):
        raise HarnessFailure("VM guest result identity or counters drifted")
    counters = value["counters"]
    if case_name == "performance":
        exact_object(
            counters,
            {
                "enqueue_samples",
                "enqueue_p99_nanoseconds",
                "poll_samples",
                "poll_p99_nanoseconds",
                "unchanged_artifact_bytes_rewritten",
                "post_open_or_save_metadata_rss_bytes",
                "exact_retry_attempts",
                "power_loss_simulated",
            },
            "VM performance counters",
        )
        enqueue_samples = unsigned(counters["enqueue_samples"], "VM enqueue samples")
        poll_samples = unsigned(counters["poll_samples"], "VM poll samples")
        enqueue_p99 = unsigned(
            counters["enqueue_p99_nanoseconds"], "VM enqueue p99"
        )
        poll_p99 = unsigned(counters["poll_p99_nanoseconds"], "VM poll p99")
        if (
            unsigned(counters["exact_retry_attempts"], "VM exact retry attempts") != 0
            or counters["power_loss_simulated"] is not False
        ):
            raise HarnessFailure("VM performance counters reported retry or power loss")
        return {
            "enqueue_poll_samples": min(enqueue_samples, poll_samples),
            "enqueue_poll_p99_ms": max(enqueue_p99, poll_p99) / 1_000_000.0,
            "incremental_unchanged_artifact_bytes_rewritten": unsigned(
                counters["unchanged_artifact_bytes_rewritten"],
                "VM unchanged artifact bytes",
            ),
            "post_open_or_save_metadata_rss_bytes": unsigned(
                counters["post_open_or_save_metadata_rss_bytes"],
                "VM metadata RSS",
            ),
        }
    exact_object(
        counters,
        {"exact_retry_attempts", "power_loss_simulated"},
        "VM recovery counters",
    )
    retries = unsigned(counters["exact_retry_attempts"], "VM exact retry attempts")
    if retries != 1 or counters["power_loss_simulated"] is not True:
        raise HarnessFailure("VM recovery counters did not prove one exact retry after power loss")
    return {"exact_retry_attempts": retries, "validated_power_cuts": 1}


def parse_trace_result(value: dict[str, Any], case_name: str) -> None:
    exact_object(
        value,
        {"schema", "schema_version", "role", "case", "status", "counters"},
        "VM trace result",
    )
    if value != {
        "schema": RESULT_SCHEMA,
        "schema_version": 1,
        "role": "trace",
        "case": case_name,
        "status": "passed",
        "counters": {},
    }:
        raise HarnessFailure("VM trace result identity drifted")


def parse_filesystem_line(line: str) -> dict[str, Any]:
    parts = line[len(FILESYSTEM_PREFIX) :].split("|")
    if len(parts) != 5 or parts[0] not in {"project-a", "project-b"}:
        raise HarnessFailure("VM filesystem observation is malformed")
    try:
        device = unsigned(int(parts[1]), "VM filesystem device id")
    except ValueError as error:
        raise HarnessFailure("VM filesystem device id is malformed") from error
    magic = parts[2].lower()
    vfs_options = sorted(set(parts[3].split(",")))
    super_options = sorted(set(parts[4].split(",")))
    if magic != "ef53":
        raise HarnessFailure("VM filesystem magic is not ext4")
    return {
        "label": parts[0],
        "type": "ext4",
        "statfs_magic_hex": "0xef53",
        "vfs_options": vfs_options,
        "super_options": super_options,
        "device": device,
    }


def read_qemu(
    process: subprocess.Popen[str],
    deadline: float,
    *,
    stop_on_ready: bool,
) -> dict[str, Any]:
    if process.stdout is None:
        raise HarnessFailure("QEMU serial output was unavailable")
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    ready: list[dict[str, Any]] = []
    results: list[dict[str, Any]] = []
    traces: list[dict[str, Any]] = []
    filesystems: list[dict[str, Any]] = []
    driver_exits: list[int] = []
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise HarnessFailure("QEMU guest exceeded the aggregate timeout")
            events = selector.select(min(0.2, remaining))
            for key, _ in events:
                line = key.fileobj.readline()
                if not line:
                    continue
                line = line.strip()
                if line.startswith(READY_PREFIX):
                    ready.append(parse_prefixed_json(line, READY_PREFIX, "VM ready marker"))
                elif line.startswith(RESULT_PREFIX):
                    results.append(parse_prefixed_json(line, RESULT_PREFIX, "VM guest result"))
                elif line.startswith(TRACE_PREFIX):
                    traces.append(parse_trace(parse_prefixed_json(line, TRACE_PREFIX, "VM trace")))
                elif line.startswith(FILESYSTEM_PREFIX):
                    filesystems.append(parse_filesystem_line(line))
                elif line.startswith(DRIVER_EXIT_PREFIX):
                    try:
                        driver_exits.append(int(line[len(DRIVER_EXIT_PREFIX) :]))
                    except ValueError as error:
                        raise HarnessFailure("VM driver exit marker is malformed") from error
            if stop_on_ready and ready:
                return {
                    "ready": ready,
                    "results": results,
                    "traces": traces,
                    "filesystems": filesystems,
                    "driver_exits": driver_exits,
                }
            if process.poll() is not None:
                # Drain the text wrapper after EOF.
                for line in process.stdout:
                    line = line.strip()
                    if line.startswith(RESULT_PREFIX):
                        results.append(
                            parse_prefixed_json(line, RESULT_PREFIX, "VM guest result")
                        )
                    elif line.startswith(TRACE_PREFIX):
                        traces.append(
                            parse_trace(parse_prefixed_json(line, TRACE_PREFIX, "VM trace"))
                        )
                    elif line.startswith(FILESYSTEM_PREFIX):
                        filesystems.append(parse_filesystem_line(line))
                    elif line.startswith(DRIVER_EXIT_PREFIX):
                        driver_exits.append(int(line[len(DRIVER_EXIT_PREFIX) :]))
                return {
                    "ready": ready,
                    "results": results,
                    "traces": traces,
                    "filesystems": filesystems,
                    "driver_exits": driver_exits,
                }
    finally:
        selector.close()


def start_qemu(
    qemu: Path,
    kernel: Path,
    initramfs: Path,
    sockets: list[Path],
    constraints: dict[str, Any],
    *,
    role: str,
    case_name: str,
    transition: str,
    lane: str,
    edge: str,
    occurrence: int,
    paused_qmp: Path | None = None,
) -> subprocess.Popen[str]:
    for token, context in [
        (role, "VM role"),
        (case_name, "VM case"),
        (transition, "VM transition"),
        (lane, "VM lane"),
        (edge, "VM edge"),
    ]:
        safe_token(token, context)
    append = " ".join(
        [
            "console=ttyS0",
            "panic=-1",
            "rdinit=/init",
            f"m4d.role={role}",
            f"m4d.case={case_name}",
            f"m4d.transition={transition}",
            f"m4d.lane={lane}",
            f"m4d.edge={edge}",
            f"m4d.occurrence={occurrence}",
        ]
    )
    command = [
        str(qemu),
        "-machine",
        "q35,accel=kvm",
        "-cpu",
        "host",
        "-m",
        str(constraints["guest_memory_bytes"] // (1024 * 1024)),
        "-kernel",
        str(kernel),
        "-initrd",
        str(initramfs),
        "-append",
        append,
        "-display",
        "none",
        "-monitor",
        "none",
        "-serial",
        "stdio",
        "-no-reboot",
    ]
    if paused_qmp is not None:
        command.extend(
            ["-S", "-qmp", f"unix:{paused_qmp},server=on,wait=off"]
        )
    for index, socket in enumerate(sockets):
        node = f"project_{index}"
        block = {
            "driver": "nbd",
            "node-name": node,
            "server": {"type": "unix", "path": str(socket)},
        }
        command.extend(
            [
                "-blockdev",
                json.dumps(block, separators=(",", ":")),
                "-device",
                f"virtio-blk-pci,drive={node}",
            ]
        )
    return subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        start_new_session=True,
    )


def qmp_message(handle: Any, deadline: float, context: str) -> dict[str, Any]:
    while time.monotonic() < deadline:
        try:
            encoded = handle.readline()
        except (OSError, TimeoutError) as error:
            raise HarnessFailure(f"QMP {context} could not be read") from error
        if not encoded:
            raise HarnessFailure(f"QMP {context} ended unexpectedly")
        try:
            value = json.loads(encoded)
        except json.JSONDecodeError as error:
            raise HarnessFailure(f"QMP {context} was not valid JSON") from error
        if not isinstance(value, dict):
            raise HarnessFailure(f"QMP {context} was not an object")
        if "event" not in value:
            return value
    raise HarnessFailure(f"QMP {context} exceeded the aggregate timeout")


def qmp_execute(handle: Any, command: str, deadline: float) -> Any:
    try:
        handle.write(
            json.dumps({"execute": command}, separators=(",", ":")).encode("utf-8")
            + b"\n"
        )
        handle.flush()
    except OSError as error:
        raise HarnessFailure("QMP command could not be sent") from error
    response = qmp_message(handle, deadline, f"{command} response")
    if set(response) != {"return"}:
        raise HarnessFailure(f"QMP {command} failed")
    return response["return"]


def wait_for_paused_qemu(
    process: subprocess.Popen[str], qmp_socket: Path, deadline: float
) -> None:
    connection = socket_module.socket(socket_module.AF_UNIX, socket_module.SOCK_STREAM)
    try:
        while True:
            if process.poll() is not None:
                raise HarnessFailure("pre-sequence QEMU exited before its cut point")
            try:
                connection.connect(str(qmp_socket))
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.monotonic() >= deadline:
                    raise HarnessFailure(
                        "pre-sequence QEMU startup exceeded the aggregate timeout"
                    )
                time.sleep(0.01)
        connection.settimeout(max(0.1, min(5.0, deadline - time.monotonic())))
        with connection.makefile("rwb", buffering=0) as handle:
            greeting = qmp_message(handle, deadline, "greeting")
            if set(greeting) != {"QMP"} or not isinstance(greeting["QMP"], dict):
                raise HarnessFailure("pre-sequence QEMU did not publish a QMP greeting")
            qmp_execute(handle, "qmp_capabilities", deadline)
            status = qmp_execute(handle, "query-status", deadline)
            if (
                not isinstance(status, dict)
                or status.get("running") is not False
                or status.get("status") not in {"prelaunch", "paused"}
            ):
                raise HarnessFailure("pre-sequence QEMU was not paused before guest execution")
            nodes = qmp_execute(handle, "query-named-block-nodes", deadline)
            if not isinstance(nodes, list):
                raise HarnessFailure("pre-sequence QEMU block inventory was malformed")
            names = {
                node.get("node-name")
                for node in nodes
                if isinstance(node, dict) and isinstance(node.get("node-name"), str)
            }
            if not {"project_0", "project_1"}.issubset(names):
                raise HarnessFailure("pre-sequence QEMU did not attach both backing disks")
    finally:
        connection.close()


def observe_filesystems(
    observations: list[dict[str, Any]], required_options: list[str]
) -> dict[str, Any]:
    if len(observations) != 2 or {item["label"] for item in observations} != {
        "project-a",
        "project-b",
    }:
        raise HarnessFailure("guest did not report both project filesystems")
    first, second = sorted(observations, key=lambda item: item["label"])
    if (
        first["statfs_magic_hex"] != second["statfs_magic_hex"]
        or first["vfs_options"] != second["vfs_options"]
        or first["super_options"] != second["super_options"]
    ):
        raise HarnessFailure("guest ext4 qualification tuples differ")
    if first["vfs_options"] != required_options or first["super_options"] != ["rw"]:
        raise HarnessFailure("guest ext4 qualification tuple is not the frozen tuple")
    if first["device"] == second["device"]:
        raise HarnessFailure("Save As disks are not independent filesystems")
    return {
        "type": "ext4",
        "statfs_magic_hex": "0xef53",
        "vfs_options": first["vfs_options"],
        "super_options": first["super_options"],
        "device_count": 2,
        "independent_devices": True,
    }


def expected_trace(flow: dict[str, Any]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for transition in flow["transitions"]:
        for occurrence in transition["occurrences"]:
            for edge in ["before", "after"]:
                rows.append(
                    {
                        "schema": TRACE_SCHEMA,
                        "schema_version": 1,
                        "transition": transition["name"],
                        "lane": flow["lane"],
                        "edge": edge,
                        "occurrence": occurrence,
                    }
                )
    return rows


def run_complete_guest(
    *,
    role: str,
    case_name: str,
    transition: str,
    lane: str,
    edge: str,
    occurrence: int,
    case_dir: Path,
    qemu: Path,
    nbdkit: Path,
    kernel: Path,
    initramfs: Path,
    constraints: dict[str, Any],
    deadline: float,
) -> tuple[dict[str, Any], list[str], int]:
    images, features = prepare_disks(case_dir, constraints["disk_bytes_each"])
    servers: list[subprocess.Popen[str]] = []
    sockets: list[Path] = []
    socket_roots: list[Path] = []
    qemu_process: subprocess.Popen[str] | None = None
    try:
        for index, image in enumerate(images):
            server, socket, socket_root = start_nbdkit(
                nbdkit, image, case_dir / f"cache-{index}", deadline
            )
            servers.append(server)
            sockets.append(socket)
            socket_roots.append(socket_root)
        qemu_process = start_qemu(
            qemu,
            kernel,
            initramfs,
            sockets,
            constraints,
            role=role,
            case_name=case_name,
            transition=transition,
            lane=lane,
            edge=edge,
            occurrence=occurrence,
        )
        output = read_qemu(qemu_process, deadline, stop_on_ready=False)
        if qemu_process.returncode != 0:
            raise HarnessFailure("QEMU baseline or validation guest failed")
        if output["driver_exits"] != [0]:
            raise HarnessFailure("VM guest driver did not exit successfully exactly once")
        peak = allocated_bytes(case_dir)
        return output, features, peak
    finally:
        if qemu_process is not None:
            stop_process(qemu_process)
        for server in servers:
            stop_process(server)
        for socket_root in socket_roots:
            shutil.rmtree(socket_root, ignore_errors=True)


def run_pre_sequence_cut(
    pre_sequence: dict[str, Any],
    *,
    case_dir: Path,
    qemu: Path,
    nbdkit: Path,
    kernel: Path,
    initramfs: Path,
    constraints: dict[str, Any],
    deadline: float,
) -> tuple[dict[str, int | float], dict[str, Any], list[str], int]:
    images, features = prepare_disks(case_dir, constraints["disk_bytes_each"])
    servers: list[subprocess.Popen[str]] = []
    socket_roots: list[Path] = []
    exercise: subprocess.Popen[str] | None = None
    peak = allocated_bytes(case_dir)
    try:
        exercise_sockets: list[Path] = []
        for index, image in enumerate(images):
            server, socket, socket_root = start_nbdkit(
                nbdkit, image, case_dir / f"cache-pre-{index}", deadline
            )
            servers.append(server)
            exercise_sockets.append(socket)
            socket_roots.append(socket_root)
        qmp_socket = socket_roots[0] / "qmp.sock"
        validate_unix_socket_path(qmp_socket)
        exercise = start_qemu(
            qemu,
            kernel,
            initramfs,
            exercise_sockets,
            constraints,
            role="exercise",
            case_name=pre_sequence["case"],
            transition="pre-sequence",
            lane=pre_sequence["lane"],
            edge="before",
            occurrence=0,
            paused_qmp=qmp_socket,
        )
        wait_for_paused_qemu(exercise, qmp_socket, deadline)
        peak = max(peak, allocated_bytes(case_dir))

        # The vCPUs have never run: this is the one explicit pre-sequence cut.
        deliberate_power_cut(exercise, "pre-sequence QEMU")
        exercise = None
        for index, server in enumerate(servers):
            deliberate_power_cut(server, f"pre-sequence nbdkit {index}")
        servers.clear()

        validation_sockets: list[Path] = []
        for index, image in enumerate(images):
            server, socket, socket_root = start_nbdkit(
                nbdkit, image, case_dir / f"cache-validate-{index}", deadline
            )
            servers.append(server)
            validation_sockets.append(socket)
            socket_roots.append(socket_root)
        validate = start_qemu(
            qemu,
            kernel,
            initramfs,
            validation_sockets,
            constraints,
            role="validate",
            case_name=pre_sequence["case"],
            transition="pre-sequence",
            lane=pre_sequence["lane"],
            edge="before",
            occurrence=0,
        )
        try:
            validation = read_qemu(validate, deadline, stop_on_ready=False)
            if validate.returncode != 0 or validation["driver_exits"] != [0]:
                raise HarnessFailure(
                    "fresh pre-sequence validation did not exit successfully"
                )
            if len(validation["results"]) != 1:
                raise HarnessFailure(
                    "fresh pre-sequence validation emitted an invalid result count"
                )
            if validation["ready"] or validation["traces"]:
                raise HarnessFailure(
                    "fresh pre-sequence validation emitted output reserved for another role"
                )
            counters = parse_result(
                validation["results"][0], pre_sequence["case"]
            )
            filesystem = observe_filesystems(
                validation["filesystems"], constraints["mount_options"]
            )
            peak = max(peak, allocated_bytes(case_dir))
        finally:
            stop_process(validate)
        return counters, filesystem, features, peak
    finally:
        if exercise is not None:
            kill_process(exercise)
        for server in servers:
            stop_process(server)
        for socket_root in socket_roots:
            shutil.rmtree(socket_root, ignore_errors=True)


def run_power_cut(
    row: dict[str, Any],
    *,
    case_dir: Path,
    qemu: Path,
    nbdkit: Path,
    kernel: Path,
    initramfs: Path,
    constraints: dict[str, Any],
    deadline: float,
) -> tuple[dict[str, int | float], dict[str, Any], list[str], int]:
    images, features = prepare_disks(case_dir, constraints["disk_bytes_each"])
    servers: list[subprocess.Popen[str]] = []
    sockets: list[Path] = []
    socket_roots: list[Path] = []
    exercise: subprocess.Popen[str] | None = None
    peak = allocated_bytes(case_dir)
    try:
        for index, image in enumerate(images):
            server, socket, socket_root = start_nbdkit(
                nbdkit, image, case_dir / f"cache-exercise-{index}", deadline
            )
            servers.append(server)
            sockets.append(socket)
            socket_roots.append(socket_root)
        exercise = start_qemu(
            qemu,
            kernel,
            initramfs,
            sockets,
            constraints,
            role="exercise",
            case_name=row["case"],
            transition=row["transition"],
            lane=row["lane"],
            edge=row["edge"],
            occurrence=row["occurrence"],
        )
        output = read_qemu(exercise, deadline, stop_on_ready=True)
        if len(output["ready"]) != 1:
            raise HarnessFailure("VM exercise did not emit exactly one ready marker")
        if output["results"] or output["traces"] or output["driver_exits"]:
            raise HarnessFailure("VM exercise emitted output reserved for another role")
        parse_ready(output["ready"][0], row)
        filesystem = observe_filesystems(
            output["filesystems"], constraints["mount_options"]
        )
        peak = max(peak, allocated_bytes(case_dir))
        # This is the power cut: neither process gets a graceful flush or exit.
        deliberate_power_cut(exercise, "transition QEMU")
        exercise = None
        for index, server in enumerate(servers):
            deliberate_power_cut(server, f"transition nbdkit {index}")
        servers.clear()

        validation_sockets: list[Path] = []
        for index, image in enumerate(images):
            server, socket, socket_root = start_nbdkit(
                nbdkit, image, case_dir / f"cache-validate-{index}", deadline
            )
            servers.append(server)
            validation_sockets.append(socket)
            socket_roots.append(socket_root)
        validate = start_qemu(
            qemu,
            kernel,
            initramfs,
            validation_sockets,
            constraints,
            role="validate",
            case_name=row["case"],
            transition=row["transition"],
            lane=row["lane"],
            edge=row["edge"],
            occurrence=row["occurrence"],
        )
        try:
            validation = read_qemu(validate, deadline, stop_on_ready=False)
            if validate.returncode != 0 or validation["driver_exits"] != [0]:
                raise HarnessFailure("fresh VM validation did not exit successfully")
            if len(validation["results"]) != 1:
                raise HarnessFailure("fresh VM validation emitted an invalid result count")
            if validation["ready"] or validation["traces"]:
                raise HarnessFailure("fresh VM validation emitted output reserved for another role")
            counters = parse_result(validation["results"][0], row["case"])
            validate_fs = observe_filesystems(
                validation["filesystems"], constraints["mount_options"]
            )
            if validate_fs != filesystem:
                raise HarnessFailure("filesystem tuple changed across power-cut restart")
            peak = max(peak, allocated_bytes(case_dir))
        finally:
            stop_process(validate)
        return counters, filesystem, features, peak
    finally:
        if exercise is not None:
            kill_process(exercise)
        for server in servers:
            stop_process(server)
        for socket_root in socket_roots:
            shutil.rmtree(socket_root, ignore_errors=True)


def aggregate_counters(target: dict[str, int | float], current: dict[str, int | float]) -> None:
    for name, value in current.items():
        if name in {
            "enqueue_poll_p99_ms",
            "incremental_unchanged_artifact_bytes_rewritten",
            "post_open_or_save_metadata_rss_bytes",
        }:
            target[name] = max(target.get(name, 0), value)
        else:
            target[name] = target.get(name, 0) + value


def success_evidence(
    manifest: dict[str, Any], repo: Path, work: Path, deadline: float
) -> dict[str, Any]:
    constraints = manifest["constraints"]
    if sys.platform != "linux" or os.uname().machine != "x86_64":
        raise HarnessFailure("project-store lifecycle requires Linux x86_64")
    if os.environ.get("GITHUB_ACTIONS") == "true":
        raise HarnessFailure("project-store lifecycle must not run in GitHub Actions")
    if os.environ.get("MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL") != "1":
        raise HarnessFailure(
            "project-store lifecycle requires MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1"
        )
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise HarnessFailure("the designated user cannot read and write /dev/kvm")

    identity = git_identity(repo)
    manifest_path = repo / "tools/project-store-vm/manifest.json"
    fixture = repo / manifest["guest_driver"]["fixture"]
    if not fixture.is_file():
        raise HarnessFailure("project-store VM fixture is missing")
    identity.update(
        {
            "manifest_sha256": sha256_file(manifest_path),
            "fixture_sha256": sha256_file(fixture),
        }
    )

    qemu = command_path("qemu-system-x86_64")
    nbdkit = command_path("nbdkit")
    busybox = command_path("busybox")
    command_path("mkfs.ext4")
    command_path("tune2fs")
    command_path("cpio")
    command_path("gzip")
    package_version(constraints["kernel_package"], constraints["kernel_version"])
    package_version(constraints["busybox_package"], constraints["busybox_version"])
    package_version(constraints["nbdkit_package"], constraints["nbdkit_version"])
    qemu_package_version(constraints["qemu_version"])
    e2fsprogs_version(constraints["e2fsprogs_version"])
    if sha256_file(busybox) != constraints["busybox_sha256"]:
        raise HarnessFailure("BusyBox binary does not match its frozen digest")

    packages = work / "packages"
    kernel_deb = download_deb(
        constraints["kernel_package"],
        constraints["kernel_version"],
        constraints["kernel_deb_sha256"],
        packages / "kernel",
        deadline,
    )
    download_deb(
        constraints["nbdkit_package"],
        constraints["nbdkit_version"],
        constraints["nbdkit_deb_sha256"],
        packages / "nbdkit",
        deadline,
    )
    kernel_root = work / "kernel-package"
    kernel_root.mkdir()
    run_checked(
        ["dpkg-deb", "--extract", str(kernel_deb), str(kernel_root)], timeout=60
    )
    kernel = kernel_root / "boot/vmlinuz-6.17.0-35-generic"
    if not kernel.is_file():
        raise HarnessFailure("frozen guest kernel was absent from its verified package")

    test_binary = build_guest_test(repo, deadline)
    identity["guest_test_sha256"] = sha256_file(test_binary)
    initramfs = build_initramfs(repo, work, test_binary, busybox, fixture)

    tool_facts = {
        "qemu": {
            "package_version": constraints["qemu_version"],
            "binary_sha256": sha256_file(qemu),
        },
        "kernel": {
            "package_version": constraints["kernel_version"],
            "package_archive_sha256": constraints["kernel_deb_sha256"],
            "image_sha256": sha256_file(kernel),
        },
        "busybox": {
            "package_version": constraints["busybox_version"],
            "binary_sha256": constraints["busybox_sha256"],
        },
        "nbdkit": {
            "package_version": constraints["nbdkit_version"],
            "package_archive_sha256": constraints["nbdkit_deb_sha256"],
            "binary_sha256": sha256_file(nbdkit),
        },
        "e2fsprogs": {"version": constraints["e2fsprogs_version"]},
    }

    rows = validate_manifest(manifest)
    qemu_boots = 0
    trace_rows = 0
    working_peak = allocated_bytes(work)
    filesystem: dict[str, Any] | None = None
    features: list[str] | None = None
    for flow in manifest["flows"]:
        case_dir = work / "cases" / f"trace-{flow['id']}"
        case_dir.mkdir(parents=True)
        first = flow["transitions"][0]
        output, observed_features, case_peak = run_complete_guest(
            role="trace",
            case_name=flow["case"],
            transition=first["name"],
            lane=flow["lane"],
            edge="before",
            occurrence=0,
            case_dir=case_dir,
            qemu=qemu,
            nbdkit=nbdkit,
            kernel=kernel,
            initramfs=initramfs,
            constraints=constraints,
            deadline=deadline,
        )
        qemu_boots += 1
        if output["ready"] or len(output["results"]) != 1:
            raise HarnessFailure("VM trace emitted an invalid role result")
        parse_trace_result(output["results"][0], flow["case"])
        selected = {transition["name"] for transition in flow["transitions"]}
        focused_trace = [
            row for row in output["traces"] if row["transition"] in selected
        ]
        trace_rows += len(focused_trace)
        if focused_trace != expected_trace(flow):
            raise HarnessFailure(
                "guest trace disagrees with manifest occurrence coverage"
            )
        observed_fs = observe_filesystems(
            output["filesystems"], constraints["mount_options"]
        )
        if filesystem is None:
            filesystem = observed_fs
            features = observed_features
        elif filesystem != observed_fs or features != observed_features:
            raise HarnessFailure("filesystem facts drifted between scenario baselines")
        working_peak = max(working_peak, case_peak, allocated_bytes(work))
        shutil.rmtree(case_dir)

    counters: dict[str, int | float] = {
        "exact_retry_attempts": 0,
        "pre_sequence_power_cuts": 0,
        "validated_power_cuts": 0,
    }
    pre_sequence = manifest["pre_sequence"]
    pre_sequence_dir = work / "cases/pre-sequence"
    pre_sequence_dir.mkdir(parents=True)
    observed, observed_fs, observed_features, case_peak = run_pre_sequence_cut(
        pre_sequence,
        case_dir=pre_sequence_dir,
        qemu=qemu,
        nbdkit=nbdkit,
        kernel=kernel,
        initramfs=initramfs,
        constraints=constraints,
        deadline=deadline,
    )
    qemu_boots += 2
    aggregate_counters(counters, observed)
    counters["pre_sequence_power_cuts"] = 1
    if filesystem != observed_fs or features != observed_features:
        raise HarnessFailure("filesystem facts drifted during the pre-sequence cut")
    working_peak = max(working_peak, case_peak, allocated_bytes(work))
    shutil.rmtree(pre_sequence_dir)

    matrix_rows: list[dict[str, Any]] = []
    for index, row in enumerate(rows):
        case_dir = work / "cases" / f"cut-{index:03d}"
        case_dir.mkdir(parents=True)
        observed, observed_fs, observed_features, case_peak = run_power_cut(
            row,
            case_dir=case_dir,
            qemu=qemu,
            nbdkit=nbdkit,
            kernel=kernel,
            initramfs=initramfs,
            constraints=constraints,
            deadline=deadline,
        )
        qemu_boots += 2
        aggregate_counters(counters, observed)
        if filesystem != observed_fs or features != observed_features:
            raise HarnessFailure("filesystem facts drifted during the power-cut matrix")
        matrix_rows.append(
            {
                "case": row["case"],
                "transition": row["transition"],
                "lane": row["lane"],
                "edge": row["edge"],
                "occurrence": row["occurrence"],
                "status": "passed",
            }
        )
        working_peak = max(working_peak, case_peak, allocated_bytes(work))
        shutil.rmtree(case_dir)

    performance = manifest["performance"]
    performance_dir = work / "cases/performance"
    performance_dir.mkdir(parents=True)
    performance_output, observed_features, case_peak = run_complete_guest(
        role="validate",
        case_name=performance["case"],
        transition="performance",
        lane=performance["lane"],
        edge="before",
        occurrence=0,
        case_dir=performance_dir,
        qemu=qemu,
        nbdkit=nbdkit,
        kernel=kernel,
        initramfs=initramfs,
        constraints=constraints,
        deadline=deadline,
    )
    qemu_boots += 1
    if len(performance_output["results"]) != 1:
        raise HarnessFailure("performance baseline emitted an invalid result count")
    if performance_output["ready"] or performance_output["traces"]:
        raise HarnessFailure("performance baseline emitted output reserved for another role")
    performance_counters = parse_result(
        performance_output["results"][0], performance["case"]
    )
    aggregate_counters(counters, performance_counters)
    performance_fs = observe_filesystems(
        performance_output["filesystems"], constraints["mount_options"]
    )
    if filesystem != performance_fs or features != observed_features:
        raise HarnessFailure("filesystem facts drifted during the performance baseline")
    working_peak = max(working_peak, case_peak, allocated_bytes(work))

    required_performance = {
        "enqueue_poll_samples",
        "enqueue_poll_p99_ms",
        "incremental_unchanged_artifact_bytes_rewritten",
        "post_open_or_save_metadata_rss_bytes",
    }
    if not required_performance.issubset(counters):
        raise HarnessFailure("performance baseline omitted required counters")
    if (
        counters["enqueue_poll_samples"] < performance["samples"]
        or counters["enqueue_poll_p99_ms"] > performance["enqueue_poll_p99_ms_max"]
        or counters["incremental_unchanged_artifact_bytes_rewritten"]
        > performance["incremental_unchanged_artifact_bytes_rewritten_max"]
        or counters["post_open_or_save_metadata_rss_bytes"]
        > performance["post_open_or_save_metadata_rss_bytes_max"]
    ):
        raise HarnessFailure("project-store performance boundary failed")
    if working_peak > constraints["working_bytes_max"]:
        raise HarnessFailure("VM working allocation exceeded 640 MiB")
    elapsed_ms = int((constraints["timeout_seconds"] - (deadline - time.monotonic())) * 1000)
    if elapsed_ms > constraints["timeout_seconds"] * 1000:
        raise HarnessFailure("VM aggregate timeout exceeded 900 seconds")
    counters.update(
        {
            "elapsed_ms": elapsed_ms,
            "working_bytes_peak": working_peak,
            "qemu_boots": qemu_boots,
        }
    )
    assert filesystem is not None and features is not None
    filesystem["features"] = features
    total_cut_cases = len(rows) + 1
    return {
        "schema": EVIDENCE_SCHEMA,
        "schema_version": 1,
        "result": "passed",
        "failures": [],
        "identity": identity,
        "tools": tool_facts,
        "filesystem": filesystem,
        "harness": {
            "rootless": True,
            "kvm": True,
            "guest_memory_bytes": constraints["guest_memory_bytes"],
            "disk_count": constraints["disk_count"],
            "disk_bytes_each": constraints["disk_bytes_each"],
            "working_bytes_max": constraints["working_bytes_max"],
            "timeout_seconds": constraints["timeout_seconds"],
            "retries": constraints["retries"],
            "power_cut": "qemu-and-nbdkit-sigkill",
            "cross_device_save_as": True,
        },
        "matrix": {
            "scenario_baselines": len(manifest["flows"]),
            "trace_rows": trace_rows,
            "pre_sequence_cut": {
                "case": pre_sequence["case"],
                "lane": pre_sequence["lane"],
                "status": "passed",
            },
            "cut_cases": total_cut_cases,
            "passed_cut_cases": len(matrix_rows) + 1,
            "qemu_kills": total_cut_cases,
            "nbdkit_kills": total_cut_cases * 2,
            "fresh_validations": total_cut_cases,
            "rows": matrix_rows,
        },
        "counters": counters,
    }


def failed_evidence(failure: str) -> dict[str, Any]:
    safe = re.sub(r"[^A-Za-z0-9 ._:+~=()-]", "?", failure)[:256]
    if not safe:
        safe = "project-store lifecycle harness failed"
    return {
        "schema": EVIDENCE_SCHEMA,
        "schema_version": 1,
        "result": "failed",
        "failures": [safe],
        "identity": {},
        "tools": {},
        "filesystem": {},
        "harness": {},
        "matrix": {},
        "counters": {},
    }


def validate_success_evidence(evidence: dict[str, Any], manifest: dict[str, Any]) -> None:
    exact_object(
        evidence,
        {
            "schema",
            "schema_version",
            "result",
            "failures",
            "identity",
            "tools",
            "filesystem",
            "harness",
            "matrix",
            "counters",
        },
        "VM evidence",
    )
    if (
        evidence["schema"] != EVIDENCE_SCHEMA
        or evidence["schema_version"] != 1
        or evidence["result"] != "passed"
        or evidence["failures"] != []
    ):
        raise HarnessFailure("VM evidence result identity drifted")
    rows = validate_manifest(manifest)
    matrix = exact_object(
        evidence["matrix"],
        {
            "scenario_baselines",
            "trace_rows",
            "cut_cases",
            "passed_cut_cases",
            "qemu_kills",
            "nbdkit_kills",
            "fresh_validations",
            "pre_sequence_cut",
            "rows",
        },
        "VM evidence matrix",
    )
    expected_rows = [
        {
            "case": row["case"],
            "transition": row["transition"],
            "lane": row["lane"],
            "edge": row["edge"],
            "occurrence": row["occurrence"],
            "status": "passed",
        }
        for row in rows
    ]
    total_cut_cases = len(rows) + 1
    expected_pre_sequence = {
        "case": manifest["pre_sequence"]["case"],
        "lane": manifest["pre_sequence"]["lane"],
        "status": "passed",
    }
    if (
        matrix["scenario_baselines"] != len(manifest["flows"])
        or matrix["trace_rows"] != len(rows) * 2
        or matrix["pre_sequence_cut"] != expected_pre_sequence
        or matrix["cut_cases"] != total_cut_cases
        or matrix["passed_cut_cases"] != total_cut_cases
        or matrix["qemu_kills"] != total_cut_cases
        or matrix["nbdkit_kills"] != total_cut_cases * 2
        or matrix["fresh_validations"] != total_cut_cases
        or matrix["rows"] != expected_rows
    ):
        raise HarnessFailure("VM evidence matrix counts drifted")
    counters = exact_object(
        evidence["counters"],
        {
            "elapsed_ms",
            "enqueue_poll_p99_ms",
            "enqueue_poll_samples",
            "exact_retry_attempts",
            "incremental_unchanged_artifact_bytes_rewritten",
            "post_open_or_save_metadata_rss_bytes",
            "pre_sequence_power_cuts",
            "qemu_boots",
            "validated_power_cuts",
            "working_bytes_peak",
        },
        "VM evidence counters",
    )
    if (
        counters["pre_sequence_power_cuts"] != 1
        or counters["exact_retry_attempts"] != total_cut_cases
        or counters["validated_power_cuts"] != total_cut_cases
        or counters["qemu_boots"]
        != len(manifest["flows"]) + total_cut_cases * 2 + 1
    ):
        raise HarnessFailure("VM evidence power-cut counters drifted")


def self_test(manifest: dict[str, Any], repo: Path) -> None:
    if "sync" not in INITRAMFS_BUSYBOX_APPLETS:
        raise HarnessFailure("VM guest runtime omits the required sync applet")
    validate_unix_socket_path(Path("/tmp/m4d-nbd-12345678/nbd.sock"))
    try:
        validate_unix_socket_path(Path("/tmp") / ("x" * 108))
    except HarnessFailure:
        pass
    else:
        raise HarnessFailure("VM AF_UNIX path-bound self-test failed")
    require_sigkill_exit(True, -signal.SIGKILL, "self-test")
    for was_running, returncode in [(False, -signal.SIGKILL), (True, 0)]:
        try:
            require_sigkill_exit(was_running, returncode, "self-test")
        except HarnessFailure:
            pass
        else:
            raise HarnessFailure("VM deliberate-SIGKILL self-test failed")
    initial_identity = {"commit": "a" * 40, "tree": "b" * 40, "clean": True}
    require_same_git_identity(initial_identity, dict(initial_identity))
    for drifted_identity in [
        {**initial_identity, "commit": "c" * 40},
        {**initial_identity, "tree": "d" * 40},
        {**initial_identity, "clean": False},
    ]:
        try:
            require_same_git_identity(initial_identity, drifted_identity)
        except HarnessFailure:
            pass
        else:
            raise HarnessFailure("VM stable-git-identity self-test failed")
    init = validate_guest_init(repo).read_text(encoding="utf-8")
    for drifted in [
        init.replace(REAL_FILESYSTEM_POLICY_EXPORT, ""),
        init.replace(REAL_FILESYSTEM_POLICY_EXPORT, f"export {REAL_FILESYSTEM_POLICY_ENV}=0"),
        init.replace(
            REAL_FILESYSTEM_POLICY_EXPORT,
            f"{REAL_FILESYSTEM_POLICY_EXPORT}\nunset {REAL_FILESYSTEM_POLICY_ENV}",
        ),
    ]:
        try:
            validate_guest_init_text(drifted)
        except HarnessFailure:
            pass
        else:
            raise HarnessFailure("VM real-filesystem-policy self-test failed")
    rows = validate_manifest(manifest)
    if len(rows) != 59 or any(row["transition"] == "pre-sequence" for row in rows):
        raise HarnessFailure("VM pre-sequence cut leaked into transition rows")
    for flow in manifest["flows"]:
        trace = expected_trace(flow)
        if not trace or any(parse_trace(dict(row)) != row for row in trace):
            raise HarnessFailure("VM trace self-test failed")
    sample_row = rows[0]
    marker = {
        "schema": READY_SCHEMA,
        "schema_version": 1,
        "role": "exercise",
        "case": sample_row["case"],
        "transition": sample_row["transition"],
        "lane": sample_row["lane"],
        "edge": sample_row["edge"],
        "occurrence": sample_row["occurrence"],
        "status": "ready",
    }
    parse_ready(marker, sample_row)
    result = {
        "schema": RESULT_SCHEMA,
        "schema_version": 1,
        "role": "validate",
        "case": sample_row["case"],
        "status": "passed",
        "counters": {"exact_retry_attempts": 1, "power_loss_simulated": True},
    }
    if parse_result(result, sample_row["case"])["validated_power_cuts"] != 1:
        raise HarnessFailure("VM result self-test failed")
    parse_trace_result(
        {
            "schema": RESULT_SCHEMA,
            "schema_version": 1,
            "role": "trace",
            "case": sample_row["case"],
            "status": "passed",
            "counters": {},
        },
        sample_row["case"],
    )
    performance = {
        "schema": RESULT_SCHEMA,
        "schema_version": 1,
        "role": "validate",
        "case": "performance",
        "status": "passed",
        "counters": {
            "enqueue_samples": 1000,
            "enqueue_p99_nanoseconds": 4_000_000,
            "poll_samples": 1000,
            "poll_p99_nanoseconds": 5_000_000,
            "unchanged_artifact_bytes_rewritten": 0,
            "post_open_or_save_metadata_rss_bytes": 100_663_296,
            "exact_retry_attempts": 0,
            "power_loss_simulated": False,
        },
    }
    if parse_result(performance, "performance")["enqueue_poll_p99_ms"] != 5.0:
        raise HarnessFailure("VM performance result self-test failed")
    total_cut_cases = len(rows) + 1
    sample_evidence = {
        "schema": EVIDENCE_SCHEMA,
        "schema_version": 1,
        "result": "passed",
        "failures": [],
        "identity": {},
        "tools": {},
        "filesystem": {},
        "harness": {},
        "matrix": {
            "scenario_baselines": len(manifest["flows"]),
            "trace_rows": sum(len(expected_trace(flow)) for flow in manifest["flows"]),
            "pre_sequence_cut": {
                "case": manifest["pre_sequence"]["case"],
                "lane": manifest["pre_sequence"]["lane"],
                "status": "passed",
            },
            "cut_cases": total_cut_cases,
            "passed_cut_cases": total_cut_cases,
            "qemu_kills": total_cut_cases,
            "nbdkit_kills": total_cut_cases * 2,
            "fresh_validations": total_cut_cases,
            "rows": [
                {
                    "case": row["case"],
                    "transition": row["transition"],
                    "lane": row["lane"],
                    "edge": row["edge"],
                    "occurrence": row["occurrence"],
                    "status": "passed",
                }
                for row in rows
            ],
        },
        "counters": {
            "exact_retry_attempts": total_cut_cases,
            "pre_sequence_power_cuts": 1,
            "validated_power_cuts": total_cut_cases,
            "enqueue_poll_samples": 1000,
            "enqueue_poll_p99_ms": 5.0,
            "incremental_unchanged_artifact_bytes_rewritten": 0,
            "post_open_or_save_metadata_rss_bytes": 100663296,
            "elapsed_ms": 900000,
            "working_bytes_peak": 671088640,
            "qemu_boots": len(manifest["flows"]) + total_cut_cases * 2 + 1,
        },
    }
    validate_success_evidence(sample_evidence, manifest)
    print(
        "project-store-vm self-test: "
        f"passed flows={len(manifest['flows'])} "
        f"transition_cut_cases={len(rows)} power_cut_cases={total_cut_cases} retries=0"
    )


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    script = Path(__file__).resolve()
    repo = script.parents[2]
    try:
        manifest = read_json(script.with_name("manifest.json"))
    except HarnessFailure as error:
        if args.self_test:
            print(f"project-store-vm self-test failed: {error}", file=sys.stderr)
        else:
            evidence = failed_evidence(str(error))
            print(EVIDENCE_PREFIX + json.dumps(evidence, sort_keys=True, separators=(",", ":")))
        return 1
    if args.self_test:
        try:
            self_test(manifest, repo)
        except HarnessFailure as error:
            print(f"project-store-vm self-test failed: {error}", file=sys.stderr)
            return 1
        return 0

    constraints = manifest.get("constraints", {})
    timeout = constraints.get("timeout_seconds", 900)
    deadline = time.monotonic() + (timeout if isinstance(timeout, int) else 900)
    target = repo / "target/mirante4d/project-store-vm"
    target.mkdir(parents=True, exist_ok=True)
    try:
        validate_manifest(manifest)
        with tempfile.TemporaryDirectory(prefix="run-", dir=target) as encoded:
            evidence = success_evidence(manifest, repo, Path(encoded), deadline)
        validate_success_evidence(evidence, manifest)
        require_same_git_identity(evidence["identity"], git_identity(repo))
    except HarnessFailure as error:
        evidence = failed_evidence(str(error))
        print(EVIDENCE_PREFIX + json.dumps(evidence, sort_keys=True, separators=(",", ":")))
        return 1
    except Exception:
        evidence = failed_evidence("unexpected project-store lifecycle harness failure")
        print(EVIDENCE_PREFIX + json.dumps(evidence, sort_keys=True, separators=(",", ":")))
        return 1
    print(EVIDENCE_PREFIX + json.dumps(evidence, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
