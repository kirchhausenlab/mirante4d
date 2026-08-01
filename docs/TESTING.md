# Testing And Validation

Last updated: 2026-08-01

Testing exists to expose meaningful failures quickly. It is not a measure of
project seriousness, and a large passing suite is not a substitute for using
the product.

## Claim Language

- **Implemented:** the change exists in the named revision.
- **Automated-verified:** the named automated checks passed for that revision.
- **Product-validated:** the normal native application ran on a real mapped
  display and the affected workflow was exercised on relevant data and
  hardware, with its output and logs inspected.

Do not collapse these claims. Unit tests, snapshots, virtual-display
automation, benchmarks, render readbacks, and protocol receipts are supporting
evidence. They do not establish that the visible product works.

Historical qualification claims remain bound to their named revisions. Later
changes do not inherit them automatically, and routine development does not
need to reproduce them.

## Verification Tiers

Use the smallest tier that can falsify the change while iterating, then close
the work with the checks appropriate to the affected boundary.

### Tier 0: Documentation Or Static Metadata

Run:

```bash
cargo xtask docs-check
```

Use `cargo xtask verification-sync --check` as well when verification metadata
changes.

### Tier 1: Focused Development

For Rust changes, format the workspace and run the narrowest useful package,
module, or named test:

```bash
cargo fmt --all
cargo test -p <affected-package> <focused-test-name>
```

Prefer a few tests with independent expected results over duplicated matrices,
large snapshots, or tests that merely reproduce the implementation.

### Tier 2: Public Pull-Request Checks

Before handing off a substantial cross-cutting change, run:

```bash
cargo xtask verify-pr
```

The protected repository has two public checks:

- `PR / policy` validates repository policy, generated metadata, dependencies,
  licensing, documentation, and workflow shape.
- `PR / rust` formats, lints, builds, and runs the ordinary Rust test groups.

Focused public leaves remain available:

```bash
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
```

Tests are not retried automatically. Generated selectors and exact test
ownership live in `verification/registry.json` and test source; this document
does not duplicate their inventory.

Expensive exhaustive or fault-injection matrices belong in explicit
developer-local lanes. They are not hidden prerequisites for ordinary pull
requests.

When their owned project-store or imported-publication boundary changes, run:

```bash
cargo test -p mirante4d-project-store matrix -- --ignored
cargo test -p mirante4d-app imported_ -- --ignored
```

### Tier 3: Changed-Boundary Checks

Run these only when their boundary changes.

Renderer, GPU-resource, or dataset-runtime work can use the trusted local
Vulkan lane on the designated workstation:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu
```

That lane is a bounded wrapper around the focused ignored Vulkan tests. It
retains the clean-revision, hardware/adapter, timeout, independent-oracle, and
normal verification-report checks. Ordinary GPU test source is not registered
as a hash-bound fixture, and passing tests are not reinterpreted by a separate
qualification-receipt parser.

Focused release diagnostics are available for resident rendering and
rendering:

```bash
cargo test --release -p mirante4d-render-wgpu \
  resident_coordinated_volume_gpu_timing -- --ignored --nocapture
```

Package-format, storage-reader/writer, identity, or publication-boundary
changes use:

```bash
python3 tools/target-fixtures/t1/validate.py \
  --manifest fixtures/target/manifest.json --self-test
cargo xtask verify-local format-lifecycle
```

Project-store durability changes use:

```bash
python3 tools/project-fixtures/validate.py \
  --manifest fixtures/project/manifest.json --self-test
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local project-store-lifecycle
```

The project-store lifecycle lane is intentionally not a routine PR check.

Import and preprocessing work can use:

```bash
cargo xtask product-validate import_preprocessing
cargo run --release -p xtask -- import-performance-t2 --help
cargo run --release -p xtask -- import-performance-t5 --help
```

The bounded temporal-pipeline release comparison is a developer-local,
generated-data benchmark rather than a PR test:

```bash
cargo test --release -p mirante4d-import-pipeline \
  temporal_decode_ahead_release_benchmark -- --ignored --nocapture --test-threads=1
```

It runs three forced-serial and three shipped one-ahead imports of the same
twenty-timepoint multipage-TIFF source, requires identical package and
scientific identities, checks that source ingest and canonical production are
both material baseline phases, enforces the 15-percent median throughput gate,
and then runs the three-pair single-unit regression control. Its ignored case
is assigned to the developer-local verification lane; ordinary PR runs never
execute it.

The current large-dataset cutover clears the predecessor private T5
configuration pin because that configuration names the removed aggregate
profile/checkpoint contract. `import-performance-t5` therefore remains a
diagnostic/configuration tool until the owner reviews and pins a new
current-format configuration; it cannot publish a current qualification claim
while the pin is absent.

The explicit-manifest/application-shell cutover has a display-free focused
set suitable for iteration:

```bash
cargo test -p mirante4d-import-pipeline --lib --test import_pipeline \
  --test sentinel_restoration
cargo test -p mirante4d-dataset-runtime --lib
cargo test -p mirante4d-application
cargo test -p mirante4d-ui-egui
cargo test -p mirante4d-app --lib
cargo test -p mirante4d-storage
cargo test -p xtask
```

These checks cover explicit source ordering, metadata inspection, stale
completion suppression, decode-once counters, no-data reconstruction and run
spill, compositional storage boundaries, bounded temporal-unit scratch,
occupied-slot edge-object accounting, `T/C`-independent hard headroom,
final-layout journal recovery/corruption rejection, exact durable-range byte
accounting, capacity-pause versus invalid-checkpoint recovery UI,
progress-lane accounting/backpressure, labels through reopen, package
lifecycle, and automation schema shape. Import statistics intentionally report
`preflight_required_headroom_bytes`; `peak_temporary_bytes` is observed stage
growth and is not compared with that one-unit headroom as though it were a
whole-package reservation. These checks do not prove that the
welcome screen, file choosers, wizard, or a long local import behave correctly
on the owner's workstation.

Private datasets and configuration stay outside the repository. Source
nonmutation, bounded work, cancellation, and atomic create-only publication
remain mandatory. Full private qualification is run only when the affected
boundary or a new claim requires it.

### Tier 4: Product Validation

Rendering, viewport, GPU, loading, interaction, and large-dataset changes are
not complete until the actual viewer is exercised on a real display with the
relevant workload and hardware, unless the user explicitly waives that check.

Useful bounded scenarios include:

```bash
cargo xtask product-validate target_package_integrity_audit
cargo xtask product-validate target_fixture_render_modes
cargo xtask product-validate pre_alpha_reliability
cargo xtask product-validate \
  /absolute/path/to/dataset.m4d representative_native_navigation
```

`representative_native_navigation` requires an explicit package. It exercises
the native-navigation cut in the normal release application across four-panel
and standalone 3D, linked-only input during 3D refinement, both sampling
modes, MIP, DVR, ISO, nonblank GPU captures, and exact/current settlement.

`target_package_integrity_audit` uses the promoted bounded target fixture. It
first proves that ordinary open and idle leave the audit in `not run`, then
starts and cancels one explicit scan, starts a second scan to
`self-consistent`, and requires real progress plus cancellation/completion
counters. The scan is read-only and cannot be used as an ordinary-open,
project, analysis, or rendering prerequisite.

`pre_alpha_reliability` is the bounded package/reliability closeout. Against
one release executable it performs exactly three launches with zero automatic
retries: durable provisional autosave followed by external SIGKILL; proactive
startup exposure and explicit actor-validated dirty recovery; and a mapped
clean X11 window-manager close with a ten-second exit deadline. The scenario
retains exact external geometry, process status, checkpoint, bounded
panic-free stderr, canonical recovery-root count, nonblank recovered GPU
capture, project-store close/join, and source-byte nonmutation evidence.
Cleanup termination is always failure. It uses only the promoted small
fixture and is not a rendering stress or linked-S0 workflow.

The obsolete `representative_four_panel_three_sessions` and
`target_fixture_resident_navigation_no_readback` commands were deleted. Both
used application-paced command injection and internal publication facts that
cannot establish visible interaction continuity.

## GPU Placeability Correctness Gate

Payload-arena placeability is checked below the native viewer boundary. The
focused tests use synthetic exact-byte allocator state and a normal headless
application fixture; they do not open a window or generate desktop input:

```bash
cargo test -p mirante4d-render-wgpu \
  fragmented_payload_failure_reports_placeability_not_aggregate_exhaustion
cargo test -p mirante4d-render-wgpu \
  segment_compaction_stably_packs_payload_and_preserves_validity_delta
cargo test -p mirante4d-app \
  physical_placeability_limit_tightens_monotonically_and_reopens_for_new_input
cargo test -p mirante4d-app \
  residual_coordinated_placement_refusal_forces_adaptive_replan_without_red_error
cargo test -p mirante4d-app \
  successful_linked_only_install_clears_its_historical_capacity_warning
```

These tests establish typed fragmentation facts, stable packing and validity
offsets, monotone generic-selector fallback, non-latching recovery, and
scoped warning cleanup. They do not establish mapped-product responsiveness,
GPU copy correctness on a particular adapter, or monitor continuity.

The owner completed this product gate in the normal application on the
representative Cell dataset and confirmed that the inconsistent
selected-`none` and stale-warning behavior was corrected. If the incident
recurs, compare a fresh process with retained residency; Reset View changes
geometry but does not reset the allocator. Inspect aggregate free bytes,
largest contiguous bytes, per-segment ranges, placement refusals, and
compaction counters. This observation does not require the quarantined stress
workflow.

## Progressive Multiscale Presentation Gate

The progressive Plane and coherent-volume cut has focused, independent
automated checks:

```bash
cargo test -p mirante4d-render-api \
  multiscale_layer_coverage_distinguishes_fallback_mixed_and_target
cargo test -p mirante4d-render-wgpu \
  progressive_plane_falls_back_by_whole_footprint_and_invalid_never_falls_through \
  -- --ignored --test-threads=1
cargo test -p mirante4d-app \
  swept_capsule_proves_the_resident_slice_then_compound_rotation_sequence
cargo test -p mirante4d-app \
  linked_rotation_crosses_repeated_guard_windows_with_current_fallback_and_latest_only_settlement
cargo test -p mirante4d-app \
  complete_resident_3d_target_rebinds_directly_without_coarse_staging_or_new_io
cargo test -p mirante4d-render-wgpu \
  same_frame_body_replacement_retains_overlap_before_releasing_predecessor
cargo test -p mirante4d-render-wgpu \
  same_frame_prefetch_promotion_updates_the_owned_body_without_pin_churn
```

The render-API check proves scalar target, scalar single-fallback, conservative
multi-level fallback range, and partial target/fallback states are not
conflated. The trusted headless Vulkan fixture supplies independent expected
pixels for coarse fallback, a partially resident fine interpolation footprint,
complete fine replacement, and invalid-fine termination. The semantic and app
checks prove overlapping rolling-window initiation, current renderability
across repeated former guard boundaries, latest-only settlement, and the
ready-target 3D fast path. Ordinary renderer tests also enforce that MIP, DVR,
ISO, Mixed, and Pick compilation units cannot call Plane fallback.

The registered trusted-GPU test
`incremental_linked_zoom_pixels_match_direct_fine_cpu_oracle_after_one_settlement_plan`
also blocks a real linked planner result, renders the provisional predecessor,
installs a semantically different body under the unchanged intent revision,
and requires one real three-panel successor color cutoff. Its ordinary
settlement phase still compares GPU pixels with the independent reference
renderer. This catches both same-frame body rejection and false retained-frame
adoption; an internal currentness flag alone cannot satisfy it.

These checks establish implementation correctness, not mapped-display
continuity or representative target-refinement latency. The final product gate
is owner-controlled use of the normal four-panel viewer: exercise linked
rotation, translation, and zoom across repeated former guard boundaries;
observe current coarse geometry and bounded visible refinement without dark
holes; settle at the target; then confirm the GUI's fallback range, mixed
range, target completion, exactness, and currentness match the image. Also
confirm already-smooth S3/S2 3D motion did not gain a compulsory coarse flash.
Do not use the quarantined linked-S0 driver as a substitute for that
observation.

## GPU Memory, Growable Arena, And Hidden-Work Gate

Selected-adapter discovery, logical-versus-committed payload accounting, and
refresh-independent exact work have focused checks:

```bash
cargo test -p mirante4d-app gpu_memory::tests
cargo test -p mirante4d-app \
  selected_vulkan_adapter_reports_typed_device_local_memory \
  -- --ignored --nocapture --test-threads=1
cargo test -p mirante4d-render-wgpu \
  initial_payload_commitment_is_small_bounded_and_follows_logical_segments
cargo test -p mirante4d-render-wgpu \
  bounded_geometric_commitment_preserves_each_segment_without_large_overshoot
cargo test -p mirante4d-render-wgpu \
  hidden_batch_adaptation_grows_and_shrinks_bidirectionally
cargo test -p mirante4d-render-wgpu \
  segmented_payload_upload_and_sampling_cross_the_first_binding \
  -- --ignored --nocapture --test-threads=1
cargo test -p mirante4d-render-wgpu \
  coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame \
  -- --ignored --nocapture --test-threads=1
```

The native memory test must run on the designated Vulkan workstation and
prints facts for the selected test adapter. It requires a typed device-local
heap result and never substitutes a second product adapter. The payload tests
require a small initial physical commitment, exact per-segment high-watermarks
with no more than 128 MiB speculative growth headroom, preservation of already
rendered bytes across a real populated-buffer growth, and physical allocation
below the logical maximum. The asynchronous GPU test
starts one hidden job, lets it finish without repeated visible executions,
holds the complete candidate private until explicit publication
authorization, then compares the one atomic result with direct MIP, DVR, ISO,
and Mixed rendering. Cancellation/replacement and validation-capture
currentness are part of the same check.

The normal product gate is
`representative_native_navigation` on the Cell package. Its diagnostics must
name the exact adapter memory source, logical/committed/physical/resident
payload bytes, growth work, and hidden job/batch/row facts. A multi-gigabyte
logical allowance must not imply a multi-gigabyte startup allocation. Smooth
exact settlement must advance while the UI is otherwise idle; visible
presentation remains vsynced.

## Native-Resolution Navigation Gate

The native 3D/pyramid cut uses focused geometry, ownership, presentation, and
headless Vulkan checks:

```bash
cargo test -p mirante4d-import-pipeline pyramid::tests
cargo test -p mirante4d-storage every_representable_geometry
cargo test -p mirante4d-application \
  target_families_advance_independently_on_one_unique_sequence
cargo test -p mirante4d-app volume_presentation::tests
cargo test -p mirante4d-app \
  finishing_camera_gesture_commits_without_allocating_a_second_3d_frame
cargo test -p mirante4d-render-wgpu \
  coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame \
  -- --ignored --test-threads=1
cargo test -p mirante4d-ui-egui \
  volume_presentation_label_retains_native_preview_during_hidden_rows
cargo test -p mirante4d-render-wgpu \
  terminal_full_volume_fast_path_matches_reference_for_all_volume_modes_and_sampling \
  -- --ignored --test-threads=1
cargo test --release -p mirante4d-render-wgpu \
  native_1080p_terminal_navigation_gpu_timing \
  -- --ignored --nocapture --test-threads=1
cargo test -p mirante4d-app \
  unsafe_3d_profile_keeps_native_preview_visible_until_atomic_exact_strips_finish \
  -- --ignored --test-threads=1
```

The geometry checks cover single-scale small data, odd dimensions, long-thin
data, more than seven levels, and the maximum representable dimension. Storage
round-trips the full 64-level profile boundary. Mailbox checks prove linked 2D
and 3D advance independently and that gesture release does not manufacture a
second frame.

Presentation checks require preview extent to equal physical output extent,
freeze one preview body per gesture, retain one preview per frame, preserve
that preview through hidden exact row batches, report screen rows rather than
data tiles, and perform one authorized exact atomic swap. The application GPU
test drives the real viewport-observation path, injects linked input during
refinement, and requires unchanged 3D revision, progress, texture, and visible
front.

The terminal fast-path fixture compares one complete 64³ resource with the
ordinary eight-page sparse path for voxel-exact and smooth-linear MIP, DVR,
and ISO. Dedicated MIP/DVR/ISO tests retain independent numerical-oracle
coverage. The timing fixture names adapter, shape, physical extent, mode,
sampling, trials, median, and p95; each 1920×1080 terminal case must remain
below the 16.667 ms product guideline. This is a practical workstation target,
not a universal adapter contract.

The product gate is the normal mapped application in four-panel and standalone
3D. Exercise rotation, translation, and zoom at an already-smooth
S3/S2 profile and at a deliberately expensive finer target; interrupt hidden
refinement; change linked 2D while 3D refines; and check voxel-exact,
smooth-linear, MIP, DVR, ISO, and Mixed. Every visible preview must retain the
physical panel resolution, one gesture must not flicker between LOD policies,
and settlement must make one preview-to-exact transition. The inspector's
shown/selected/ideal LOD, native preview/direct state, exact row progress, and
currentness must agree with the image.

The owner completed this product observation after the final navigation-ladder
cut and reported the four-panel and 3D result working as expected.
Smooth-linear remains functionally covered by these checks, but its
fine-scale exact refinement is a known non-blocking performance limitation
relative to voxel-exact. No smooth-linear performance claim is part of the
closed refactor; optimization or removal is a separate product change.

## Resident 3D Navigation Ladder Gate

The terminal-to-fine ladder adds focused planner, render-contract, selection,
and integration checks:

```bash
cargo test -p mirante4d-render-api \
  dormant_multiscale_residency_suffix_never_changes_uniform_volume_coverage
cargo test -p mirante4d-app \
  progressive_plan_keeps_terminal_floor_when_finer_full_volume_fits
cargo test -p mirante4d-app \
  installed_ladder_requires_terminal_first_and_reuses_adjacent_rungs
cargo test -p mirante4d-app volume_presentation::tests
cargo test -p mirante4d-app \
  complete_navigation_floor_makes_latest_camera_immediately_renderable
cargo test -p mirante4d-render-wgpu \
  coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame \
  -- --ignored --nocapture --test-threads=1
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
  /absolute/path/to/cell.m4d representative_native_navigation
```

The API check proves that a multiscale residency suffix is permanently
dormant for a uniform volume wrapper, cannot be promoted, and cannot change
coverage or the target scale. Planner checks prove mandatory terminal-first
ordering, bounded coherent finer admission, and installed-ladder reuse without
another semantic traversal. Selection checks prove finest-safe choice,
target-quality rejection, interaction-unsafe rejection, cold-terminal
bootstrap without a false residency claim, one-way loss of a camera-local
target, and no same-gesture upgrade.

The mapped report must serialize every candidate's active scale, exact
resource/payload cost, native work, residency, target eligibility, safety, and
disposition. The 2026-07-31 Cell closeout retained S6/S5/S4/S3 plus exact S2,
selected S4, rejected resident S3/S2 as unsafe, completed all 60 commands, and
reported zero renderer validation errors. This is evidence for the selected
workstation and scenario, not a universal frame-rate guarantee.

## Composed Temporal Presentation Gate

The playback scenario accepts an explicit representative multi-timepoint
package and runs the normal release application:

```bash
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
  /absolute/path/to/time-series.m4d representative_temporal_playback
```

The current script has 70 commands and uses the same playback clock,
readiness, mailbox, demand, renderer-completion, and presentation boundaries as
the viewer. It independently checks distinct timepoint-bound GPU captures,
fixed session scales, ordered successors, and coherent active-layout target
groups. Seven paired phases compare a same-duration stationary baseline with
two seconds of held mapped input: standalone 3D orbit and zoom, four-panel 3D
orbit, linked pan/zoom/oblique rotation, and lower-FPS four-panel zoom.

Every held-input phase must receive its generated input, materially change the
requested geometry, preserve the gesture through the interval, and present
temporal successors in both halves. Its transition count must reach 80% of the
immediately preceding stationary baseline, rounded up. Its maximum temporal
gap must not exceed the greater of three requested frame periods or the
baseline maximum plus one requested period. This is a calibrated pause and
starvation check for the designated scenario, not a universal frame-rate
promise.

The scenario also records Stop handoff states from the actual presented
timepoint, target group, frame identity, texture binding, completeness, and
displayed scale. Only the playback scale followed directly by the stationary
scale is accepted; blank, partial, stale, mixed, or intermediate coarse output
fails. The launcher isolates the runtime log and rejects requirement-set
changes, missing prepared plans, contract loss, capacity regressions, scale
escape, timepoint skips, incoherent panel publication, and renderer errors.

The 2026-08-01 mapped run used a 960x640 native client viewport on the NVIDIA
GeForce RTX 3070 Ti Laptop GPU/Vulkan adapter. All seven cadence comparisons
passed; input transition counts ranged from 21 to 36 and all exceeded their
paired 80% floors, while the worst input maximum gap was 119.342 ms. All three
Stop traces had no violation and contained only exact S1 plus exact S0 where a
finer stationary replacement was available. Focused app, xtask, and renderer
suites passed 305, 169, and 71 tests respectively; expected ignored hardware
tests remained ignored in those ordinary package runs. The owner then
exercised the normal app and accepted its visible behavior. Internal
presentation records and GPU readbacks remain supporting evidence; owner
observation on the mapped display supplies product validation.

The final public PR gate passed every policy phase, zero-warning workspace
Clippy, exact lane discovery, and all 1,316 applicable unit, contract, and UI
tests with retries disabled.

The
[composed presentation scheduler cutover](plans/active/VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md)
owns the architecture, threshold rationale, and completion boundary.

The mapped four-panel linked diagnostic is currently quarantined after its
first S0 attempt froze the desktop. Do not use it as an unattended check. A
later owner-approved controlled invocation must acknowledge that host risk
explicitly:

```bash
cargo xtask viewer-oblique-continuity \
  --workflow linked-lod-diagnostic \
  --dataset /absolute/path/to/dataset.m4d \
  --duration-secs 30 --runs 1 \
  --allow-host-stress
```

The linked diagnostic selects four-panel layout in the normal release
application, requires exact warm S3, S1, and S0, and generates bounded,
nonstationary Shift+primary XY motion at an independent 60 Hz for 30 seconds
per phase. It returns toward S3 and records generated/received input, UI,
planning/data/upload, renderer CPU, linked GPU, and egui texture-paint queuing.
It does not observe surface present or compositor/monitor visibility and has
no performance acceptance threshold. The owner is the visual authority.

The narrower 3D workflows remain:

```bash
cargo xtask viewer-oblique-continuity \
  --workflow zoom \
  --dataset /absolute/path/to/dataset.m4d \
  --duration-secs 30 --runs 3 --skip-build
cargo xtask viewer-oblique-continuity \
  --workflow combined \
  --dataset /absolute/path/to/dataset.m4d \
  --duration-secs 300 --runs 1 --skip-build
```

The `zoom` workflow drives independently clocked real wheel input over the 3D
panel, deliberately observes both a feasible finer display and an ideal
boundary constrained to a coarser adaptive display,
balances back to the original zoom, and checks raw wheel receipt,
authoritative camera applications, main-loop progress, external 3D pixels,
truthful LOD status, and complete S3 recovery. `combined` first performs a
verified real-window resize round trip and real 3D orbit round trip, then runs
the same zoom oracle for the sustained duration.

The workflows attempt finite closure and write only ignored local CSV/text
under `target/mirante4d/viewer-oblique-continuity/`. The rendering handoff
plans own any applicable acceptance thresholds. The app-side trace is bounded
and any dropped event invalidates the run; it cannot silently turn missing
observations into a pass. Linked internal publication and X11 client-surface
reads are not monitor-visible evidence. `--video` optionally retains ignored
lossless video for a focused visual diagnosis. Omit `--skip-build` for the
first workflow after changing product source.

For the real-display exercise:

```bash
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
```

Inspect the native mapped client, the changed workflow, visible output, and
logs. Confirm that the application remains alive without a hidden fallback or
repeating GPU error. Use the packaged application when packaging or release
behavior changed.

## Linked-2D Evidence Boundary

The owner has confirmed that ordinary incremental XY/XZ/YZ zoom can now reach
visibly fine S0 output, and a deterministic fixture check compares linked GPU
pixels to the independent reference renderer. Neither fact establishes
monitor-continuity performance for the representative mapped workload:

- the real-window `zoom` and `combined` workflows drive the independent 3D
  panel;
- the generic product screenshot and image-stat helpers implicitly read the 3D
  target;
- planner/scope scale fields, `Current`, `Exact`, coordinated settlement, and
  runtime idle share the product authority and cannot validate themselves; and
- X11 client-surface artifacts can change while the mapped monitor image
  remains static on the current host.

The old linked-wheel visible-change samples were synthesized from internal
publication and are deleted. That workflow is endpoint correctness only. The
new diagnostic records real input and causal work boundaries but explicitly
leaves surface present and monitor continuity unobserved.

The
[closed linked-2D handoff](plans/active/VIEWER_LINKED_2D_LOD_TRUTH_AND_SETTLEMENT.md)
records the hard cut and the first live diagnostic incident. That run
completed generated S3 and S1 phases, froze the desktop while approaching S0,
and lost its app timeline at reboot. It produced no valid performance result
or freeze diagnosis. The linked S0 workflows are quarantined by default;
their receipt, heartbeat, bounded-input, and input-release safeguards are
statically tested, and the bounded trace now checkpoints during live input/UI
heartbeats instead of waiting for graceful exit. Those diagnostic safeguards
are not product-validated, and rerunning them is not required for the closed
product correction.

## What Must Remain Strong

The simplification of development process does not weaken these product
boundaries:

- scientific values, validity, axes, calibration, and independent numerical
  expectations;
- source nonmutation and content addressing where a source is imported;
- bounded memory, descriptors, queues, and cancellation;
- atomic create-only publication and project/package durability;
- per-object integrity at storage and transport boundaries; and
- real viewer behavior on relevant data and hardware.

Exact hashing is appropriate for durable object identity, import-source
currentness, scientific content addressing, publication self-consistency, and
external downloads. A digest authenticates nothing without an independent
expected value or trust source. Rehashing the same immutable inputs at every
internal step, hash-chaining development checkpoints, or signing ordinary test
output is not a default requirement.

## Retired Viewer Qualification Protocols

The EP-00 and EP-01 viewer-performance protocols are frozen historical
development artifacts. Their raw-report, receipt, replay, population,
selection, and per-role provenance machinery is not a current development
prerequisite and must not block product work. Their command, schema, selection,
receipt, replay, harness, shared gate/counter, and repeated resource-union
hashing roots have been removed from the live repository.

The corrective viewer program completed three small product slices:

1. resident interaction and per-view LOD;
2. cold complete refinement, reuse, and presentation correctness; and
3. MIP, DVR, and ISO kernels.

Each slice closed with focused automated checks, a small independent
correctness oracle where needed, useful GPU timings, and real-product
validation. One-off cold-refinement scripts, signal probes, and terminal-latch
tests remain scoped development evidence rather than permanent qualification
protocols. [Current state](CURRENT_STATE.md) owns the implemented result.

## Reporting

Report:

- the meaningful commands and results;
- the real dataset, display, and hardware when relevant;
- important skips or explicit waivers;
- the visible product result; and
- remaining risk.

A performance claim must also name its workload, metric, sampling method,
cache condition, and threshold. Avoid publishing private paths, labels,
geometry, or scientific identities.
