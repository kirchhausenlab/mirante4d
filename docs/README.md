# Documentation

This is the sole owner of Mirante4D's documentation read order and human
index. The machine inventory is
[`documentation-index.json`](documentation-index.json).

## Read Order

1. [Product](PRODUCT.md) — product scope and non-goals.
2. [Current state](CURRENT_STATE.md) — implemented facts and limitations.
3. [Current work](planning/NOW.md) — the current checkpoint and next package.
4. The document that owns the task:
   - [Architecture](ARCHITECTURE.md)
   - [Data format and safety](DATA_FORMAT.md)
   - [Testing and validation](TESTING.md)
   - [Development commands](DEVELOPMENT.md)
   - [Release status](RELEASE.md)
   - [Decisions](decisions/README.md)
   - [Unresolved backlog](BACKLOG.md)

Agents must also follow the [agent guide](AGENTS.md). Dependency-policy
exceptions have one separate tool-owned
[ledger](DEPENDENCY_EXCEPTIONS.md).

## Plans

The
[pre-alpha reliability and packaging plan](plans/active/PRE_ALPHA_RELIABILITY_AND_PACKAGING.md)
is the active checkpoint for publishing the completed rendering work, making
native clean shutdown single-owner, exposing earlier-launch provisional
autosaves in the packaged application, and building one validated local Linux
x86_64 pre-alpha package.

The
[resident 3D navigation ladder plan](plans/active/VIEWER_RESIDENT_3D_NAVIGATION_LADDER.md)
is implemented, verified, and owner product-validated. It replaces the binary
selected-target or terminal-volume preview choice with one bounded, globally
accounted ladder of complete native-resolution volume bodies. The
representative mapped Cell run installed S6 through S3 plus exact S2, selected
safe resident S4 during navigation, rejected S3 and S2 as
interaction-unsafe, and completed all 60 product commands with zero renderer
validation errors.

The
[GPU memory and asynchronous refinement plan](plans/active/VIEWER_GPU_MEMORY_AND_ASYNC_REFINEMENT.md)
is the implemented renderer-foundation handoff. It owns selected-adapter
native GPU-memory discovery, a growable payload arena that starts with a
bounded physical commitment and caps growth headroom, and hidden exact
refinement that progresses independently of visible vsynced application
frames. Implementation and focused, trusted-Vulkan, and normal-product
verification completed on 2026-07-30.

The
[native-resolution navigation plan](plans/active/VIEWER_NATIVE_RESOLUTION_NAVIGATION.md)
is the implemented renderer/import handoff. It hard-cuts reduced-output 3D
previews in favor of a deterministic geometry-bounded pyramid, one
renderer-owned resident coarse tail, native-resolution stable navigation,
independent linked-2D/3D frame identities, retained-preview settlement, and
focused smooth-linear/ISO fast paths.

The
[bounded 3D presentation plan](plans/active/VIEWER_BOUNDED_3D_PRESENTATION.md)
is the superseded predecessor checkpoint. Its reduced-output preview,
same-camera preview upgrades, and visible timing-probe policy are deleted by
the native-resolution cut. Its retained-preview settlement, hidden exact
screen-row batches, atomic whole-frame promotion, latest-only cancellation,
and one renderer/residency authority remain.

The
[progressive multiscale presentation plan](plans/active/VIEWER_PROGRESSIVE_MULTISCALE_PRESENTATION.md)
is the completed linked-2D coarse-fallback/refinement, unbounded plane
navigation, and coherent 3D fast-path preservation handoff. Its hard-cut
implementation and focused automated checks are complete. The owner also
completed the ordinary mapped four-panel exercise and confirmed that the
corrected transient coarse fallback no longer lingers.

The
[GPU placeability and recovery plan](plans/active/VIEWER_GPU_PLACEABILITY_AND_RECOVERY.md)
is the completed correctness handoff for the fragmented payload-arena
incident, graceful coarser selection, recoverable retry, truthful capacity
diagnostics, and stale-warning cleanup. Its implementation and focused
automated checks are complete, and the owner confirmed the correction in the
normal product.

The
[linked-2D LOD truth and settlement plan](plans/active/VIEWER_LINKED_2D_LOD_TRUTH_AND_SETTLEMENT.md)
is the closed linked fidelity/evidence handoff. Its product correction,
evidence hard-cut, independent 3D/linked-2D inspector reporting, and
diagnostic instrumentation are implemented. A first mapped diagnostic
completed S3 and S1 but froze the desktop while approaching S0, so the S0
workflow remains quarantined and produced no valid monitor-performance result.
That withdrawn evidence branch is not unfinished product work.

The
[viewer rendering pipeline overhaul](plans/active/VIEWER_RENDERING_PIPELINE_OVERHAUL.md)
completed its authorized S3 continuity correction and remains the
implementation/evidence handoff for that scope. It does not establish
linked-2D LOD-transition fidelity or performance.

The
[adaptive LOD and global capacity plan](plans/active/VIEWER_ADAPTIVE_LOD_AND_GLOBAL_CAPACITY.md)
completed the subsequently observed finer-LOD capacity correction and remains
its implementation/evidence handoff. The implemented policy is general across
catalog levels, targets, datasets, layouts, and hardware, but its mapped zoom
evidence exercised the independent 3D panel rather than linked-2D zoom.

The development-simplification and viewer-recovery program completed on
2026-07-26; [current state](CURRENT_STATE.md) owns its implemented facts, Git
history retains the completed plan, and the verified external recovery bundle
preserves the unpublished research spike.

The viewer performance refactor closed on 2026-07-31 after final owner
product validation. No rendering-performance package remains active.
Smooth-linear sampling remains supported with a known fine-scale refinement
cost relative to voxel-exact; any optimization or removal is a separate
future product decision.

Two future outcomes remain deliberately separate from ordinary development:

- [public microscopy data](plans/deferred/OPEN_DATA_FOLLOW_ON.md);
- [possible segmentation](plans/deferred/SEGMENTATION.md).

## Documentation Rules

- Keep one authority for each fact and link to it instead of copying it.
- Label implemented, target, deferred, and reference material honestly.
- Update the owning document in the same change that changes a fact.
- Delete superseded plans and policies; Git history is the archive.
- Keep private datasets, machine paths, credentials, and unpublished metadata
  out of public documentation.
- Run `cargo xtask docs-check` before merging documentation changes.
