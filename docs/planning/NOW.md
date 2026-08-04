# Current Work

Last updated: 2026-08-04

## Active Viewer Correction

The
[viewer rendering correctness, recovery, and numerical cutover](../plans/active/VIEWER_RENDERING_CORRECTNESS_RECOVERY_AND_NUMERICAL_CUTOVER.md)
is the active product package.

Its P1 through P6 correctness cut is implemented. The current code has:

- complete logical transaction membership and renderer-owned reuse proof;
- one render-attempt, failure, wake, and composition authority;
- typed retryable and terminal outcomes that retain valid predecessor pixels;
- isolated color, Pick, hidden-worker, and device failure scopes;
- operation-scoped identity exhaustion;
- one binary32 affine, coordinate, camera, work, and page-traversal contract;
  and
- focused CPU and trusted-GPU regressions for those boundaries.

The mapped 141-command render-mode scenario and 60-command native-navigation
scenario pass. Direct trusted hidden-handoff and temporal pixel-replacement
checks also pass. These are automated results, not owner acceptance.

The package remains open for two positive outcomes:

1. P7 must run the controlled matching-work A/B/C static-performance campaign.
   The required pre-edit P0 campaign was not captured and is not claimed.
   P7 must identify a safe correction or retain the performance gap honestly.
2. P8 must use a suitable non-clipped temporal package, run the required clean
   trusted lane, exercise the normal mapped product, and obtain owner
   acceptance.

The bundled temporal fixture cannot satisfy the independent changed-pixel
capture gate. Do not weaken that gate or relabel the existing run.

## GPU Evidence

The [viewer GPU testing refactor](../plans/active/VIEWER_GPU_TESTING_REFACTOR.md)
has implemented separate correctness and performance lanes.

The 25-case trusted correctness lane is complete and green. The mapped Vulkan
observer can correlate product images with present completion and
first-pixel-out timing. The current absolute product smoke meets all eight
interaction and refinement limits.

The accepted performance baseline remains `pending_initial_calibration`.
Calibration requires ten accepted runs, explicit owner acceptance, and no
automatic replacement. One clean activation stopped on a real component
limit: the eight-channel voxel-exact ISO fixed-LOD case measured 58.767 ms p95
against the 33.3 ms limit. That failed activation contributes no calibration
runs.

## Next Actions

1. Select a suitable temporal package and complete the P8 normal-product
   workflow.
2. Run the P7 matching-work static-performance campaign on the designated
   workload and workstation.
3. Apply only a measurement-selected correction, if the campaign identifies
   one.
4. Repeat the clean trusted and mapped product checks after any correction.
5. Complete the ten-run GPU calibration and ask the owner to accept or reject
   the proposed baseline.
6. Obtain direct owner observation before closing the viewer package.

## Scope Boundary

The active work does not authorize:

- a weaker correctness, capture, or performance gate;
- a GPU budget increase;
- reduced-resolution rendering;
- a CPU or alternate renderer;
- a storage, shard, codec, or chunk-format change;
- a new device-recovery path;
- removal of smooth-linear sampling;
- the quarantined linked-S0 host-stress workflow;
- 4K or non-Linux qualification; or
- a broad new verification framework.

Import, preprocessing, storage, project-store testing, packaging, composed
playback presentation, progressive linked-plane refinement, placeability,
native-resolution navigation, and resident 3D navigation are closed product
or verification boundaries. Their current facts live in the domain documents.
Git history retains their completed plans.
