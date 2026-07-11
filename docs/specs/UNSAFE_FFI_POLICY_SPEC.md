# Unsafe And FFI Policy Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define when unsafe Rust and foreign-function interfaces are allowed.

## Scope

This spec covers `unsafe`, native library bindings, GPU/vendor interop, memory-mapped files if used unsafely, and platform APIs.

## Non-Goals

- Banning all dependencies that internally use unsafe.
- Allowing unsafe for convenience.
- Adding vendor-specific native paths before benchmarks justify them.

## Requirements

- Project-authored unsafe code is forbidden by default.
- Any unsafe block must be isolated, reviewed, and documented with a `SAFETY` comment.
- Unsafe modules must expose safe typed APIs to the rest of the codebase.
- FFI must live behind narrow boundary crates/modules.
- FFI use must include tests or validation harnesses appropriate to the boundary.
- Vendor-specific interop must be explicit and optional unless accepted as a project baseline by decision record.

## Allowed Cases

Potential allowed cases after review:

- required platform API with no safe wrapper
- proven performance-critical memory mapping
- GPU/native interop with measured benefit
- dependency boundary requiring unsafe callback or pointer handling

## Invariants

- No casual unsafe.
- No unsafe in UI/app orchestration.
- No unsafe without a local safety argument.
- No FFI that leaks raw pointers or lifetimes into broad app code.
- No CUDA/native interop path unless explicitly decided.

## Failure Modes

- undefined behavior
- lifetime or aliasing bug
- platform-specific crash
- memory corruption
- hard-to-test native boundary

## Testing Requirements

- Safe wrapper tests.
- Boundary stress tests.
- Platform-specific checks where applicable.
- Miri or sanitizer checks where practical for unsafe-heavy code.

## Open Questions

- Whether memory mapping is needed for dataset reads.
- Whether any future native codec requires FFI.
- Whether optional CUDA ever becomes justified.

