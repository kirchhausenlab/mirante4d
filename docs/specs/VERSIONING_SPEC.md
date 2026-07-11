# Versioning Specification

Status: DRAFT
Last updated: 2026-06-30

## Purpose

Define version identities used by Mirante4D.

## Scope

This spec covers app version, dataset format version, preprocessing version, project/session version, benchmark schema version, and diagnostics schema version.

## Non-Goals

- Backward compatibility promises.
- Multi-version native readers in the core app.
- Silent migrations.

## Required Version Identities

- Application version.
- Native dataset format string: `mirante4d-v1`.
- Preprocessing pipeline version.
- Dataset writer version.
- Project/session file version.
- Benchmark output schema version.
- Diagnostics schema version.

Current identities:

- Application version: Cargo package version.
- Native dataset format: `mirante4d-v1`.
- Project/session format: `mirante4d-project-v14`.
- Preferences format: `mirante4d-preferences-v1`.
- Analysis table artifact format: `mirante4d-analysis-table-v1`.
- Analysis plot artifact format: `mirante4d-analysis-plot-v1`.
- Diagnostics format: `mirante4d-diagnostics-v1`.

## Hard-Cutover Behavior

Versioning exists to make rejection explicit, not to preserve old behavior by default.

When the native dataset format changes incompatibly during greenfield development:

- update the accepted format identity
- update specs and tests
- delete old reader code
- reject old data clearly
- use separate converters only if explicitly requested

## Invariants

- Every persisted format must have an explicit version.
- Unsupported persisted versions fail clearly.
- Version changes must be documented in `../DECISIONS.md` or the relevant spec.
- Test fixtures must declare the version they exercise.

## Failure Modes

- dataset produced by old local build
- benchmark output schema drift
- project/session file drift
- unclear error for unsupported version
- stale fixture version

## Testing Requirements

- Version rejection tests.
- Valid current-version acceptance tests.
- Fixture version checks.
- Benchmark schema version tests.

## Open Questions

- Exact app versioning scheme.
- How preprocessing version is represented in the manifest.
