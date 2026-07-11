# CI/CD Specification

Status: DRAFT — forward use blocked by the 2026-07-09 foundation audit
Last updated: 2026-07-10

## Purpose

Define expected continuous integration, verification, and artifact behavior.

This document records the old intended surface and is not authority to enable
the current workflows or attach a runner. The [verification/evidence
brief](../plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md), under
plan 0.21's [implementation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md), contains the
owner-approved D-022/D-023 replacement target; implementation remains
gated by the WP-06 entry brief.

## Scope

This spec covers CI gates, nightly checks, platform strategy, artifact capture, and release-build verification.

## Non-Goals

- Building release installers immediately.
- Depending on local sample data in normal CI.
- Treating CI as a substitute for local targeted testing during development.

## Requirements

- Fast checks should run on every change in CI.
- Full checks should be available before larger merges/releases.
- Render and e2e checks should exist even if some are local-only at first.
- CI failures must remain diagnosable from bounded logs and job summaries; the
  initial target uploads no Actions artifacts.
- CI commands should call the same `xtask` gates used locally.
- Linux is the sole foundation product/release target; Windows and macOS may
  receive non-claiming portability checks only.

The accepted test tooling and gate composition are defined in `TESTING_TOOLING_SPEC.md`.

## CI Provider

Actions was disabled while the clean public root was constructed. The sole
provisional source workflow is
`.github/workflows/bootstrap.yml`: one bounded Linux check for unfiltered pull
requests and a guarded root dispatch. Its first remote execution occurs only
after public cutover; WP-06 owns the replacement verification topology.
Windows and macOS remain bounded portability work without a support or release
claim.

Do not attach the persistent GPU workstation to the public repository as a
self-hosted runner. Trusted GPU/performance/T5/E4 evidence runs locally against
maintainer-selected immutable revisions with no upload credential in the tested
process.

## Current Implementation Authority

This DRAFT does not carry the authoritative current workflow inventory.
Current workflow facts live in the accepted `TESTING_TOOLING_SPEC.md` and are
checked by `cargo xtask workflow-audit`.

The four predecessor workflow files have been deleted. Hosted `verify-fast`,
self-hosted GPU, platform, external-evidence, and artifact-upload workflows are
legacy inventory only, not current CI. If this file disagrees with
[`CURRENT_STATE.md`](../CURRENT_STATE.md), the sole provisional workflow, or
fresh audit evidence, those current-state authorities win. No report can
authorize a hosted run.

## Legacy Gate Inventory

The lists below describe the rejected/current-era intent for audit comparison;
they do not override the approved two-check/six-leaf target in the verification
brief and must not be copied into the future workflow topology.

Every-change gate:

- formatting
- clippy/static checks
- unit tests
- fast integration tests
- golden format tests
- architecture boundary checks

Legacy renderer gate:

- GPU initialization smoke where available
- synthetic render tests
- pixel/snapshot artifacts on failure

Legacy E2E gate:

- app launch
- valid dataset open
- invalid dataset rejection
- tiny TIFF/OME-TIFF import workflow
- project package save/reopen
- analysis table/plot workflows

Legacy nightly gate:

- full and e2e gates
- coverage reporting
- bounded fuzzing
- mutation testing
- stress tests
- benchmark smoke tests
- larger synthetic datasets
- local real sample data only on machines where it is configured

## Artifact Policy

The approved target replacement begins with logs and job summaries only. A separate
post-bootstrap storage decision may later permit pre-sized, short-lived,
sanitized failure bundles such as:

- logs
- screenshots
- rendered images
- benchmark summaries
- hardware diagnostics
- failing fixture outputs where size permits

Do not upload private or huge local sample data by default.

## Invariants

- CI should not hide failing tests.
- CI should not require old-format data.
- Local and CI verification commands should stay aligned.
- GPU-unavailable hosted environments may prove only explicit unsupported-
  adapter/package behavior. Missing/skipped/unsupported output never satisfies
  a required GPU/viewer-evidence gate.

## Failure Modes

- flaky GPU runner
- platform-specific dependency failure
- benchmark noise
- artifact storage too large
- CI command diverges from local command

## Testing Requirements

- `xtask` gates should be runnable locally.
- CI config should be treated as code and reviewed with the same quality bar.
- Failure artifacts should be checked after adding render/e2e tests.

## Open Questions

- Release signing/notarization automation.
