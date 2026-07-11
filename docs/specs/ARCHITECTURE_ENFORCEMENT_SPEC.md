# Architecture Enforcement Specification

Status: DRAFT — current inventory; forward gate topology superseded by plan 0.21
Last updated: 2026-07-10

## Purpose

Define checks that should enforce project architecture rules automatically.

## Scope

This spec covers dependency direction checks, forbidden imports, monolith detection, generated artifact checks, and verification gate integration.

## Non-Goals

- Replacing human/agent judgment.
- Blocking useful refactors with arbitrary line-count rules.
- Enforcing compatibility with old code.

## Requirements

- Architecture checks should run through `xtask`.
- Forbidden dependency directions should fail verification.
- Lower-level crates must not import app/UI crates.
- Renderer must not directly read arbitrary dataset files.
- Format crate must not depend on renderer or app crates.
- Generated heavy data must not be committed accidentally.
- Exceptions must be explicit and documented.

## Current Implementation Status

`cargo xtask verify-fast` currently runs `xtask` architecture checks before
formatting, clippy, and tests. This is factual inventory, not a trustworthy
closure prescription: the 2026-07-09 foundation audit found `verify-fast`
failing before tests. The owner-approved target routes architecture policy
through the nonrecursive `policy` leaf defined by the [verification
brief](../plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md); that
replacement remains unimplemented and is owned by WP-06.

Implemented checks:

- required crate directories exist
- empty reserved future crates are rejected
- normal Cargo dependencies between Mirante4D crates follow the documented dependency direction
- non-app product/library crates are scanned for UI/app imports such as `egui`, `eframe`, `rfd`, `egui_kittest`, and `mirante4d_app`
- renderer source is scanned for direct filesystem I/O patterns
- tracked repository files are checked for generated/local artifact paths such as `target/`, `.nextest/`, and `sample_data/`
- tracked large microscopy/generated data files above the current policy threshold are rejected

`xtask` itself is excluded from source-pattern scanning because it contains the policy implementation and tests that name forbidden patterns. It remains covered by crate dependency policy.

## Candidate Checks

- crate dependency graph check: implemented for normal Mirante4D dependencies through Cargo manifest scanning
- source import path scan: implemented for UI/app imports outside the app crate and direct renderer filesystem I/O
- generated artifact path check: implemented for tracked generated/local paths and large generated data files
- forbidden module name scan for dumping grounds
- large file warning report
- no old-format reader identifiers unless explicitly approved
- no commented-out code blocks as compatibility placeholders

## Invariants

- Checks should be deterministic.
- Checks should provide actionable error messages.
- Implemented checks stay discoverable in the current command inventory until
  the approved hard cutover assigns each one exactly once to the `policy` leaf.
- Warnings can exist, but architectural violations should fail.

## Failure Modes

- false positive check blocks legitimate code
- false negative misses coupling
- check diverges from docs
- check becomes too slow for fast gate

## Testing Requirements

- `xtask` architecture check should have fixture tests where practical.
- Current CI is disabled. The approved future public CI topology runs
  architecture checks through the nonrecursive `policy` leaf, not through the
  rejected aggregate `verify-fast` topology.
- New exceptions should update docs and tests.

## Open Questions

- Whether to promote monolith/file-size reporting from documentation guidance into an automated warning.
- How to express temporary exceptions.
