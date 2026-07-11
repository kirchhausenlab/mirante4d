# Testing Tooling Specification

Status: ACCEPTED
Forward implementation: BLOCKED — current closure model rejected by the 2026-07-09 foundation audit
Last updated: 2026-07-10

## Purpose

Define the active testing-tooling contract without duplicating the exact
`xtask` command inventory.

## Authority

- Exact command names, arguments, help text, and environment variables are
  authoritative in `cargo xtask --help`.
- Command classification is authoritative in `cargo xtask command-audit`.
- `cargo xtask report-audit` describes the current report inventory but is not
  authoritative completion proof: the foundation audit found that it can
  accept stale, failed, or missing required evidence.
- Native process/CI results and requirement-specific current evidence remain
  authoritative over any summary report or waiver machinery.
- `docs/TESTING.md` is the concise human-facing verification policy.

The [verification/evidence
brief](../plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md), under
the [implementation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md),
contains the owner-approved D-022/D-023 replacement target. The current command
descriptions below are factual inventory, not endorsement of the current
topology as the future verification architecture.

## Required Tooling Shape

- Use `cargo nextest` as the normal workspace test runner.
- Use `cargo xtask verify-bootstrap` as the explicitly temporary normal check.
  `verify-fast` remains a known failing legacy command and is not advertised as
  a trustworthy closure gate. WP-06 replaces both the bridge and old topology.
- Keep broader gates for full verification, dependency policy, render/GPU
  verification, UI snapshots/semantic coverage, e2e workflows, coverage, and
  nightly/deep runs.
- Keep benchmark and audit commands report-backed and versioned.
- Keep product validation separate from smoke tests, virtual-window tests,
  render readbacks, and benchmark reports.
- Keep command help and command-audit classification synchronized in the same
  change when adding, removing, or changing an `xtask` command.

## Evidence Classes

- Unit, integration, property, fuzz, mutation, snapshot, render, UI, e2e,
  benchmark, audit, CI, smoke, and product-validation evidence must remain
  distinguishable.
- Smoke tests and preflight reports are supporting evidence only.
- Product-open validation requires the normal native app, a real package or
  accepted generated scenario, non-preflight execution, relevant workflow
  coverage, and inspected logs/artifacts.
- Heavy local evidence must stay opt-in and must not be required by normal CI.

## Reports

Current report families include:

- command inventory and classification
- curated baseline policy
- workflow surface policy
- external CI evidence
- completion waivers
- report-audit inventory/readiness
- render, UI, e2e, benchmark, comparison, packaging, and product-validation
  reports

Reports must include schema/version metadata, status, failure reasons when
applicable, and enough context to diagnose stale, weak, missing, or quarantined
evidence.

Comparison reports for the Neuroglancer-style 2D runtime must remain
report-backed. They must fail on missing operation latency samples, missing
Neuroglancer memory/performance fields, missing Mirante Performance Gate
Contract coverage fields, old-path 2D fallback evidence, or latency ratios
outside the accepted policy.

A passing comparison report proves only that the supplied artifacts satisfy the
comparison schema and configured numeric policy. It is not accepted external
latency evidence unless the Neuroglancer measurement method is itself accepted.
The local screenshot-completion Neuroglancer artifact is explicitly not valid
final latency comparison evidence.

## CI Policy

- Hosted CI is authorized only for the public repository, on free standard
  runners, under the bounded checked-in workflow and the organization `$0`
  hard stop.
- No self-hosted workflow or trusted workstation may be attached to the public
  repository. Trusted GPU/product evidence runs locally against
  maintainer-selected immutable revisions outside the public runner pool.
- External CI evidence cannot compensate for a failing primary gate or weak,
  stale, missing, or invalid requirement evidence.
- Normal CI must not depend on private `MIRANTE4D_SAMPLE_DATA` contents.

## Invariants

- Documentation must not present an inert help command as evidence-producing.
- Documentation must not list stale command names as active commands.
- A passed report with stale revision, weak evidence type, preflight-only
  product validation, or missing required artifacts does not prove completion.
- Current report-audit completion blockers must be reflected in `BACKLOG.md`
  and `CURRENT_STATE.md` when they affect active milestone status.

## Failure Modes

- command help and command-audit disagree
- docs duplicate and drift from command help
- report-audit accepts stale, missing, malformed, weak, or quarantined evidence
- smoke/preflight evidence is described as product-open validation
- external CI evidence is claimed for dirty or different revisions
- private sample data becomes a normal CI dependency

## Testing Requirements

For tooling changes, run the relevant subset of:

```bash
cargo xtask --help
cargo xtask command-audit
cargo xtask baseline-audit
cargo xtask workflow-audit
cargo xtask report-audit
```

Run heavier verification only when the changed tooling surface requires it.
