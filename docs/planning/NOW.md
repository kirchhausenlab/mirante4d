# Current Work

Last updated: 2026-07-31

## Current Status

No implementation package is currently active. The
[pre-alpha reliability and packaging plan](../plans/active/PRE_ALPHA_RELIABILITY_AND_PACKAGING.md)
is complete: the rendering checkpoint is preserved on its publication
branch, native clean shutdown has one exit-time owner, unsaved provisional
autosaves are proactively exposed for explicit actor-validated recovery, and
the clean Linux x86_64 release directory, AppImage, and tarball passed their
package checks. The unpacked packaged executable also passed the mapped
render-mode matrix and the exact three-launch crash/recovery/native-close
scenario with zero retries, panic-free stderr, and byte-identical source
data. This remains a local pre-alpha handoff, not a supported public release.
The next implementation package requires a new owner decision.

The viewer performance refactor is closed as of 2026-07-31. The owner
exercised the normal application after the final resident-navigation-ladder
cut and reported that four-panel and 3D behavior works as expected. No
rendering implementation, performance-repair, or validation package remains
active. Smooth-linear sampling stays functionally supported but has a known
fine-scale refinement cost relative to voxel-exact; any optimization or
removal is a separate future product decision.

The
[resident 3D navigation ladder plan](../plans/active/VIEWER_RESIDENT_3D_NAVIGATION_LADDER.md)
is implemented, verified, and owner product-validated. The 3D controller no
longer chooses only between the exact target and the terminal body. One
globally accounted terminal-to-fine ladder is retained, only complete
GPU-resident and target-eligible rungs may be selected normally, and one
gesture freezes the finest rung inside the native interaction-work envelope.
A cold terminal prefix may initiate its own bounded upload, but it is neither
reported resident nor presented before GPU completion. Every rung remains a
uniform native-resolution volume body; the common aggregate suffix is
residency-only and cannot affect pixels.

The final mapped Cell scenario retained S6/S5/S4/S3 plus exact S2 and selected
S4. Resident S3 and S2 were rejected as interaction-unsafe, all 60 normal-app
commands completed, and the Vulkan renderer reported zero validation errors.
Focused planner, selection, render-contract, integration, and trusted atomic
GPU checks pass.

The
[GPU memory and asynchronous refinement plan](../plans/active/VIEWER_GPU_MEMORY_AND_ASYNC_REFINEMENT.md)
is implemented and verified. Default GPU policy now derives from typed memory
facts for the exact native window adapter; unavailable/shared-memory cases
remain explicit fallbacks and persisted settings remain overrides. Logical
payload capacity is separate from a 64 MiB initial physical commitment, and
growth is tied to exact per-segment placement with at most 128 MiB speculative
headroom. Hidden exact batches now advance on renderer-worker GPU submission
completion rather than the visible vsynced repaint clock. This changed no
storage representation, bricking policy, shader sampling semantics, or
explicit user budget.

On the selected 8 GiB RTX adapter, an isolated recommended-settings product
run exposed a 2.652 GB logical payload maximum with only 64 MiB committed and
1.24 MiB resident. The representative Cell run preserved the explicit 1 GiB
setting, passed all 60 product commands with zero validation errors, and
recorded 688 independently scheduled hidden batches. Focused app/renderer/UI
tests and trusted Vulkan memory, growth-preservation, and atomic exact-output
checks pass.

The
[native-resolution navigation plan](../plans/active/VIEWER_NATIVE_RESOLUTION_NAVIGATION.md)
is implemented. It replaces reduced-output 3D previews and visible timing
probes with a deterministic geometry-bounded pyramid, a renderer-owned
terminal navigation floor, native-resolution stable navigation,
retained-preview exact settlement, independent 3D/linked-2D frame identities,
and a complete-resource GPU fast path. It restores no second renderer,
compatibility format, GPU-budget increase, hidden scientific approximation,
or quarantined linked-S0 workflow.

The
[bounded 3D presentation plan](../plans/active/VIEWER_BOUNDED_3D_PRESENTATION.md)
is superseded. Its reduced extents, visible timing probes, and same-camera
preview upgrades are deleted. Every current preview renders at the physical
panel extent, one gesture freezes one uniform body, the same preview remains
visible through hidden screen-row batches, and only one exact promotion
follows.

Focused geometry tests prove that small data can remain single-scale and that
odd, long-thin, 15-level, and maximum-`u64` shapes reach the terminal
`max-dimension <= 64 AND voxels <= 262,144` contract. The storage profile
admits 64 levels, covering the 59-level representable maximum. Focused
mailbox and application tests prove linked-only input preserves 3D frame
identity and hidden progress, and gesture release does not allocate a second
frame.

Trusted Vulkan checks prove the one-resource terminal path agrees with the
ordinary sparse path for voxel-exact and smooth-linear MIP, DVR, and ISO, with
dedicated independent numerical oracles retained. On the NVIDIA GeForce RTX
3070 Ti Laptop GPU, warm resident 64³ rendering at 1920×1080 produced p95 GPU
times from 1.247 ms to 11.128 ms across those six cases, below the 16.667 ms
product guideline. The normal release application also passed the 60-command
representative Cell navigation scenario with nine nonblank GPU captures,
exact/current native settlement, independent target revisions, and no
validation, capacity, demand, or renderer fault.

The foundation refactor through WP-15, import/preprocessing cutover, and
corrective `uint8` sentinel restoration remain implemented. Their scientific,
source-safety, persistence, bounded-resource, and product-boundary guarantees
remain in force.

The development-simplification and viewer-recovery program completed on
2026-07-26. EP-00/EP-01 qualification, receipt, replay, provenance, repeated
resource-union hashing, and related development gates remain deleted. The
normal product, independent scientific oracles, hard resource bounds,
source-safety checks, and focused real-GPU checks remain.

The
[viewer rendering pipeline overhaul](../plans/active/VIEWER_RENDERING_PIPELINE_OVERHAUL.md)
completed its authorized high-risk scope. It remains the canonical
implementation and evidence handoff. The owner authorized P0 through P6 on
2026-07-28 and approved its real-interaction corrective amendment on
2026-07-29.

The
[adaptive LOD and global capacity plan](../plans/active/VIEWER_ADAPTIVE_LOD_AND_GLOBAL_CAPACITY.md)
also completed its authorized A0 through A5 scope on 2026-07-29. It remains
the implementation and product-evidence handoff for the later finer-LOD
capacity correction.

The
[linked-2D LOD truth and settlement plan](../plans/active/VIEWER_LINKED_2D_LOD_TRUTH_AND_SETTLEMENT.md)
remains the linked fidelity and evidence handoff. Its documentation and claim reset
and the linked-2D product correction are implemented. The owner then limited
the next package to evidence hard-cut, truthful GUI reporting, a faithful
diagnostic workload, and useful instrumentation. Those changes are
implemented, but the first full live diagnostic froze the desktop while
approaching S0. The workflow is quarantined; any later invocation requires
owner review and explicit host-risk acknowledgement. Later correctness and
progressive-presentation work does not use it as an unattended gate.

The owner subsequently reproduced history-dependent linked selection: one run
remained indefinitely at shown S1 with no selected target while another
reached exact/current S0, alongside a contradictory payload-residency warning
whose requested allocation was smaller than its reported available bytes. The
[GPU placeability and recovery plan](../plans/active/VIEWER_GPU_PLACEABILITY_AND_RECOVERY.md)
is the correctness handoff for that incident. Its implementation and focused
automated verification are complete, and the owner subsequently confirmed the
inconsistent selection/warning behavior was corrected in the normal product.

The diagnosed boundary was physical payload-arena placement. The renderer now
distinguishes aggregate exhaustion from a missing contiguous range, exposes
per-segment placeability facts, and performs one bounded renderer-owned
compaction before retrying exact-union preflight. A residual refusal tightens
the existing generic catalog selector monotonically until a feasible coarser
candidate installs or the true minimum fails. Recovery/backpressure is not
terminally latched, and successful linked planning/presentation clears its
matching historical warning. This cut changes no shader, storage format, GPU
budget, or performance policy.

The
[progressive multiscale presentation refactor](../plans/active/VIEWER_PROGRESSIVE_MULTISCALE_PRESENTATION.md)
is now implemented and product-validated. It makes a complete coarsest
navigation floor the linked panels' first-useful
coverage, refines target bricks visibly through plane-only multiscale
sampling, makes the finite plane guard a rolling fine-data optimization rather
than a presentation boundary, and preserves coherent atomic 3D refinement
with a direct ready-target fast path. The hard cut and focused automated
verification are complete. The owner completed the normal mapped four-panel
exercise and reported the view working as intended. The quarantined
host-stress workflow was not rerun and remains quarantined.

A corrective same-intent body cutover is now implemented after the normal
mapped viewer logged repeated `RequirementSetChanged` display-refresh errors
during view changes. Asynchronous linked planning retains the mailbox's input
revision, but a semantically different prepared body is now recognized as new
display work, accepted by the renderer, and submitted for all three linked
panels. Retained-frame adoption compares immutable body identity as well as
frame and extent, and the old eviction-regression guard cannot suppress a
different successor body. Focused lease and trusted-GPU checks cover the
transition. In the repeated normal mapped check, the owner observed at most a
millisecond-scale S4-or-S5 fallback before the proper scale returned, rather
than the former indefinite coarse display. The reported cutover defect is
product-validated as corrected.

## Closed Linked-2D Evidence Boundary

The earlier incremental-settlement defect is corrected, and the owner
confirmed that ordinary linked zoom now reaches visibly fine S0 output under
the appropriate view. XY/XZ/YZ remain one linked LOD group; the 3D camera,
LOD, refinement, and presentation remain independent.

The first real-window linked-wheel report was nevertheless invalid. It
converted internal coordinated-target publication into synthetic “visible”
samples, and its `XGetImage` artifacts observed the X11 client surface rather
than compositor/monitor presentation. All linked visible-change, visible-gap,
input-to-visible-latency, and pass/fail claims from that workflow are
withdrawn. The retained linked-wheel workflow is endpoint correctness only.

The inspector now reports `3D scale` separately from linked-2D shown,
selected, ideal, exact/provisional, refining, and display-current state,
splitting XY/XZ/YZ when their facts differ. The new diagnostic uses the normal
release application, exact warm S3/S1/S0 phases, and bounded nonstationary
60 Hz Shift-drag. It records input receipt, UI, planning/data/upload, renderer
CPU, linked GPU, and egui paint-command boundaries while explicitly leaving
surface present and monitor continuity unobserved.

One mapped exercise generated complete S3 and S1 phases and then froze the
desktop while trying to reach S0. The reboot left the app trace empty; system
logs contain no OOM, GPU-reset, or kernel-hang diagnosis. No performance
result or root-cause conclusion is valid. Linked S0 workflows are quarantined
behind explicit host-stress acknowledgement, with per-input receipt,
UI-heartbeat, strict input-cap, input-release guards, and live trace
checkpointing. Those guards have not been live exercised after the reboot.
This retained evidence limitation is not unfinished product work and does not
block the closed performance refactor.

## Corrective Rendering Result

P0 through P5 implemented the intended single-authority architecture: one
latest-intent mailbox, one CPU data plane, one renderer-owned global residency
authority, one frame coordinator and color-submit authority, dedicated
Plane/MIP/DVR/ISO/Mixed kernels, and a separate Pick kernel. Their focused
correctness and ownership checks remain useful.

The owner-observed four-panel/S3 oblique freeze was corrected at the linked
cross-section authority. Common transient geometry had been applied only to
the active plane even though XY, XZ, and YZ are one linked interaction.
Previously published numerical visible-gap and input-to-visible results for
that path are withdrawn because their “visible” samples were internal
publication events, not human-visible output.

The former `representative_four_panel_three_sessions` command did not exercise
that interaction. It injected application commands, let application progress
pace the input stream, measured internal admission-to-currentness, and
inspected only static pixels. It and the separate command-driven resident
navigation cadence proxy have now been deleted.

## Completed Product Checkpoint

The evidence reset, faithful reproducer, rendering-test hard cut,
localization, and production correction are complete. Active gestures now
preflight and atomically promote all three renderer-resident plane guards,
render the linked panels with active-first priority, invalidate stale pixels
at cold plan cutover, and adopt all three exact rendered frames atomically on
durable release. The change adds no fallback, second renderer, storage format,
or benchmark-only path.

The source-level correction and focused checks remain implemented, and the
owner later observed smooth ordinary S3 motion when the separate
resident-envelope boundary was not crossed. No automated linked continuity
pass or latency threshold is currently established. The former integration
checks remain source and policy evidence only; they cannot restore the
withdrawn monitor-visible claim.

## Adaptive LOD And Capacity Result

The later owner-observed finer-LOD freeze is corrected. Screen-derived ideal
LOD is now a quality target rather than mandatory admission. The normal viewer
separates ideal, aggregate-feasible selected, and complete displayed LOD;
starts from one complete, globally accounted navigation floor; and admits
generic catalog refinements only while the exact renderer-global retained
union fits. Fixed equal panel quotas and terminal latching of an unaffordable
ordinary refinement are deleted. This is one policy over every catalog level,
not an S3/S2 branch.

The renderer's existing residency owner remains the physical-capacity
authority. It transactionally preflights the proposed global requirement
union, including retained fronts and transition overlap. A worker result also
binds the exact union base it used; an intervening hidden refinement makes
that result stale and schedules current work again instead of producing a red
error. The UI reports shown, selected, ideal, refining, and adaptive states.
Only failure of the coarsest valid navigation plan remains a hard capacity
failure.

The real-window investigation also found a separate input multiplier: egui's
smoothed scroll tail had been reused as authoritative input on every repaint,
so one physical wheel event could generate many camera revisions and another
repaint tail. Camera and cross-section wheel handling now consumes only raw
current-frame wheel or pinch events; a focused test proves that no Mirante
wheel delta remains while egui smoothing is still nonzero.

Three fresh 30-second 3D-panel zoom sessions passed on the mapped normal app,
representative four-panel workload, and NVIDIA GeForce RTX 3070 Ti Laptop GPU.
Each deliberately observed a feasible finer display and an ideal boundary
constrained to a coarser selected/displayed level, then recovered to complete
S3. Across them the worst window-receipt, main-loop, externally
visible-change, and p99 input-to-visible observations were 35.838 ms,
33.638 ms, 54.763 ms, and 28.945 ms. Camera applications remained bounded by
real wheel receipts.

The five-minute 3D combined run performed a real 1854×1011 → 1100×650 →
1854×1011 resize, a 120-sample 3D orbit round trip, and 18,004 independently
clocked wheel inputs. It recorded 17,910 authoritative camera applications,
39.150 ms worst receipt gap, 34.181 ms main-loop gap, 86.384 ms worst external
visible-change gap, and 46.697 ms p99 input-to-visible latency. Both boundary
classes were present, no ordinary hard capacity error occurred, and the final
frame recovered to complete S3. The former fresh linked-oblique “pass” is
withdrawn because it did not observe monitor-visible output.

The native-resolution navigation plan and its resident-ladder successor are
implemented and verified. Their broad policy/Rust gates, trusted Vulkan
comparisons, and normal mapped representative Cell integration are complete.
The quarantined linked-S0 workflow remains outside that closeout. No
GPU-budget increase, fallback renderer, compatibility path, result hash,
receipt, or new qualification framework was introduced.

## Authority And Scope Boundary

The old automated P6 performance and storage conclusions are revoked. The
current package format and `64³` logical renderer/cache brick edge remain the
unchanged product defaults, not a newly proven performance decision. Storage
or format work remains unauthorized unless the faithful profile shows required
unavailable-brick physical I/O/decode is the first blocking boundary and the
owner approves a separate amendment.

No absolute all-hardware performance, comparison-viewer, or release claim is
active. The supported workstation guideline and its exact synthetic
measurement are recorded above. Public-data and segmentation outcomes remain
deferred and separate.

## Verification Boundary

Focused formatting, `xtask`, UI, application, selector, residency, capacity,
currentness, and stale-result checks pass. The final broad PR gate also passed
every policy phase, zero-warning workspace Clippy, exact lane discovery, and
all 1,298 current unit, contract, and UI tests. Those checks do not establish
linked-2D monitor continuity or transition performance: the mapped zoom
workflow targeted 3D, generic capture implicitly targeted 3D, and internal
publication cannot validate visible output. A passing suite cannot outvote
the mapped product. Product validation for this work means the normal native
application on the relevant real display, GPU, data, layout, LOD, and actual
input path, with direct owner observation where no trustworthy presentation
capture boundary exists.

## Next Package

There is no active viewer-performance package. The quarantined host-stress
diagnostic is retained only for an explicitly approved future investigation,
not as routine validation. Smooth-linear optimization or removal, public
benchmarks, 4K support, other platforms, storage-format experiments, and
release claims each require their own separately scoped decision.
