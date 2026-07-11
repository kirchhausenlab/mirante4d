# Testing And Verification

Last updated: 2026-07-11

Mirante4D uses verification-first development. A change is not complete because
code or docs were edited; it is complete only when the relevant behavior has
been verified and the evidence is reported.

## Evidence Classes

- **Implemented**: code or documentation changes are present.
- **Automated-verified**: relevant automated gates, tests, audits, snapshots,
  benchmarks, or reports passed.
- **Product-validated**: the actual interactive native app was opened and
  exercised on the relevant dataset/workflow, either by direct human interaction
  or by an accepted real-display product-automation report that covers the
  workflow.

Do not collapse these labels. Smoke tests, virtual-display automation,
preflight reports, benchmarks, render readbacks, and UI snapshots are useful
evidence, but they are not product-open validation.

## Normal Gates

The temporary normal local command is:

```bash
cargo xtask verify-bootstrap
```

It runs formatting, workspace compilation, a frozen 169-test CPU subset, and
active Markdown/local-link checks. Each phase has a subprocess ceiling and the
test phase has zero retries. It deliberately excludes the complete suite,
Clippy, doctests, dependencies, GPU, UI snapshots, E2E, packaging, performance,
real data, and product-open validation.

The older topology remains untrusted: `verify-fast` fails its pre-test source-
size check, `verify-full` recursively duplicates it, report auditing has a
blocking mismatch, and the full suite is too slow and integration-heavy. Do
not represent either legacy aggregate or the temporary bridge as final
verification architecture.

The [verification/evidence brief](plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md),
under plan 0.21's sole [implementation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md),
contains the owner-approved D-022/D-023 replacement target: two required
public CPU checks over six nonrecursive leaves, cache/artifact-free startup,
pairwise-independent T1 byte-producer/fact-oracle/reader lineages, generated T2
support cases, exact shard/object proof, an E0-E4 packaged-product ladder,
fixed-HW statistical qualification, zero automatic retries, and trusted local
GPU/T5/E4 execution without registering the workstation as a public-repository
runner. This is approved but unimplemented target policy, not current command
or completion authority.

Additional scoped commands:

```bash
cargo xtask verify-render
cargo xtask verify-ui
cargo xtask verify-e2e
cargo xtask report-audit
```

Supporting gates and evidence commands:

```bash
cargo xtask verify-fast
cargo xtask verify-full
cargo xtask verify-coverage
cargo xtask verify-nightly
cargo xtask verify-deps
cargo xtask command-audit
cargo xtask baseline-audit
cargo xtask baseline-refresh-plan [benchmark-report-root]
cargo xtask baseline-promote <current-benchmark.json> <baseline-name.json>
cargo xtask baseline-promote-manifest <promotion-manifest.json>
cargo xtask workflow-audit
cargo xtask external-ci-evidence
cargo xtask completion-waiver
cargo xtask bench-smoke
cargo xtask bench-runtime-stress
cargo xtask bench-native-package <native-package.m4d>
cargo xtask bench-import-sample <experiment>
cargo xtask bench-check <current-benchmark.json> <baseline-benchmark.json>
cargo xtask neuroglancer-compare <comparison-manifest.json>
cargo xtask package-dev
cargo xtask package-linux-release
```

The sanitized
[`pre-foundation-verification-disposition.json`](plans/active/foundation-refactor/manifests/pre-foundation-verification-disposition.json)
retains all 1,055 pre-foundation record identities and their removal/retention
dispositions without publishing pre-public revision or machine bindings. It is
an inventory for WP-06 reconciliation, not proof that those surfaces pass.

Renderer and UI gates:

- `verify-render` requires a usable non-CPU GPU adapter and writes a report
  under `target/mirante4d/verify-render/`.
- `verify-ui` separates semantic UI-tree coverage from visual snapshot coverage
  and writes a report under `target/mirante4d/verify-ui/`.
- `verify-e2e` separates library workflow tests, virtual-window automation, and
  real-window product automation where available.

## Product Automation

The main product automation command is:

```bash
cargo xtask product-validate [native-package.m4d] [scenario]
```

Current scenarios:

- `generated_fixture_camera_smoke`
- `generated_fixture_render_modes`
- `t5_qual_001_interaction_mip`
- `t5_qual_001_interaction_render_modes`
- `t5_qual_001_interaction_continuous`
- `t5_qual_001_four_panel_cross_section`
- `t5_qual_001_four_panel_fine_scale`
- `t5_qual_001_four_panel_continuous_cross_section`
- `t5_qual_002_four_panel_timepoint`
- `t5_qual_002_four_panel_autoplay`
- `custom_script`

`t5_qual_001_four_panel_cross_section` is the heavy four-panel refactor scenario. A
preflight run of this scenario proves script generation, package identity, and
report wiring only. It becomes product-open evidence for the workflow it covers
only when the normal native app launches on a real display, the report status
is `passed`, the scenario covers the claimed four-panel workflow, and
logs/artifacts are inspected. The current passed report covers the first
product four-panel T5-QUAL-001 workflow when paired with the passed
`t5_qual_001_interaction_mip` `Single3d` comparison.

`t5_qual_001_four_panel_fine_scale` is the heavy fine-scale four-panel 2D scenario for
the Neuroglancer-style runtime rewrite. It opens the local T5-QUAL-001 dataset,
switches to four-panel view, zooms the `XZ` panel until the 2D target/render
scale is `s0`, asserts `XY`, `XZ`, and `YZ` have current displayed schedules
with zero missing occupied visible chunks, captures a GPU display artifact,
returns to `Single3d`, and orbits the 3D view. It counts as product-open
evidence only when the normal native app runs on a real display and the report
status is `passed`.

`t5_qual_001_four_panel_continuous_cross_section` is the heavy continuous 2D
interaction scenario for the Neuroglancer-style runtime rewrite. It opens the
local T5-QUAL-001 dataset, switches to four-panel view, repeatedly drives `XZ`
cross-section rotate, slice-step, pan, and cursor zoom commands, asserts all
three `XY`, `XZ`, and `YZ` displayed panel GPU frames remain nonblank during
the burst, waits for settled current schedules with zero missing occupied
visible chunks, captures GPU display artifacts, returns to `Single3d`, and
orbits the 3D view. It counts as product-open evidence only when the normal
native app runs on a real display and the report status is `passed`.

`t5_qual_002_four_panel_timepoint` is the heavy T5-QUAL-002 four-panel timepoint scenario.
It opens the local T5-QUAL-002 time dataset, switches to four-panel view, verifies
`XY`, `XZ`, and `YZ` 2D stream keys at timepoints `0`, `1`, and `2`, captures
nonblank GPU display artifacts, returns to `Single3d`, and requires the normal
native app to pass on a real display before it counts as product-open evidence.

`t5_qual_002_four_panel_autoplay` is the heavy T5-QUAL-002 four-panel autoplay scenario.
It starts from a settled four-panel view at timepoint `0`, enables the normal
movie playback control, observes playback-driven timepoint changes without
scripted `set_timepoint` or `step_timepoint` commands, stops playback, waits
for `XY`, `XZ`, and `YZ` to settle on the active timepoint, asserts the 2D
streams match that active timepoint, captures nonblank GPU display artifacts,
returns to `Single3d`, and requires the normal native app to pass on a real
display before it counts as product-open evidence.

T5-QUAL-001 and T5-QUAL-002 local-sample scenarios are heavy local evidence and require:

```bash
MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
```

Useful controls:

```bash
MIRANTE4D_PRODUCT_VALIDATE_PREFLIGHT_ONLY=1
MIRANTE4D_PRODUCT_VALIDATE_ALLOW_NO_DISPLAY=1
MIRANTE4D_PRODUCT_VALIDATE_SKIP_RELEASE_BUILD=1
MIRANTE4D_PRODUCT_VALIDATE_MAX_RSS_BYTES=<bytes>
MIRANTE4D_PRODUCT_VALIDATE_GPU_TIMESTAMPS=1
MIRANTE4D_PRODUCT_VALIDATE_SCRIPT=<script.json>
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display|virtual_display
```

Preflight output, virtual-display runs, and no-display `unsupported` reports are
not product-open validation. They are useful to inspect scripts, dataset
identity, limits, and report paths before launching heavy workflows.

## Neuroglancer Comparison

The four-panel 2D runtime rewrite uses a report-backed comparison command after
real Mirante product-validation reports and equivalent Neuroglancer
measurements exist:

```bash
cargo xtask neuroglancer-compare <comparison-manifest.json>
```

The manifest schema is `mirante4d-neuroglancer-comparison-input` v1. It names
one or more Mirante product-validation reports and one
`neuroglancer-cross-section-performance-measurement` v1 JSON file. The command
does not launch either viewer; it verifies report compatibility and writes a
strict comparison report under `target/mirante4d/neuroglancer-comparison/` by
default. Passing comparison evidence still depends on prior real-display
Mirante product validation and real Neuroglancer measurements from the same
machine or an explicitly documented equivalent environment.

The Neuroglancer measurement JSON must include:

- `operations` entries for the required operation set, with positive
  `sample_count` and `p95_ms`.
- `memory.peak_rss_bytes`, `memory.gpu_resident_bytes`,
  `memory.chunk_cache_bytes`, and `memory.measurement_notes`.
- `performance` fields for first current partial latency, visible-chunk
  planning, candidate/emitted chunk counts, queue and chunk-state counts,
  upload bytes/time, render/UI frame time, CPU RSS, GPU-resident chunk bytes,
  panel target bytes, and eviction count.

The Mirante reports named by the manifest must be passed product-validation
reports from the global chunked 2D path. Their wrapper metrics must expose the
same Performance Gate Contract coverage, including
`display_refresh_timing_summary.phases_ms.gpu_upload` and
`cross_section_runtime.latest_visible_work_assertion.panel_resources[].candidate_chunks`.
Older reports generated before those telemetry fields were added must be
rerun before they can pass `neuroglancer-compare`.

The existing local Neuroglancer comparison artifacts are retained for harness
debugging and schema coverage only:

- manifest:
  `target/mirante4d/neuroglancer-comparison/real-comparison-manifest.json`
- Neuroglancer measurement:
  `target/mirante4d/neuroglancer-comparison/real-neuroglancer-measurement.json`
- comparison report:
  `target/mirante4d/neuroglancer-comparison/real-neuroglancer-comparison-report.json`

That measurement uses the local checkout
`/external/neuroglancer` and records browser screenshot
completion after state mutation. It is a real local Neuroglancer run, but it is
not a patched internal first-current-partial timestamp; uninstrumented Chrome
RSS and fine-grained Neuroglancer GPU/planning fields are recorded explicitly as
null with measurement notes. It is **not accepted final latency comparison
evidence** and must not be cited as proof that Mirante matches or beats
Neuroglancer. A valid comparison must instrument Neuroglancer in-browser from
state/input mutation to first visible/current-partial slice presentation, use
matched LOD and viewport conditions, and exclude screenshot/readback from the
measured interval.

## Product-Open Validation

For renderer, viewport, GPU, data-loading, interaction, or large-dataset work,
automated gates are supporting evidence only. Before calling such work
complete, unless explicitly waived:

- launch the actual interactive `mirante4d-app` native window
- open the relevant real native package
- exercise the changed workflow through real UI interaction or through an
  accepted real-display product-automation scenario/script
- verify the app remains alive without panic, WGPU validation error, render
  retry loop, crash, or hidden fallback
- inspect logs
- report dataset, render mode, interactions, outcome, and log findings

Automation satisfies this gate only when it launches the normal native app, is
non-preflight, reports `passed`, records `real_display`, opens the relevant
native package, covers the changed workflow, and produces logs/artifacts that
were inspected. Manual UI exercise is still required when no accepted automation
scenario covers the workflow or the active plan requires human inspection.

Do not use `MIRANTE4D_APP_SMOKE`, `cargo xtask app-smoke`, benchmarks, or JSON
report generation as substitutes for this gate.

## Local Dataset Paths

Private workstation datasets are resolved locally and are not documented in
the public tree. They are not CI fixtures and must not be committed.

## Reporting Expectations

Final responses for implementation work must list:

- changed behavior
- checks run
- result of each check
- skipped or impossible checks
- residual risk
