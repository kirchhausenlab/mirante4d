# Viewer GPU Testing Refactor

- Status: IMPLEMENTED — TRUSTED CORRECTNESS, PRESENTATION BOUNDARY, AND EXACT SCANOUT SMOKE PASS; CALIBRATION, BASELINE ACCEPTANCE, AND OWNER PRODUCT EVIDENCE PENDING
- Planning requested by owner: 2026-08-02
- Target approved by owner: 2026-08-02
- Planning baseline: `482b97bd4263dfc90e114ec88c8afad0670624dd`
- Last reviewed: 2026-08-03
- Scope: trusted Vulkan correctness, GPU component performance, mapped-product
  interaction performance, baseline lifecycle, reporting, and the hard
  deletion of the former mixed trusted-GPU lane.

This document is the sole design and implementation authority for the GPU
testing refactor described below. The repository cutover is implemented: the
mixed command is deleted, correctness and performance have exact separate
owners, the mapped presentation observer and campaign exist, and the baseline
manifest is fail-closed. The trusted hardware cases, ten-run calibration,
comparison pairs, and owner mapped-product observation were deliberately not
run during the initial repository cutover. Later executions are recorded
below: the repaired correctness lane passes, while the first performance
activation stopped in its controlled presentation preflight and accepted no
runs. The product-probe liveness and evidence-construction defects are
repaired. The owner-approved Vulkan observer now combines present-wait,
marker identity, and first-pixel-out timing to establish correctly correlated
product visibility, exact scanout cadence, maximum active visible gap,
conservative resident input response, and coarse settlement. Comparison pairs
and owner product observation remain unrun, the baseline remains
`pending_initial_calibration`, and no accepted calibration, comparison,
baseline, photon, or owner product-validation claim is active.

The owner approved the complete target after a read-only audit and subsequent
decisions about local execution, performance triggers, baseline construction,
30 Hz versus 60 Hz, visible stalls, and exact refinement. Approval of this
document does not itself authorize a test run, a performance claim, a
repository workflow change, or product implementation outside the testing
boundary.

The owner later authorized the first complete trusted correctness execution.
The clean run at commit `a6ef07aea1ea04bddeb77efd7ad9cbd0f97a236d`
selected all 25 registered cases; the native Nextest summary reported 19
passes and six failures. The implemented
[GPU correctness campaign follow-up](VIEWER_GPU_CORRECTNESS_CAMPAIGN_FOLLOW_UP.md)
owns their reviewed correction. Its subsequent clean execution passed all 25
cases with exact structured attribution. Automated normal-product scenarios
also pass; owner observation remains separate. The first performance
calibration activation was unevaluated before measurement.

## Owner-Approved Vulkan Present-Wait Amendment

On 2026-08-03 the owner approved replacing the unavailable X11 Present
completion authority with a narrowly scoped Vulkan presentation authority.
The replacement uses `VK_KHR_present_id` and `VK_KHR_present_wait` only to
prove that a correctly correlated final swapchain image reached presentation,
to close exact-settlement visibility, and to detect the maximum interval with
no correct presented change while eligible resident interaction is active.

This amendment deliberately does **not** authorize exact displayed cadence,
FPS, presentation-interval percentiles, or input-to-photon measurement from
the wait-return timestamps. The specification does not give the waiter's host
wake-up a precise relationship to scanout. The 30 Hz target and its 33.3 ms
cadence gate remain unchanged but unevaluated until a separately approved
precise timing authority exists.

The implemented amendment is:

1. Prove the two extensions on the qualified adapter with one isolated FIFO
   Vulkan window before changing the product integration. The probe must cover
   multiple labeled presentations, a controlled no-present interval, an
   unchanged repeat, unmap/remap, resize, swapchain recreation, finite worker
   shutdown, and an explicit `exact_cadence=unevaluated` result.
2. Add one opt-in hook at the existing final WGPU Vulkan swapchain-present
   boundary. It assigns strictly increasing present IDs and never waits on the
   UI, renderer, WGPU submission, or presentation thread. A bounded worker is
   the sole `vkWaitForPresentKHR` caller.
3. Retain the independent marker and X11 window-lifecycle observer. Marker
   readback after the Vulkan completion correlates the final surface image to
   the eligible product identity; X11 continues to prove map, focus,
   occlusion, resize, and extent facts but no longer claims presentation
   completion.
4. Hard-cut the old `x11_present_complete_notify_marker_v1` measurement
   authority and version the performance schema. There is no fallback to X11
   completion, GPU completion, client-surface change, application publication,
   or egui paint queuing.
5. Report visibility, maximum visible stall, and coarse exact-settlement
   latency separately from exact cadence. A missing extension, failed wait,
   timeout, dropped correlation, surface mismatch, ambiguous marker, or
   lifecycle invalidation is failed, invalid, or unevaluated; it is never
   green.

The product renderer, scientific output, LOD, viewport resolution, queues,
and ordinary WGPU device configuration do not change. Extension enablement
and final-present instrumentation exist only when the private presentation
report environment activates this local campaign. The exact upstream
`wgpu-hal` version is patched locally only at device-extension enablement and
native swapchain presentation; no second renderer, alternate presentation
path, timing block on the UI thread, Vulkan layer, or hosted workflow is
introduced.

The isolated feasibility probe passed on the NVIDIA GeForce RTX 3070 Ti
Laptop GPU with FIFO presentation. Four labeled IDs completed, a deliberate
160 ms no-present interval remained observable, and presentation completed
again after unmap/remap, an exact 640x480 to 800x600 resize, and swapchain
recreation. The probe made no cadence claim.

The subsequent normal-product boundary run passed with 230 submitted and
completed presents, four swapchain changes, five correctly changed product
frames, 191 unchanged repeats excluded, 34 completions during the controlled
unmap excluded, and zero wait failures, timeouts, or ambiguous marker
bindings. It proved unchanged-repeat, distinct-queued-image, lifecycle, marker
ordering, and surface-recreation controls. The representative interaction
also produced scoped visibility, stall, and coarse-settlement measurements.
Its report explicitly records `exact_cadence.status = unevaluated`.

## Owner-Approved First-Pixel-Out Scanout-Timing Amendment

On 2026-08-03, after the qualified workstation was upgraded and rebooted onto
the NVIDIA 595.84 open kernel driver, the owner approved completing the exact-
cadence boundary with `VK_EXT_present_timing`. A real X11 Vulkan-surface probe
on that workstation established extension revision 3, present-timing support,
and `VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT`. The same probe found no
first-pixel-visible support. This amendment therefore uses first-pixel-out and
does not claim physical photon visibility.

This amendment supersedes only the present-wait amendment's explicit exact-
cadence deferral. The present-wait and X11-marker evidence remains required;
neither is deleted or silently relabelled.

### Measurement Meaning

The automated authorities are deliberately separate:

1. the normal product supplies the eligible frame identity and paints the
   checksummed foreground marker;
2. `VK_KHR_present_id` plus `VK_KHR_present_wait` proves completion of the
   numbered final-swapchain present and supplies the existing conservative
   host-time visibility, stall, and settlement upper bounds;
3. X11 reads the marker only after that completion and proves which complete
   product image reached the mapped client surface;
4. `VK_EXT_present_timing` supplies the hardware presentation-engine time at
   which the first pixel left that engine for display hardware; and
5. exact interaction intervals are calculated only after the marker-qualified
   product record and first-pixel-out result agree on present ID, swapchain
   generation, phase, surface, extent, and work identity.

The resulting metric is named **first-pixel-out scanout cadence**. It is the
precise automated authority for the representative product's 30 Hz cadence
gate. It is not a measurement of photons leaving the panel, a photodiode
result, end-to-end input-to-photon latency, or proof that every monitor and
compositor behaves like this qualified X11 workstation.

### Required Vulkan Contract

The observer is opt-in and inactive unless the private presentation-report
environment is present before WGPU instance creation. In that mode only, the
Vulkan path must require all of:

- `VK_KHR_get_surface_capabilities2` at instance creation;
- `VK_KHR_present_id` and `VK_KHR_present_wait` for the retained completion
  authority;
- `VK_KHR_present_id2`, including its device feature and the surface's
  `presentId2Supported` capability;
- `VK_KHR_calibrated_timestamps`, as required by the timing extension; and
- `VK_EXT_present_timing` revision 3, its `presentTiming` device feature, the
  surface's `presentTimingSupported` capability, and the
  `IMAGE_FIRST_PIXEL_OUT` query bit.

The current Ash release predates these final extension bindings. The smallest
accepted implementation is a local, `repr(C)` binding of only the ratified
revision-3 structures, constants, and four device entry points needed here,
kept beside the existing narrowly patched WGPU Vulkan hook. This is not a
second renderer, loader, presentation path, or general Vulkan-binding fork.
The ABI constants and layouts must match the Khronos registry, and the real
driver activation probe remains the final ABI/capability check.

Every observed native swapchain must:

- be created with `VK_SWAPCHAIN_CREATE_PRESENT_ID_2_BIT_KHR` and
  `VK_SWAPCHAIN_CREATE_PRESENT_TIMING_BIT_EXT`;
- allocate exactly 256 timing-result slots with
  `vkSetSwapchainPresentTimingQueueSizeEXT` before its first timed present;
- select one time-domain ID actually returned for that swapchain;
- attach both the retained present ID and the ID2 value, with the same strict
  nonzero ID, to every observed `vkQueuePresentKHR` call;
- request only `VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT`, with
  `targetTime = 0`; and
- record a new monotonically increasing swapchain generation even if the
  driver later reuses a raw handle value.

The 256-slot driver queue and the existing 256-item worker queue are one
bounded admission contract. Timing results are drained asynchronously after
present completion and during idle worker polls. Swapchain destruction waits
only for that swapchain's already-admitted bounded completion and timing
records; no cadence wait is added to the ordinary UI, renderer, queue-submit,
or present path.

### Correlation And Clock Rules

One timing result qualifies only when all of these are true:

- its present ID was admitted exactly once for the same live swapchain
  generation;
- the report is complete and contains exactly one requested first-pixel-out
  stage;
- the first-pixel-out time is nonzero;
- the corresponding present completed without timeout or Vulkan failure;
- the mapped-window marker decodes to the eligible product identity for that
  present; and
- the record is neither unchanged, superseded, window-unavailable, ambiguous,
  nor outside the declared measurement phase.

Driver feedback may arrive later than the present-wait completion. The worker
therefore retains either half of the correlation until the other half arrives;
arrival order does not create a sample. Driver results returned out of present
order, duplicate or unknown IDs, zero/missing stages, incomplete results,
result-queue exhaustion, unconsumed records at swapchain destruction or
process close, and finite-drain timeout are explicit failures or unevaluated
capability results. They are never replaced by CPU timestamps.

First-pixel-out deltas are compared only inside one time-domain kind and ID.
A swapchain recreation begins a new segment. A time-domain or timing-
properties counter change inside one live swapchain is recorded and
invalidates the affected measurement rather than joining incomparable clocks.
No interval crosses a surface generation, phase boundary, mapped-window
unavailability, or clock-domain boundary.

### Product Metrics And Gates

Each fresh representative product session adds these raw vectors and blocking
summaries:

| Metric | Source | Summary | Hard limit |
| --- | --- | --- | ---: |
| Standalone interaction scanout interval | Consecutive qualified first-pixel-out records during active standalone 3D input | nearest-rank p95 | 33.3 ms |
| Four-panel interaction scanout interval | Consecutive qualified first-pixel-out records during active linked input | nearest-rank p95 | 33.3 ms |
| Resident input-response upper bound | First marker-qualified present-wait completion for each represented active input generation | nearest-rank p99 | 50 ms |

The existing 100 ms maximum active visible-gap gate remains separate and uses
the conservative post-present host observation so it retains start/end-of-
input coverage without pretending that two different clock domains can be
subtracted. Existing 250 ms, 1 second, 250 ms startup-coarse, and 2 second
startup-exact settlement gates also retain their conservative post-present
upper-bound method.

The two first-pixel-out vectors must be nonempty and reproducible from the raw
records. Their p95 values decide the minimum 30 Hz product contract; 16.667 ms
remains preferred. A synthetic two-refresh interval around 33.3 ms must be
distinguishable from the normal one-refresh interval around 16.7 ms. p95 does
not replace the independent 100 ms maximum-stall gate.

The measurement version, observer schema, private configuration authority,
accepted-baseline schema, and pending baseline placeholder hard-cut together.
An old present-wait-only probe receipt or baseline is incomparable, not a
fallback. Calibration still requires ten accepted fresh sessions and explicit
owner acceptance before a repository baseline becomes active.

### Validation And Closeout

Portable tests must independently cover:

- exact extension constants and bounded queue policy;
- complete, delayed, duplicate, unknown, zero-stage, zero-time, missing,
  out-of-order, and cross-generation timing records;
- swapchain and time-domain segmentation;
- nearest-rank p95/p99 recomputation and the 16.7/33.3 ms distinction;
- report rejection when either cadence vector, raw record, capability,
  correlation, or clock fact is missing; and
- hard rejection of the former present-wait-only authority.

The real-display activation sequence is: one short controlled boundary probe,
one representative-product smoke run, then the complete performance campaign
when calibration or comparison evidence is requested. The live proof must
show strict IDs, nonzero first-pixel-out times, stable time-domain identity,
finite timing drain, unchanged-repeat exclusion, resize and swapchain
recreation, unmap/remap exclusion, and zero queue, timeout, duplicate,
correlation, or validation failures.

External photodiode or high-speed-camera validation and a Wayland timing
profile may later cross-check this authority. Neither is required for the
approved X11 first-pixel-out gate, and neither may be inferred from it.

## Decision

Mirante4D will replace the single mixed trusted-GPU lane with three distinct
evidence classes:

1. a trusted-local Vulkan correctness lane;
2. a trusted-local release performance campaign; and
3. a normal mapped-product interaction and refinement gate.

The separation is semantic, not cosmetic. Correctness may never be skipped
because performance measurement is expensive. Component timing may never be
presented as proof of visible product cadence. Product observation may never
turn a wrong or stale image into a performance pass.

The following decisions are fixed:

- Hardware-dependent tests remain excluded from ordinary `cargo test`
  through `#[ignore]` or an equally explicit local-only mechanism.
- Ignored means opt-in execution, not waived or permanently disabled.
- Every hardware case has one exact lane owner and one exact expected
  discovery count.
- The trusted GPU runs locally on the designated workstation. They do not run
  in GitHub Actions and do not require a public or private self-hosted runner.
- Public hosted verification remains limited to the existing zero-cost
  policy and Rust checks.
- Correctness runs whenever a boundary capable of changing GPU meaning,
  resources, synchronization, or presentation changes.
- Performance runs whenever a boundary capable of changing work, latency,
  cadence, loading, or refinement cost changes.
- The reference performance platform is the qualified Linux x86_64/Vulkan
  workstation with the NVIDIA GeForce RTX 3070 Ti Laptop GPU.
- The qualified product resolution is 1920×1080. This plan creates no 4K,
  Windows, macOS, Metal, DX12, AMD, Intel, or universal hardware claim.
- The representative product workload is the owner-provided Cell package in
  the normal application, in four-panel and standalone 3D layouts.
- MIP, DVR, ISO, Mixed, Pick, voxel-exact sampling, and smooth-linear sampling
  remain scientifically unchanged and must be represented at their applicable
  evidence boundaries.
- Thirty displayed updates per second is the minimum interactive product
  target. Sixty displayed updates per second is preferred, not mandatory.
- Exact refinement is measured separately from interactive preview cadence.
- Absolute product limits and relative baseline-regression limits both apply.
- A benchmark may not pass by selecting a coarser LOD, reducing output
  resolution, dropping a visible layer, changing sampling, skipping work, or
  weakening scientific output.
- Baselines are intentionally accepted and version-bound. They are never
  learned from or silently replaced by the candidate under test.
- A missing adapter, unsupported required capability, skipped case,
  incomplete report, invalid environment, or unavailable presentation
  measurement is unevaluated or failed as specified below; it is never green.
- Tests and campaigns have zero automatic retries.

## Authority And Relationship To Existing Work

This plan owns the target verification topology and performance contract for
the GPU renderer and its application presentation boundary. It does not
replace the renderer, application, LOD, residency, presentation, or
scientific authorities described by the existing viewer plans.

In particular:

- the rendering correctness, recovery, and numerical cutover retains
  ownership of its open R11/P7 one-time static recovery claim;
- the existing private A/B/C static-recovery campaign remains a separate
  claim-specific protocol until that plan is closed or explicitly amended;
- historical GPU and mapped-product results remain attached to their named
  revisions, hardware, workloads, metrics, and thresholds;
- the composed playback product gate retains its fixed-session and temporal
  semantics;
- the linked-S0 host-stress workflows remain quarantined and are not made a
  prerequisite of this refactor; and
- no result from this plan can retroactively repair, invalidate, or broaden a
  historical claim.

The refactor may reuse small, proportional runner and reporting code from the
current verification system. It must not recreate the retired EP-00/EP-01
receipt, replay, role-provenance, population, or persistent qualification
machinery.

## Current Inventory And Required Disposition

The implemented trusted-GPU selector currently expects 27 ignored test
functions: five application cases and 22 renderer cases. Three renderer
functions are explicitly timing-oriented. One of those also carries important
structural correctness assertions.

The cutover must inventory exact discovered names before moving any selector.
The following table is the approved disposition. A rename may improve clarity,
but it may not change ownership or silently remove an assertion.

| Current case or exact family | Current count | Target disposition |
| --- | ---: | --- |
| `selected_vulkan_adapter_reports_typed_device_local_memory` | 1 | Correctness |
| `unsafe_3d_profile_keeps_native_preview_visible_until_atomic_exact_strips_finish` | 1 | Correctness |
| `exact_transient_cross_section_updates_all_linked_panels_before_finish_and_empty_clears_them` | 1 | Correctness |
| `incremental_linked_zoom_pixels_match_direct_fine_cpu_oracle_after_one_settlement_plan` | 1 | Correctness |
| `direct_timepoint_selection_replaces_retained_gpu_pixels_with_the_successor_body` | 1 | Correctness |
| `volume_page_traversal_crosses_sub_epsilon_direction_for_all_kernels_and_pick` | 1 | Correctness |
| `volume_page_traversal_zero_direction_stays_in_one_page` | 1 | Correctness |
| `volume_page_traversal_subnormal_direction_is_portably_canonicalized_or_rejected` | 1 | Correctness |
| `volume_page_traversal_far_boundary_overflow_clamps_only_to_ray_exit` | 1 | Correctness |
| `existing_device_constructor_publishes_color_before_pick_readiness` | 1 | Correctness |
| `progressive_plane_falls_back_by_whole_footprint_and_invalid_never_falls_through` | 1 | Correctness |
| `dedicated_mip_off_axis_color_matches_independent_numerical_oracle` | 1 | Correctness |
| `dedicated_iso_affine_lighting_and_six_tap_halo_match_independent_reference` | 1 | Correctness |
| `dedicated_mixed_mip_iso_authored_over_matches_independent_hand_facts` | 1 | Correctness |
| `dedicated_dvr_off_axis_perspective_uses_physical_world_distance` | 1 | Correctness |
| `resident_body_changes_reuse_global_gpu_residency_across_targets` | 1 | Correctness |
| `coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame` | 1 | Correctness |
| `coordinated_four_target_resident_cutoff_has_real_pixels_one_submit_and_idle_zero` | 1 | Correctness |
| `cross_target_upload_after_progress_snapshot_remains_dirty_until_consumed` | 1 | Correctness |
| `coordinated_target_pins_are_union_scoped_and_layout_retirement_releases_them` | 1 | Correctness |
| `segmented_payload_upload_and_sampling_cross_the_first_binding` | 1 | Correctness |
| `rays_beyond_the_removed_16384_sample_cap_reach_the_far_voxel` | 1 | Correctness |
| `multichannel_semantics_are_order_independent` | 1 | Correctness |
| `terminal_full_volume_fast_path_matches_reference_for_all_volume_modes_and_sampling` | 1 | Correctness |
| `resident_coordinated_volume_gpu_timing` | 1 | Split: one correctness case plus one performance benchmark |
| `native_1080p_terminal_navigation_gpu_timing` | 1 | Performance; duplicate setup assertions move to correctness owners |
| `fixed_lod_multichannel_gpu_timing_matrix` | 1 | Performance; scientific and zero-work prerequisites remain explicit |

After the split, the expected minimum inventory is:

- 25 trusted-GPU correctness functions: the existing 24 predominantly
  correctness functions plus the structural resident-coordinator case
  extracted from `resident_coordinated_volume_gpu_timing`;
- three performance benchmark functions or equivalently explicit benchmark
  families; and
- 39 performance measurements: six native terminal mode/sampling cases, the
  existing 32-case fixed-LOD multichannel topology, and one resident
  coordinated-volume case.

The exact correctness count may increase only when a genuinely distinct
failure is split for clarity. It may not decrease below 25 without an
owner-approved amendment that identifies the removed case and its replacement
evidence. The 39-measurement performance topology may change only when the
supported product modes, sampling policies, or named workload changes.

Broad prefixes such as `dedicated_` or `volume_page_traversal_` may remain
convenient source organization, but they may not be the sole registration
authority. The generated selector must expand to the exact discovered test
names and compare them with the exact registered inventory. An added,
removed, renamed, duplicated, unexpectedly unignored, or unexpectedly ignored
case fails registry synchronization.

## Problems This Refactor Must Close

The refactor is incomplete while any of these problems remains:

1. correctness and performance share one command, timeout, report, and pass
   meaning;
2. ordinary test output says the hardware cases are ignored without exposing
   whether the trusted lane later ran them;
3. important structural assertions live only inside timing functions;
4. the 16.667 ms terminal component threshold is described as a product
   requirement even though the repository establishes only a practical
   workstation guideline;
5. a five-sample nearest-rank p95 is effectively the slowest of five samples
   and is too weak for a stable performance gate;
6. a small 64³ synthetic fixture can be confused with representative
   application performance;
7. internal application publication can be confused with surface present,
   compositor output, click-to-photon latency, or monitor-visible cadence;
8. average or p95 cadence can hide isolated, obvious freezes;
9. exact completion can be inferred from worker, upload, submission, or
   metadata state before the selected image is actually current and visible;
10. an absolute threshold can hide a large relative regression;
11. a relative threshold can qualify a product that still misses its absolute
    usability limit;
12. changed scientific work, LOD, resolution, cache state, residency, layer
    set, or mode can be compared as though it were the same workload; or
13. a missing capability or incomplete run can be mistaken for a passing
    test.

## Existing Threshold Reconciliation

The repository currently contains several numbers that describe different
boundaries. The refactor must not merge them or leave their authority
implicit:

- the 12 ms interaction-work guideline belongs to the renderer's conservative
  navigation-body selection model. It is an internal work-policy input, not a
  displayed frame-rate requirement, and this testing refactor does not change
  it;
- the 16.667 ms terminal GPU-pass limit becomes a preferred component and
  displayed-cadence target. It is no longer a mandatory universal product
  threshold;
- the historical 20 ms p95 and 50 ms maximum app-admission-to-publication
  limits measure a pre-surface internal boundary. They remain useful
  attribution/headroom diagnostics and historical evidence, but they do not
  define actual presented cadence;
- the 33.3 ms displayed-frame p95, 50 ms p99 input response, and 100 ms maximum
  visible-gap limits in this plan are the blocking representative-product
  interaction contract;
- the 250 ms resident settlement, 1 second prepared nonresident replacement,
  and 2 second fresh-application warm-cache selected-target limits are the
  blocking representative refinement contract;
- the fixed-LOD matrix's 1.20 maximum homogeneous linear normalized ratio is
  an intra-run channel-scaling shape gate. It is not a cross-revision
  regression allowance and cannot replace the 1.05 component-baseline gate;
- the open static-recovery campaign's 1.05 fixed-GPU and 1.10/1 ms product
  ratios remain attached to that separate claim-specific A/B/C protocol; and
- playback transition floors and maximum-gap formulas remain owned by the
  composed temporal presentation gate and are not rewritten as static viewer
  cadence.

Implementation must update every current document that calls 16.667 ms a hard
product guideline. Historical tables may retain their exact result when
clearly labelled historical. No historical 20/50, 50/100, 1.20, or playback
result is retroactively reevaluated under the new contract.

## Definitions

The implementation and reports must use the following terms exactly.

### Trusted Workstation

The owner-controlled Linux x86_64 machine containing the qualified NVIDIA
GeForce RTX 3070 Ti Laptop GPU and using the Vulkan product path. It is not a
GitHub runner and receives no repository upload or status credential merely
to execute GPU tests.

### Correctness Case

A test whose pass/fail result is determined by semantic, numerical,
resource-ownership, synchronization, validation, or presentation facts.
Elapsed time may enforce a bounded timeout but may not decide performance
acceptance.

### Component Performance Measurement

A controlled measurement of a named renderer or application boundary, such as
GPU render-pass time, CPU planning time, queue-submit time, or scaling ratio.
It is diagnostic and regression evidence. It is not by itself proof of
displayed product cadence.

### Product Performance Measurement

A measurement taken through the normal release application, representative
workload, real input path, mapped window, qualified GPU, and trustworthy
visible-output boundary.

### Input Admission

The first canonical application boundary at which one OS input event or
explicitly generated product input sample becomes authoritative for the
current viewer intent. Input generation, window receipt, and admission remain
separate timestamps.

### Correct Changed Frame

A complete visible image that corresponds to the newest eligible camera,
cross-section, time, mode, sampling, layer, surface, and selected-quality
identity at the measurement point. A repeated image while geometry changes,
an old image relabelled current, a blank or partial frame, or an image at an
undeclared coarser scale is not a changed frame.

### Presented Frame

A correct changed frame observed at a boundary demonstrated to follow actual
surface presentation. Application publication, texture registration, command
submission, GPU completion, X11 client-surface change, or egui paint-command
queuing is not automatically a presented frame.

### Resident Target

The complete selected target body is already present in the renderer's exact
residency authority before the measured release. No data read, decode, dataset
request, or payload upload is required.

### Cached Or Prepared Nonresident Target

The selected body is not yet the current renderer front, but required source
data is available from the declared warm cache or prepared product path. This
condition must be proved by read/decode/upload counters rather than inferred
from elapsed time.

### Fresh-Application, Warm-OS-Cache Target

The application and GPU residency begin fresh while the operating-system file
cache is intentionally warm. It is not described as genuinely cold storage.

### Exact Settlement

The selected target for the final admitted intent has been published as one
complete, current, correct visible result. Its exact body, scale map, frame,
surface, extent, source, layer membership, mode, sampling, and synchronized
target identities agree; foreground work for that target is idle. Worker
completion, complete uploads, a GPU fence, an internal current flag, or
progress reaching 100 percent is insufficient alone.

### Visible Stall

An interval during active authoritative input in which no correct changed
frame is presented even though a complete resident navigation body is
available. Waiting for explicitly classified cold loading is reported
separately and cannot be silently included or excluded.

### Invalid Run

A run whose declared environment or measurement authority was not present,
including suspend, display disconnect, focus/input loss, changed adapter,
driver reset, thermal or power-policy violation, competing GPU load above the
declared bound, dropped trace events, clock discontinuity, corrupted report,
or missing required workload fact. Invalid is not pass or fail. The complete
run may be repeated only after the invalidating fact is recorded and removed;
individual slow samples may not be discarded.

## Target Verification Topology

| Lane | Command target | Primary evidence | Maximum target duration |
| --- | --- | --- | ---: |
| Ordinary public verification | Existing `cargo xtask verify-pr` | Portable compile, policy, unit, contract, and UI checks | Existing limit |
| Trusted GPU correctness | `cargo xtask verify-local trusted-gpu-correctness` | Exact hardware correctness inventory | 15 minutes |
| GPU performance campaign | `cargo xtask gpu-performance --config <private-config>` | Baseline/candidate component and product metrics | 30 minutes per revision pair, excluding build |
| Mapped product validation | `cargo xtask product-validate <cell-package> representative_gpu_interaction` | Normal-product correctness, cadence, stalls, and refinement | 15 minutes |

Every trusted command requires
`MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1`. The implementation must fail
immediately when `GITHUB_ACTIONS=true`, when the selected backend is not
Vulkan, or when the selected adapter is not the configured trusted adapter.

The exact command spelling above is the target interface. The old
`verify-local trusted-gpu` command is deleted at cutover rather than retained
as an alias. Documentation, help output, generated Nextest configuration, and
the verification registry change atomically with that deletion.

Compilation time is reported separately from test or campaign duration. A
cold build may take longer and does not alter the lane's execution budget.
Correctness and performance processes run serially with one test thread and no
competing Mirante GPU process.

## Trigger Policy

### Trusted GPU Correctness Is Required When

Any changed file can alter:

- WGSL source, shader composition, entry points, bindings, formats, or
  compilation;
- renderer pipelines, device limits, buffer layout, allocation, upload,
  residency, eviction, compaction, directory, capture, Pick, timing, or
  submission behavior;
- render-API controls, transforms, sampling, validity, coverage, LOD, frame
  identity, presentation target, or requirement bodies;
- application-to-renderer planning, execution, currentness, retry, failure,
  exact settlement, composed presentation, or texture handoff;
- dataset leases, resource identity, decoded payload layout, or lifetime
  assumptions consumed by the renderer;
- WGPU, Naga, Vulkan feature selection, compiler flags, or a dependency that
  can alter generated shaders or GPU behavior;
- a GPU fixture, independent oracle, tolerance, selector, registry entry,
  timeout, or test harness; or
- the qualifying adapter or required capability policy.

An uncertain boundary is treated as affected. Documentation-only wording,
purely portable test changes, or unrelated application behavior does not
require a GPU run unless it changes a claim or selector.

### GPU Performance Is Required When

Any changed file can alter:

- shader instruction work, memory access, branching, sampling, compositing, or
  per-layer scaling;
- selected LOD, navigation ladder, target eligibility, work estimation,
  resolution, viewport extent, mode, sampling, or visible-layer membership;
- CPU planning, command recording, queue submission, buffer allocation,
  upload, residency, compaction, scheduling, cancellation, or exact-row
  batching;
- application repaint, presentation, input coalescing, currentness,
  refinement, or mapped-display cadence;
- dataset read/decode/upload work on the representative viewer path;
- WGPU, compiler, driver, feature, release-profile, or dependency behavior
  capable of changing timings; or
- a performance workload, metric, sampling method, baseline, threshold, or
  measurement hook.

Performance is not required for an unrelated correctness-only edit whose
executed work is proved unchanged. That proof must be concrete; a comment
that performance should be unaffected is insufficient.

Before a release or a new performance claim, both the correctness and
performance boundaries run even if no individual changed file was classified
as performance-sensitive.

### Product Validation Is Required When

Rendering, viewport, input, presentation, GPU, loading, LOD, refinement, or
visible UI behavior changes. The owner may explicitly waive a product run for
a named revision, but the result then remains implemented or
automated-verified only and carries the waiver as remaining risk.

## Trusted GPU Correctness Lane

### Execution Contract

The correctness lane must:

1. require a clean immutable revision;
2. select the exact configured Vulkan adapter;
3. discover the exact registered 25-case minimum inventory;
4. run only correctness cases, serially, with zero retries;
5. enforce a 15-minute global timeout and a bounded per-case timeout;
6. capture WGPU validation output and the first typed device failure;
7. fail on any panic, timeout, skipped expected case, validation error,
   unsupported required capability, unexpected adapter, or nonzero exit;
8. report every discovered and executed exact name;
9. distinguish test failure from infrastructure invalidity; and
10. write a bounded local report under `target/` without treating the report
    as stronger than the native process exit.

A direct named `cargo test ... --ignored` invocation remains useful while
developing on a dirty tree. It is diagnostic only and cannot qualify the
revision or replace the clean-lane result.

### Assertion Contract

Each correctness case must be classified into one or more of these evidence
types:

- independent numerical or hand-authored pixel facts;
- independent coverage, validity, depth, position, scale, or Pick facts;
- resource identity, byte, pin, lease, allocation, upload, or residency facts;
- submission, atomicity, ordering, cancellation, or stale-suppression facts;
- adapter, capability, memory, or typed failure facts; or
- exact application/renderer presentation-handoff facts.

Where a pixel tolerance is necessary, the case must name the channel,
encoding, tolerance, and reason. Production CPU/GPU self-agreement is not an
independent oracle. An independent reference renderer may be used only when
its relevant algorithm and expected inputs do not share the production
implementation under test.

Internal counters may support a case but may not substitute for its primary
meaning. For example, a submission count cannot prove correct pixels, and an
`Exact` flag cannot prove that the displayed frame represents the selected
target.

Correctness cases may enforce a generous finite deadline to detect hangs.
They may print timings for diagnosis, but a timing number may neither pass nor
fail correctness except for that deadline.

### Mixed-Test Extraction

`resident_coordinated_volume_gpu_timing` currently proves meaningful
structural facts in addition to printing timing. The cutover must create a
correctness owner that proves:

- the nine-brick working set becomes exact through the product coordinator;
- resident movement performs zero reads, decodes, requests, payload uploads,
  residency submissions, evictions, allocator plans, control-buffer
  allocations, bind-group creations, or pipeline creation;
- each changed frame performs exactly the intended color submission;
- the final identical settled cutoff performs zero renderer submissions;
- timing tickets, if enabled for the separate benchmark, bind the exact target
  and frame; and
- WGPU emits no validation error.

The performance benchmark then measures the same declared resident work
without owning those structural assertions as its only protection.

The native terminal and fixed-LOD timing tests must likewise move reusable
scientific, presentation, zero-upload, and validation prerequisites to their
correctness owners. Performance execution still rechecks the prerequisites
needed to prove matching work, but a skipped performance run can no longer
skip their only regression coverage.

## GPU Performance Campaign

### Evidence Levels

The campaign contains two levels that remain separately labelled:

1. component benchmarks using GPU timestamps and application CPU clocks; and
2. normal-product interaction and refinement measurements.

Component results may localize a regression. Product results decide whether
the representative viewer meets the agreed interaction contract. A component
pass cannot override a product failure, and a product pass cannot bless
incorrect component output or changed scientific work.

### Environment

Every comparable baseline/candidate pair must use:

- the same physical workstation and GPU;
- Linux x86_64 and the Vulkan backend;
- the same display connection, refresh configuration, 1920×1080 product
  window or explicitly recorded target extents, and compositor session;
- the same driver, WGPU dependency lock, release profile, and relevant
  environment;
- AC power and the same configured performance/power policy;
- no competing Mirante process, game, video encode, GPU benchmark, package
  update, or other material GPU load;
- the same representative package, workload identity, camera path, layout,
  visible layers, modes, sampling, transfers, LOD decisions, and cache
  condition; and
- synchronized monotonic clocks without suspend or resume.

The report records the public adapter name, backend, driver version, OS/kernel
class, display refresh, target extents, power-policy result, process/revision
identity, and invalidating conditions. It must not publish a private path,
dataset label, acquisition metadata, geometry, or scientific identity.

A driver, GPU, backend, OS, power-policy, release-profile, workload,
measurement, or presentation-authority change invalidates direct comparison
with the stored baseline. Establish a new baseline rather than applying an
arbitrary correction factor.

### Component Workloads

The retained component topology is:

1. native terminal volume at 1920×1080 over the fixed complete 64³ resource,
   covering MIP, DVR, and ISO with voxel-exact and smooth-linear sampling;
2. the existing 32-case fixed-LOD multichannel matrix, with its exact kernel,
   sampling, compatibility, channel-count, resource, and payload topology;
3. the resident coordinated 1280×720 nine-brick volume case; and
4. supporting CPU planning, queue-submit, upload, residency, validation, and
   work-identity counters for each applicable case.

These are stable microbenchmarks. They are never called representative Cell
performance, visible frame rate, or all-hardware performance.

Each measured component case uses:

- a release executable;
- at least 30 untimed warm-up frames;
- exactly 120 measured frames after warm-up;
- GPU render-pass and batch-envelope timestamps where supported;
- CPU planning and queue-submit intervals where applicable;
- nearest-rank median and p95 without interpolation;
- the complete raw vector retained locally;
- zero measured-sample uploads unless uploads are the named workload; and
- zero validation errors.

If 120 measured frames make the declared 30-minute campaign budget impossible,
implementation stops for owner review. It may not silently lower the sample
count or keep a five-sample p95.

The former 16.667 ms assertion becomes a preferred component observation, not
a universal hard product gate. A component render pass above 33.3 ms proves
that path cannot independently sustain 30 Hz and therefore fails its
applicable absolute feasibility check. A component below 33.3 ms is necessary
but does not prove the full application can sustain 30 Hz. The product gate
remains authoritative.

### Representative Product Workload

The representative campaign uses the normal release application and the
owner-provided Cell package. The private configuration freezes, without
publishing private facts:

- package identity;
- exact executable and revision;
- 1920×1080 mapped client geometry and actual per-target extents;
- four-panel and standalone 3D layouts;
- visible layer order and transfer settings;
- voxel-exact and smooth-linear sampling;
- MIP, DVR, ISO, Mixed, and applicable Pick operations;
- initial camera and cross-section state;
- selected, installed, displayed, and ideal per-layer LOD maps;
- input path and exact gesture sequence; and
- warm-resident, cached/prepared, or fresh-application warm-OS-cache state.

Each candidate and baseline executes three fresh sessions with no retries.
Each session:

1. opens the representative package through the normal product path;
2. reaches and verifies exact/current target fidelity;
3. performs 30 seconds of independently clocked resident standalone-3D
   rotation and zoom;
4. performs 30 seconds of independently clocked resident four-panel linked
   translation and compound rotation at a safe non-quarantined profile;
5. releases input and measures already-resident exact settlement;
6. crosses one declared resident-envelope boundary while retaining the prior
   complete front and measures cached/prepared exact replacement;
7. repeats the declared fresh-application, warm-OS-cache four-panel startup to
   complete-coarse and selected-target milestones;
8. exercises the applicable mode/sampling matrix with fixed short gestures
   without changing the declared scientific work;
9. interrupts refinement with new input and proves the obsolete candidate
   cannot publish;
10. leaves the final view idle and proves exact/current settlement and zero
    avoidable foreground submissions; and
11. terminates finitely while preserving the complete report.

The S0 linked host-stress workflow is not part of this sequence. A safe
representative linked profile must be used. This plan does not convert the
quarantined diagnostic into an unattended gate.

### Presentation Measurement Boundary

The product cadence clock must observe a boundary demonstrated to follow
actual surface presentation. Before activation, the implementation must prove
with an independent controlled sequence that:

- one submitted but unpresented image does not count;
- repeated presentation of unchanged pixels does not count as changed work;
- two distinct queued images are not collapsed into a fabricated cadence;
- resize, occlusion, focus loss, and surface recreation are detected;
- the observed target and extent match the normal product window; and
- the measurement cannot advance solely from application publication, egui
  texture registration, X11 client-surface writes, or internal currentness.

If no trustworthy presentation boundary is available through the current
stack, numeric displayed-cadence and click-to-photon results remain
unevaluated. Internal admission/publication metrics may still diagnose the
application, and the owner may still product-validate visible correctness,
but neither can be relabelled as a displayed 30 Hz performance pass. Selecting
a substitute measurement requires an owner-approved amendment.

### Metrics

Every product session records, where applicable:

- generated input interval and count;
- window receipt interval and count;
- input admission interval and count;
- input-admission-to-next-correct-presented-frame latency;
- correct presented-frame intervals;
- maximum active visible gap;
- coalesced, superseded-before-work, stale-after-work, and current intents;
- selected, installed, displayed, ideal, provisional, and exact scale maps;
- release-to-resident exact settlement;
- nonresident target admission-to-exact replacement;
- fresh-application demand-to-complete-coarse publication;
- fresh-application demand-to-selected-target exact settlement;
- GPU render-pass and batch-envelope intervals;
- CPU planning, command recording, and queue-submit intervals;
- reads, decodes, requests, uploads, upload bytes, evictions, allocator plans,
  compactions, buffer/bind-group/pipeline creation, and submissions;
- hidden refinement jobs, batches, rows, cancellation, and handoff;
- validation, device, capacity, demand, renderer, and presentation failures;
  and
- settled-idle work.

The report emits every session's raw local vector, median, p95, p99 where
required, maximum, and scenario result. Correlated frame samples are not
pooled across sessions to manufacture a larger independent sample. Campaign
summary uses the median of the three session-level values and reports the
worst session separately.

### Absolute Interaction And Refinement Contract

All limits below apply to the representative workload and trusted
workstation, not universally.

| Situation | Metric | Hard limit | Preferred |
| --- | --- | ---: | ---: |
| Continuous interaction | p95 interval between correct presented frames | 33.3 ms | 16.667 ms |
| Resident input response | p99 admission to next correct presented change | 50 ms | Report |
| Active visible stall | Maximum gap while a resident navigation body exists | 100 ms | Report |
| Already-resident release | Release to exact/current selected target | 250 ms | Report |
| Cached/prepared nonresident replacement | Target admission to exact/current replacement | 1 second | Report |
| Fresh application, warm OS cache | Demand to complete coarse four-panel result | 250 ms | Report |
| Fresh application, warm OS cache | Demand to exact selected-target settlement | 2 seconds | Report |
| Genuinely cold storage | Separately reported loading metric | No GPU gate | None |

The 33.3 ms limit is a displayed-product limit. It is not assigned wholesale
to the GPU render pass because input handling, CPU planning, command
submission, UI composition, surface acquisition, and presentation also
consume time.

The 50 ms input-response limit permits a rare missed 30 Hz update. The 100 ms
maximum is the hard visible-freeze boundary. A repeated, stale, blank,
partial, snapped-back, or silently coarser image cannot reset either clock.

Refinement may use a retained complete predecessor, but that image remains
truthfully provisional for the newest selected target. Refinement completes
only at exact settlement as defined above. New input during refinement must
become visible under the interaction limits, supersede obsolete hidden work,
and prevent the obsolete result from publishing later.

Genuinely cold disk, network, removable-media, launch, and OS-cache-warmup
latencies are recorded separately. They neither consume the GPU refinement
budget nor disappear from the report.

### Relative Regression Contract

Absolute and relative gates both apply:

- a candidate over an absolute product limit fails even if it improves on a
  slower baseline;
- a candidate with a material regression fails even if it remains under the
  generous absolute limit; and
- a candidate that changes scientific work or quality is incomparable, not
  faster.

Before activating relative gates, the accepted baseline revision runs ten
fresh calibration sessions under the fixed environment. For each run-level
metric, calibration records the median, full range, and maximum proportional
deviation from the ten-run median.

The target relative gates are:

- component GPU p95 must be at most 1.05 times the comparable accepted
  baseline;
- warm product p95 and refinement latency must be at most the greater of 1.10
  times baseline or baseline plus 1 ms; and
- zero-work and correctness prerequisites require exact equality where the
  product contract declares zero.

The 5 percent component gate activates only when calibration's maximum
proportional deviation is at most 2.5 percent. The 10 percent product gate
activates only when that deviation is at most 5 percent. If natural variation
exceeds half the intended regression allowance, the benchmark is unstable:
the implementation must stabilize the environment or measurement. It may not
widen the threshold, discard slow valid samples, or declare a pass.

Comparable candidate campaigns interleave accepted-baseline and candidate
processes in fixed A/B, B/A, and A/B pair order. Each pair uses the same
session configuration. The campaign reports all three ratios and uses their
median for the relative decision. One pair may not be deleted because it is
slow; only a complete objectively invalid pair may be rerun after recording
the invalidation.

### Baseline Lifecycle

One small sanitized baseline manifest under `verification/` records:

- schema and measurement version;
- accepted source revision;
- public platform, adapter, backend, driver, and display profile;
- opaque representative workload profile identifier;
- mode, sampling, layout, target extent, and cache-condition identifiers;
- per-metric baseline values, calibration variation, relative thresholds, and
  absolute thresholds;
- date and explicit owner acceptance; and
- superseded baseline identity when updated.

Private configuration, package paths, raw samples, host identifiers, and
scientific facts remain outside the repository. Raw reports live in ignored
local `target/` output or an owner-controlled private directory.

A baseline changes only after:

1. the candidate is correct;
2. matching work is proved;
3. applicable absolute and relative gates pass or an explicit owner decision
   accepts a known regression;
4. the normal product is inspected;
5. the owner approves the new baseline; and
6. the manifest change names why the predecessor is superseded.

The benchmark never rewrites its own baseline. Deleting, weakening, or
replacing a baseline solely to make a candidate green is a test failure and a
plan violation.

## Mapped Product Validation

Automated timing and readback remain supporting evidence. Final product
validation uses the normal release application on the real mapped display,
representative Cell workload, trusted GPU, and actual input path.

The owner observes:

- continuous standalone 3D and linked four-panel response;
- no blank, stale, partial, snapped-back, or falsely labelled frame;
- truthful shown, selected, ideal, provisional, refining, exact, and current
  state;
- a complete retained predecessor throughout cold refinement;
- one atomic exact replacement;
- cancellation and replacement when new input interrupts refinement;
- all supported modes and both sampling policies;
- no repeating renderer, capacity, or validation error; and
- finite exact settlement and clean idle.

Owner observation establishes visible product acceptance. It does not invent
numeric presentation timestamps when the presentation boundary is
unavailable. Conversely, numeric timing cannot override an observed visible
defect.

## Reporting And Failure Semantics

Each lane writes one bounded machine-readable report plus concise human
output. The report includes:

- command and exact revision;
- dirty/clean status;
- exact discovered, selected, executed, passed, failed, skipped, and
  unevaluated names;
- adapter, backend, required capabilities, and environment result;
- workload and measurement-profile identifiers;
- timings and raw-vector location;
- correctness and matching-work prerequisites;
- absolute, relative, preferred, and product-validation results separately;
- first failure or invalidation boundary;
- important skips or waivers; and
- remaining risk.

Native process status is authoritative. A report cannot override a failed,
timed-out, killed, incomplete, or unsupported test process.

Result vocabulary is fixed:

- `pass`: every applicable requirement was evaluated and passed;
- `fail`: a requirement was evaluated and violated;
- `invalid`: the declared environment or measurement authority failed;
- `unevaluated`: required evidence was unavailable or not run; and
- `not-applicable`: the plan explicitly excludes the requirement for the
  named change.

`invalid`, `unevaluated`, and `not-applicable` are never serialized as
`pass`. A complete closeout may contain `not-applicable` only with the
trigger decision and reason. It may not contain `invalid` or
`unevaluated`.

## Milestones

### G0 — Freeze Inventory And Current Evidence

- Record the exact current 27-case discovered inventory and selector output.
- Record current test names, assertions, fixture/oracle ownership, ignored
  reasons, capabilities, timeouts, and known baseline failures.
- Record the three current timing functions and their 39 measurements.
- Confirm that no current run or historical result is relabelled by the
  refactor.
- Add focused registry tests for duplicate, missing, renamed, unassigned, and
  unexpectedly unignored GPU cases.

Exit: one reviewed inventory maps every current case to its target owner.

### G1 — Split Verification Authorities

- Add exact `trusted-gpu-correctness` registry and Nextest ownership.
- Add the explicit GPU performance campaign command and configuration schema.
- Keep mapped product validation separate.
- Delete the mixed `trusted-gpu` selector and command in the same cut.
- Preserve local-only, clean-revision, exact-adapter, timeout, serial, and
  zero-retry guards.

Exit: no command can run correctness and call the result performance, or skip
correctness because performance was not requested.

### G2 — Extract And Strengthen Correctness

- Move resident coordinator structural assertions into their correctness
  owner.
- Move terminal and multichannel scientific/zero-work prerequisites into
  correctness owners where necessary.
- Review every case for an independent expected fact and meaningful failure.
- Replace metadata-only or self-agreement assertions where an independent
  oracle is required.
- Make exact discovery and executed count fail closed.

Exit: at least 25 exact correctness functions run and every one can identify
the user- or renderer-meaningful defect it catches.

### G3 — Rebuild Component Performance

- Convert the three timing functions into the declared component benchmark
  families.
- Use release builds, 30 warm-ups, 120 measurements, raw vectors, median, and
  nearest-rank p95.
- Separate structural prerequisites from timing decisions.
- Replace the 16.667 ms hard product wording with the preferred component
  observation and full-product authority defined here.
- Add calibration stability and matching-work rejection.

Exit: all 39 component measurements are stable, comparable, and honestly
labelled.

### G4 — Establish Trustworthy Presentation Measurement

- Implement or select the actual-present boundary.
- Prove it with distinct queued, repeated, occluded, resized, focused,
  unpresented, and surface-recreated cases.
- Retain internal admission/publication clocks as diagnostics only.
- Fail numeric cadence qualification when presentation feedback is absent.

Exit: the product clock measures presented changes rather than internal work.

### G5 — Implement Representative Product Campaign

- Freeze the private Cell workload configuration.
- Implement the three-session standalone, four-panel, resident, nonresident,
  fresh-application, mode, sampling, interruption, and idle sequence.
- Record every metric and correctness prerequisite defined above.
- Enforce the 33.3/50/100 ms interaction limits and 250 ms/1 s/2 s refinement
  limits.
- Exclude genuinely cold storage from the GPU gate while still reporting it.
- Keep the linked-S0 host-stress path quarantined.

Exit: the normal product can evaluate every absolute limit on the trusted
workstation.

### G6 — Calibrate And Activate Relative Gates

- Run ten accepted-baseline calibration sessions.
- Reject unstable metrics under the half-threshold rule.
- Create the sanitized baseline manifest.
- Run interleaved baseline/candidate pairs.
- Prove same work, quality, cache condition, and environment.
- Require explicit owner acceptance for baseline replacement.

Exit: relative gates distinguish material regression from measured noise
without auto-rebaselining.

### G7 — Close Documentation And Product Evidence

- Update current state, architecture where measurement authority changes,
  testing, development commands, current work, and verification metadata.
- Remove superseded command and 60 Hz hard-product wording.
- Run documentation, registry, focused, public, trusted correctness,
  performance, and mapped-product checks appropriate to the final revision.
- Obtain owner observation of the mapped product.

Exit: implemented, automated-verified, and product-validated claims are
separate and accurate.

## Expected Implementation Surface

The implementation is expected to touch only the smallest useful set of:

- `crates/mirante4d-render-wgpu` GPU correctness and timing cases;
- `crates/mirante4d-app` application GPU and mapped-product cases;
- `crates/xtask` local verification, performance, and product orchestration;
- `verification/registry.json` and generated Nextest configuration;
- one sanitized baseline manifest under `verification/`;
- testing/development documentation and current-state authority;
- presentation timing integration only to the extent required for an honest
  measurement boundary; and
- focused portable tests for discovery, parsing, calculation, and failure
  semantics.

No GitHub workflow, branch protection, public runner, cache, artifact upload,
paid compute, or public self-hosted runner is part of the implementation.

## Deletion Gate

The refactor is incomplete while any predecessor below remains:

- the mixed `verify-local trusted-gpu` command or selector;
- one report that combines correctness and performance into one pass;
- correctness assertions owned only by a timing function;
- five-sample p95 performance decisions;
- a hard 16.667 ms universal or product requirement for the terminal
  component benchmark;
- a small synthetic fixture described as representative viewer performance;
- internal publication, egui paint, X11 client-surface change, GPU completion,
  or submission described as actual surface presentation without proof;
- average or p95-only reporting that can hide a visible gap over 100 ms;
- refinement completion based solely on worker, upload, submission, progress,
  or currentness metadata;
- a performance comparison with changed scientific work, quality, LOD,
  resolution, layer set, mode, sampling, cache state, or environment;
- automatic baseline replacement;
- unexpected ignored or skipped cases accepted as green;
- broad selector enrollment without exact discovered-name reconciliation;
- documentation implying GitHub executes or is required to execute the local
  GPU lane; or
- duplicate obsolete commands and threshold descriptions in current
  documentation.

No compatibility alias or hidden fallback retains the mixed lane after G1.
Git history is the archive.

## Risks And Controls

### Performance Tests Become Too Expensive

Control: correctness and performance remain separate; performance runs only at
its affected boundary. Component execution is capped at 30 minutes per
revision pair and product validation at 15 minutes. If the declared sample
count cannot fit, stop for review instead of silently weakening statistics.

### One Workstation Is Mistaken For Universal Support

Control: every report and claim names Linux x86_64, Vulkan, RTX 3070 Ti,
resolution, workload, and measurement profile. Other hardware can supply
diagnostics but cannot inherit this baseline.

### A Noisy Benchmark Produces False Failures

Control: ten-run calibration, half-threshold stability requirements,
interleaved comparisons, serial execution, fixed environment, raw vectors,
and explicit invalidation. An unstable benchmark is repaired, not widened.

### A Generous Absolute Limit Hides Regression

Control: every comparable candidate must pass both absolute and relative
gates.

### A Fast Wrong Image Passes

Control: exact scientific, currentness, selected-scale, target, extent, and
presentation prerequisites precede every timing decision. Wrong work is
incomparable and fails correctness.

### A Rare Freeze Hides Behind Good p95

Control: p95 cadence, p99 input response, and maximum visible gap are separate
blocking metrics.

### Cold I/O Is Blamed On The GPU

Control: resident, cached/prepared, fresh-application warm-OS-cache, and
genuinely cold storage states are separately named and proved with counters.

### Internal Publication Is Relabelled Visible

Control: G4 requires an independently demonstrated post-presentation boundary.
Without it the numeric product cadence gate is unevaluated.

### Product Observation Becomes Subjective Permission To Ignore Numbers

Control: owner observation and numeric gates are independent. Neither
overrides the other.

### Baseline History Grows Into Qualification Bureaucracy

Control: one small sanitized manifest and bounded raw local output are
sufficient. No receipt, signing, replay, role executable, artifact upload, or
persistent population system is introduced.

### Hardware Testing Damages The Workstation

Control: the quarantined linked-S0 workload remains excluded; commands have
finite timeouts, serial execution, input-release and process cleanup, and
explicit invalidation. Destructive OOM, device loss, driver reset, or thermal
stress injection remains outside scope.

## Hard Stop Conditions

Stop implementation and request owner direction if the plan requires:

- a public or private GitHub self-hosted runner;
- paid hosted compute, cache, or artifact storage;
- a second renderer, CPU fallback, alternate backend, or device-recovery path;
- changed renderer scientific semantics, sampling, transfer, validity, or
  output resolution to meet a threshold;
- a GPU-budget, storage-format, brick, shard, compression, or dataset-format
  change;
- running the quarantined linked-S0 host-stress workflow;
- destructive device-loss, out-of-memory, reset, or thermal fault injection;
- publication of private dataset or host facts;
- accepting internal publication as monitor-visible without the G4 proof;
- widening a threshold because calibration is noisy;
- reducing the 120-sample component requirement because the campaign is slow;
- auto-updating a baseline from the candidate;
- removing a current correctness case without named replacement evidence; or
- recreating retired qualification/provenance machinery.

## Explicit Non-Goals

- Changing product rendering, LOD, sampling, transfer, or scientific output.
- Making 60 Hz mandatory.
- Qualifying 4K, Windows, macOS, Metal, DX12, AMD, Intel, integrated GPUs, or
  every Vulkan adapter.
- Running GPU tests on GitHub.
- Making every GPU or performance case a routine pull-request prerequisite.
- Reopening linked-S0 host-stress qualification.
- Redesigning playback cadence or temporal policy.
- Closing the separate R11/P7 static-recovery campaign merely by installing
  this testing topology.
- Publishing the private Cell dataset or its identifying metadata.
- Benchmarking genuinely cold storage as GPU refinement.
- Supporting automatic device recovery or a CPU renderer.
- Adding retries, flaky-pass policy, or quarantine-based green status.

## Implementation Record

The repository cutover completed on 2026-08-03 with these concrete
authorities:

- `trusted-gpu-correctness` owns 25 exact ignored test names; discovery,
  registration, ignored status, selection, serial execution, timeout, and
  zero-retry policy fail closed;
- `gpu-performance` owns exactly the native-terminal, fixed-LOD multichannel,
  and resident-coordinator benchmark functions and validates all 39 emitted
  measurements, raw 120-sample GPU vectors, applicable CPU vectors, medians,
  nearest-rank p95 values, zero uploads, validation status, and the 1.20 shape
  diagnostic;
- `cargo xtask gpu-performance --config <private-config>` implements clean
  baseline/candidate builds, automatic controlled presentation activation,
  ten-run calibration proposals, A/B–B/A–A/B comparison, stability,
  matching-work, absolute and relative gates, private raw evidence, and a
  sanitized public report without automatic baseline replacement;
- the private configuration schema is
  `verification/gpu-performance-config.schema.json`; the accepted baseline
  schema and fail-closed pending placeholder are under `verification/`;
- the normal release product scenario records named resident, prepared,
  interruption, and idle counter checkpoints and rejects changed work, hidden
  reads/decodes/uploads, missing replacement work, obsolete publication, or
  avoidable settled-idle submissions;
- the opt-in observer accepts a correct changed frame only after final
  swapchain `VK_KHR_present_wait` completion, independent marker readback, and
  matching `VK_EXT_present_timing` first-pixel-out feedback; it rejects or
  excludes unchanged, superseded, out-of-date, window-unavailable, failed,
  timed-out, cross-generation, or ambiguous samples and requires strict ID,
  marker, clock-domain, and generation ordering;
- first-pixel-out deltas qualify exact scanout cadence, while Vulkan wait-
  return times retain visibility, maximum active visible gap, conservative
  resident input response, and coarse refinement settlement; X11 detects
  resize, map, focus, visibility, and extent conditions, and no photon or
  input-to-photon claim is made; and
- the old `trusted-gpu` command, selector, mixed report meaning, five-sample
  benchmark decision, and hard 16.667 ms product wording are removed from
  current authority.

Portable parser, topology, statistics, script, matching-work, structural
counter, observer, and registry tests are part of ordinary verification. The
ignored hardware executions are not. G0–G5 and the G6/G7 repository machinery
are implemented. G6's replacement presentation preflight and current-tree
exact-scanout product smoke pass, but ten accepted calibration runs and
relative gates remain pending. G7 now has clean trusted-correctness, automated
mapped-product evidence, and all eight absolute product metrics; explicit
owner acceptance remains open. Those pending items keep this plan open under its
completion standard without making the implementation itself ambiguous.

The final portable working-tree closeout passed Clippy, exact discovery and
lane ownership, and all 1,478 routine unit/contract/UI cases with zero
retries. The audit found the complete 42-case ignored inventory and accepted
its exact registered ownership; it did not execute the 25-case trusted GPU
correctness lane or three-function performance campaign. No accepted baseline,
hardware pass, displayed-cadence result, or owner product-validation claim is
created by this portable result.

## First Trusted Correctness Execution

The subsequent owner-authorized clean execution used commit
`a6ef07aea1ea04bddeb77efd7ad9cbd0f97a236d`, tree
`e63dd568ae8a7f315301cf18ad8e7db60f63c3e1`, the expected Vulkan backend,
and the NVIDIA GeForce RTX 3070 Ti Laptop GPU. Discovery and exact inventory
reconciliation passed. The serial, zero-retry native run selected all 25
registered correctness cases and reported 19 passed and six failed.

Because the native process exited nonzero, the lane is failed as required.
Its current wrapper report retained the selected inventory but did not retain
the available per-case outcomes after failure, so it conservatively marked all
25 selected names unevaluated. That reporting limitation does not erase the
native result or convert any case to pass. The follow-up plan linked above
includes exact failed-run attribution alongside the six case repairs.

No performance function, ten-run calibration, comparison pair, accepted
baseline, numeric mapped-product campaign, or owner product observation was
part of this correctness execution.

## Correctness Follow-Up Execution

The approved follow-up repaired the six diagnosed boundaries and the failed-
run attribution defect. Its clean immutable execution used commit
`efb745221ba3a78d00a20fe14b843d07f5272d06`, tree
`acbcce58fe91ac7e1dd7c1623998a3ab2b22bf11`, the expected Vulkan backend, and
the NVIDIA GeForce RTX 3070 Ti Laptop GPU. Discovery and exact inventory
reconciliation passed. All 25 selected cases started, executed, and passed
serially with zero retries, skips, not-started cases, validation errors, or
unattributed outcomes. The structured report status was
`complete_native_success`.

On that same clean revision, normal release-product automation passed all 141
commands in `target_fixture_render_modes` and all 60 commands in
`representative_native_navigation`. This is mapped automated evidence; it is
not a substitute for owner visual acceptance.

## Initial Performance Calibration Activation And Boundary Replacement

The owner-selected package passed production catalog preflight as multi-layer,
and the qualified workstation entered the controlled campaign environment.
The X11 presentation probe originally reported that it stopped after its
1920x1080-to-1600x900 resize while waiting for coordinated presentation
settlement. Live replay on 2026-08-03 corrected that attribution. Its
one-second progress sidecar could lag behind fast command changes, an in-pass
repaint request could be consumed by native geometry transition, and the
script eventually asked the same UI loop to minimize and later restore the
window even though that loop became dormant while minimized.

The current-tree repair separates those authorities. Command changes publish
immediately while same-command heartbeats remain one-second bounded. Only the
non-measured boundary probe receives an out-of-pass UI wake. The parent runner
detects the exact PID-bound X11 client, observes iconic/hidden state, performs
one real unmap, then maps/restores/focuses it. The app holds the minimize
command until its independent observer has seen a map after that unmap and
eframe sees a non-minimized focused viewport, so later geometry cannot race
the restore. The script also takes the required final nonblank GPU screenshot.
A current-tree real-display verification passed all 29 product commands with
exit zero and matching requested, observed, and captured 1920x1080 extents.

That run also proved the formerly planned X11 Present authority unavailable:
it observed the lifecycle and target extent but recorded zero X11 Present
completion events. No timing claim was inferred from it. The owner then
approved the Vulkan present-wait amendment above.

The replacement is integrated at the existing final WGPU Vulkan present call
and is inactive in ordinary product runs. After the NVIDIA 595.84 upgrade, the
owner-approved first-pixel-out amendment was implemented with revision-3
`VK_EXT_present_timing`. The final controlled current-tree proof passed all 29
product commands and the required nonblank capture. It completed all 212
successful presents across nine timing-configured swapchains, classified
three normal out-of-date recreation presents separately, drained and excluded
one result returned for a rejected ID, accepted 11 changed marker-correlated
images including a post-remap record, and recorded zero fatal rejection,
unknown or duplicate ID, queue exhaustion, wait/timing failure, timeout,
clock change, ambiguity, or outstanding record.

The representative interaction then passed all 89 product commands, its
required nonblank GPU capture, the exact first-pixel-out authority, and all
eight absolute product gates. It completed 4,448 successful present waits and
timing records with 4,043 correctly changed product presentations. Standalone
and four-panel nearest-rank scanout interval p95 were 16.96 and 17.01 ms;
resident input-response p99 was 49.65 ms; maximum active visible gap was 88.69
ms; resident exact settlement was 45.36 ms; prepared nonresident replacement
was 97.50 ms; startup coarse visibility was 5.88 ms; and startup exact
settlement was 1.008 seconds. These are current dirty-tree activation/smoke
results on the named workstation, not ten-run calibration, accepted relative
baseline, photon visibility, input-to-photon latency, or owner visual
acceptance. The campaign still has zero accepted calibration runs, and no
baseline was created or replaced.

## Documentation And Authority Closeout

This closeout keeps implemented facts synchronized in:

- `docs/CURRENT_STATE.md` for the live verification topology and remaining
  limits;
- `docs/ARCHITECTURE.md` only if the presentation measurement changes a
  production authority or data flow;
- `docs/TESTING.md` for current commands, triggers, metrics, thresholds, and
  claim language;
- `docs/DEVELOPMENT.md` for retained developer commands;
- `docs/planning/NOW.md` for active milestone and status;
- `docs/README.md` and `docs/documentation-index.json` for inventory;
- `verification/registry.json` and generated Nextest configuration for exact
  lane ownership; and
- predecessor plans only for narrow target-supersession links where needed.

No implemented wording appears before the mixed predecessor is deleted. No
automated-verified wording appears before the exact named checks pass on the
same revision. No performance wording appears without workload, hardware,
metric, sampling method, cache condition, and threshold. No product-validated
wording appears before the owner exercises the normal mapped application.

## Completion Standard

This plan is complete only when:

1. G0 through G7 are closed;
2. the exact correctness and performance inventories reconcile;
3. every correctness case passes on the trusted adapter with zero unexpected
   skips and zero validation errors;
4. all 39 component measurements execute with the declared sampling method;
5. calibration proves the active metrics stable enough for their relative
   thresholds;
6. the representative product campaign evaluates and passes every applicable
   absolute and relative gate;
7. the trustworthy presentation boundary is demonstrated;
8. the mapped normal product is exercised and accepted by the owner;
9. the mixed predecessor command, misleading 60 Hz hard-product wording,
   five-sample p95, and every deletion-gate item are absent;
10. documentation and verification metadata agree with the implemented
    state; and
11. no invalid or unevaluated required result remains.

A green ordinary test suite, a direct dirty-tree GPU invocation, a component
benchmark below 16.667 ms, internal `Exact` metadata, an owner statement
without the required numeric campaign, or a numeric campaign without correct
visible output does not complete this plan.
