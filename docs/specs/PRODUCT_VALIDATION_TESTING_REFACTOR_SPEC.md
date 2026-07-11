# Product Validation And Testing Refactor

Status: ACCEPTED
Implementation: BLOCKED — current closure model rejected by foundation audit
Last updated: 2026-07-10

## Purpose

Define the active testing/evidence refactor that keeps automated verification,
benchmark evidence, product automation, CI evidence, waivers, and real
product-open validation distinct.

## Current Implemented Foundation

- Env-gated semantic native app automation through the real eframe update loop.
- `cargo xtask product-validate [native-package.m4d] [scenario]`.
- Scenario-scoped product-validation reports and artifacts.
- Generated-fixture camera and render-mode scenarios.
- Heavy opt-in T5-QUAL-001 interaction scenarios:
  `t5_qual_001_interaction_mip`, `t5_qual_001_interaction_render_modes`, and
  `t5_qual_001_interaction_continuous`.
- Heavy opt-in four-panel T5-QUAL-001 scenario:
  `t5_qual_001_four_panel_cross_section`. This scenario belongs to the four-panel
  cross-section work. Current implementation claims live in
  [`CURRENT_STATE.md`](../CURRENT_STATE.md); this testing spec makes no broader
  incomplete/complete claim. Preflight evidence for this scenario is
  script/package evidence only.
- `custom_script` scenario using a versioned automation script.
- Report-backed `verify-render`, `verify-ui`, and `verify-e2e`.
- Command, baseline, workflow, external-CI, completion-waiver, and report audit
  surfaces.
- Typed `Uint8`, `Uint16`, and `Float32` benchmark measurement paths.
- Heavy benchmark/product evidence opt-in through
  `MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1`.
- Product-validation preflight mode for heavy scenarios.
- Resource-limit, timing, viewport-artifact, stdout/stderr, display-class, and
  dataset-identity metadata in product-validation reports.

## Current Audit Status

The 2026-07-09 foundation audit found that external CI is not the sole remaining
blocker. The primary fast gate fails before tests; `verify-full` duplicates the
fast suite; report auditing can accept stale, failed, or missing evidence; the
test topology is too slow and integration-heavy; product automation is not
full black-box E2E; and current performance baselines are not credible gates.

The [verification/evidence
brief](../plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md), under
plan 0.21's [implementation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md), contains the
owner-approved D-022/D-023 replacement topology, independent-
oracle, packaged-E2E, statistical, failure, and zero-cost CI target. It does not
authorize implementation. Until an approved replacement handoff exists, this
refactor's implemented code remains current fact but its old “external evidence
only” closure path must not resume or be cited as foundation readiness.

Any interim evidence run must be zero-paid-compute, revision-bound, explicitly
required, and incapable of overriding a failed native CI/process result.

## Evidence Rules

- A smoke test is not product-open validation.
- Virtual-display automation is not proof of real desktop/GPU presentation
  timing.
- Preflight-only T5-QUAL-001 reports are script/metadata evidence, not product-open
  evidence.
- A product-validation report counts as product-open evidence only when it is
  non-preflight, passed, real-display, launches the normal native app, opens the
  relevant package, covers the claimed workflow, and has inspectable logs and
  artifacts.
- Workflow-audit is static CI configuration evidence, not proof that CI ran.
- Completion waivers are explicit exceptions, not product, benchmark, or CI
  evidence.
- Render timing is not interaction FPS unless the measurement actually covers
  interaction or presentation.

## Commands

Core gates:

```bash
cargo xtask verify-fast
cargo xtask verify-render
cargo xtask verify-ui
cargo xtask verify-e2e
cargo xtask report-audit
```

Product validation:

```bash
cargo xtask product-validate [native-package.m4d] [scenario]
```

Heavy T5-QUAL-001 product validation must be deliberate, bounded, and opt-in:

```bash
MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1 \
MIRANTE4D_PRODUCT_VALIDATE_MAX_RSS_BYTES=4294967296 \
MIRANTE4D_PRODUCT_VALIDATE_GPU_TIMESTAMPS=1 \
cargo xtask product-validate \
  "${MIRANTE4D_SAMPLE_DATA}/preprocessed_datasets/phase20-extreme-T5-QUAL-001.m4d" \
  t5_qual_001_interaction_continuous
```

## Exit Criteria

This refactor cannot be called complete under its former closure model. It must
either be superseded by the approved foundation verification work packages or
receive a separately approved corrective plan that addresses every audit
finding above; a clean `report-audit` result alone is insufficient.
