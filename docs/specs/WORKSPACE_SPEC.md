# Workspace Specification

Status: ACCEPTED
Last updated: 2026-07-10

## Purpose

Define the initial Rust workspace shape and local automation contract.

## Scope

This spec covers crate layout, Rust toolchain policy, common commands, local automation, and generated artifact locations.

## Non-Goals

- Implementing the workspace in this documentation step.
- Locking every crate name forever.
- Adding compatibility crates for old `llsm_viewer` behavior.
- Adding a user-facing CLI or headless product path.

## Requirements

- The repository should use a Cargo workspace.
- The Rust toolchain should be pinned with `rust-toolchain.toml`.
- The workspace should use one current Rust edition, expected to be Rust 2024 unless a concrete blocker appears.
- Formatting, linting, tests, architecture checks, and benchmarks should be runnable through stable commands.
- Automation commands should live in an `xtask` crate or equivalent repo-local tool.
- Generated artifacts must be ignored unless explicitly curated as fixtures or reports.

## Historical Bootstrap Layout

The original first implementation created only crates with immediate code:

```text
crates/
  mirante4d-app/
  mirante4d-core/
  mirante4d-renderer/
  mirante4d-data/
  mirante4d-format/
  xtask/
```

Do not create empty future crates just to reserve names.

## Current Layout

```text
crates/
  mirante4d-app/
  mirante4d-analysis/
  mirante4d-core/
  mirante4d-data/
  mirante4d-format/
  mirante4d-import/
  mirante4d-renderer/
  xtask/
```

Current responsibilities:

- `mirante4d-app`: GUI shell and application orchestration.
- `mirante4d-analysis`: hover, measurement, scene artifacts, tables, plots,
  and analysis helpers.
- `mirante4d-core`: shared domain types, math, units, coordinates, and small pure utilities.
- `mirante4d-data`: runtime dataset access, caches, scheduling, decompression, and prefetch.
- `mirante4d-format`: native format validation, manifest parsing, binary indexes, and writers.
- `mirante4d-import`: GUI-backed TIFF/OME-TIFF source import and native dataset generation internals.
- `mirante4d-renderer`: `wgpu` renderer and GPU resource contracts.
- `xtask`: repo automation commands for developers, agents, and CI. This is not a user-facing Mirante4D product mode.

This is the factual current workspace, not the approved-but-unimplemented
refactor target. The [workspace architecture
brief](../plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md), under
the [foundation implementation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md), contains
the owner-approved D-017 ownership-crate/deletion boundary. Until the eventual
handoff is separately approved and implemented, the crate list above remains
current authority.

## Baseline Commands

Commands established by the first workspace milestone and still useful for local development:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --all-targets
cargo test --workspace --doc
cargo xtask verify-fast
cargo xtask generate-fixture basic-u16-16cube
cargo xtask run-dev
```

`verify-fast` remains the canonical advertised command in the current command
surface. The 2026-07-09 foundation audit found that it fails before tests and is
not trustworthy closure evidence. Until an approved WP-01 bridge exists, use
only explicitly scoped zero-cost diagnostics; the foundation handoff defines
the later replacement. Do not infer readiness from the canonical command name.
Broader automation is described below, with exact command inventory delegated
to command help and command-audit.

## Current Automation Surface

The exact implemented `xtask` command surface is intentionally not duplicated
here. Use these authoritative command inventories instead:

```bash
cargo xtask --help
cargo xtask command-audit
```

`cargo xtask command-audit` classifies each command by family, evidence class,
default safety, heavy opt-in requirement, product-evidence role, stale/unsafe
status, and report paths.

Current command families include:

- verification gates: fast, full, dependency, GPU render, UI, e2e, coverage,
  and nightly gates
- fixture and developer launch helpers
- Linux packaging gates
- bounded, local, renderer, interaction, import, phase-audit, and baseline
  benchmark commands
- product-validation automation
- command, baseline, workflow, report, external-CI, and completion-waiver
  evidence commands

Docs that need the full command list should point to `cargo xtask --help` or
the generated command-audit report instead of maintaining a parallel list.
There is no reserved placeholder command for a generic `cargo xtask bench`;
new commands should be added only when they perform real work and update the
command help plus command-audit classification in the same change.

## Invariants

- Lower-level crates must not depend on app/UI crates.
- Renderer code must not directly read arbitrary dataset files.
- Format code must not depend on renderer code.
- `xtask` should be the shared interface for agents, developers, and CI.
- `xtask` must not grow into a user-facing headless Mirante4D app.
- `run-dev` is developer automation for launching the GUI with a generated fixture; it is not a user-facing CLI product.
- `verify-deps` requires `cargo-deny` for advisory checks and must fail clearly if the tool is missing.
- No generated heavy data should be committed accidentally.

## Failure Modes

- dependency cycle between crates
- local command diverges from CI command
- generated data committed by accident
- app crate becomes a monolith
- workspace toolchain drift

## Testing Requirements

- The current `verify-fast` command factually aggregates format, lint, unit,
  integration, and architecture checks, but it is not a passing closure gate.
  The approved foundation target replaces this recursive/aggregate shape with
  six nonrecursive leaves; do not preserve the aggregate merely for
  compatibility.
- Workspace crate boundaries should be tested by architecture checks.
- `xtask` commands should have smoke tests or self-checks where practical.

## Open Questions

None at this time.
