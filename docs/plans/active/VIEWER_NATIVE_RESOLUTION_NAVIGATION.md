# Viewer Native-Resolution Navigation Plan

> Scheduling amendment (2026-07-30): the
> [GPU memory and asynchronous refinement plan](VIEWER_GPU_MEMORY_AND_ASYNC_REFINEMENT.md)
> supersedes this plan's one-strip-per-coordinated-cutoff mechanism. Native
> preview, private-candidate, cancellation, and atomic-promotion semantics
> remain; hidden rows now advance in renderer-owned bounded asynchronous
> batches and require explicit application publication authorization.

- Status: IMPLEMENTED AND VERIFIED
- Planning requested by owner: 2026-07-30
- Implementation authorized by owner: 2026-07-30
- Last reviewed: 2026-07-30

This document is the authoritative implementation and handoff plan for the
hard cut from reduced-output 3D previews to native-resolution navigation over
a deterministic geometry-bounded coarse pyramid. It also closes the
presentation and ownership defects exposed while product-validating the
bounded 3D presentation work.

The implementation replaces the reduced-resolution and visibly probing parts
of
[the bounded 3D presentation plan](VIEWER_BOUNDED_3D_PRESENTATION.md). It
retains that plan's hidden native-resolution exact refinement, atomic
whole-frame promotion, latest-only cancellation, dedicated volume kernels,
and one renderer/residency authority.

## Owner-Observed Problems

The normal Cell workflow established that linked 2D now has the desired
progressive behavior, but the first 3D controller has unacceptable product
behavior:

- interaction can visibly alternate between preview LODs and internal
  resolutions while the controller probes its timing envelope;
- releasing a gesture can report or expose
  `preview -> pending output -> direct` instead of retaining the existing
  preview until one exact promotion;
- reduced internal output can collapse to only a few pixels, especially for
  smooth-linear ISO, even though a complete coarse volume is available;
- smooth-linear presentation is charged and rendered far more expensively
  than voxel-exact, with manual repeated sparse sampling and disabled skip
  opportunities;
- ISO exact progress is reported as hundreds of apparent "tiles" even though
  these are screen-row strips, and unrelated ISO thresholds share one timing
  family despite radically different first-hit behavior;
- linked-2D-only input advances one global frame identity and cancels or
  restarts an unchanged hidden 3D exact candidate; and
- the importer has a nominal bounded-navigation policy, but an early
  generation cutoff, `OR` terminal predicate, and seven-level ceiling prevent
  it from being the deterministic geometry contract the viewer needs.

The representative Cell dataset's current terminal level is
`41 x 32 x 64` (83,968 voxels). Failure to render that complete resident
volume smoothly at a native framebuffer is a renderer defect, not a reason to
create S7-S9 or reduce screen resolution.

## Outcome

The product will use one deterministic data-space rule and one
native-resolution presentation rule:

1. Import generates factor-two spatial levels until the terminal level
   satisfies both a maximum spatial dimension of 64 and at most 262,144
   spatial voxels per layer/timepoint.
2. Scale count follows dataset geometry. A small base already satisfying the
   contract remains single-scale; a larger source receives however many
   distinct levels are required. Time is never reduced.
3. The viewer retains a fixed, byte-bounded coarse tail for the active
   timepoint and visible layers. Every retained coarse level uses ordinary
   canonical data and one renderer-owned residency authority.
4. The complete terminal level is the guaranteed navigation floor. Any finer
   complete retained coarse level may be used only when its deterministic
   native-resolution work class is accepted.
5. Every visible 3D navigation frame is rendered at the physical panel
   extent. There is no reduced internal output extent, dynamic resolution,
   preview upscaling, perceptual-resolution floor, or visible performance
   probe.
6. One gesture freezes one uniform preview body. A newer sample changes camera
   geometry but does not oscillate presentation policy. A one-way move to the
   terminal floor is allowed only if the chosen body violates the fixed
   interaction contract.
7. The same complete preview remains visible and truthfully labelled after
   release while the native-resolution exact candidate renders in hidden
   strips. Only the completed candidate swaps atomically.
8. Linked 2D and 3D own independent latest frame identities. A linked-only
   change cannot invalidate, cancel, restart, rerender, or relabel an
   unchanged 3D front/candidate; a 3D-only camera change cannot invalidate the
   linked group.

The supported product framebuffer envelope remains 1920x1080. This is a
project engineering target, not a universal contract for arbitrary displays
or adapters.

## Non-Negotiable Invariants

### Scientific And Storage Integrity

- Source microscopy data is never mutated.
- Pyramid work remains bounded, cancellable, restartable, deterministic, and
  atomically published through the existing importer/storage authorities.
- Every new scale is represented by the one current package profile. There is
  no compatibility reader, migration shim, alternate package type, or
  file-per-brick path.
- Existing validity semantics remain exact. Sentinel levels retain their
  guarded valid-only reduction and centered transforms; ordinary inputs retain
  their declared current reduction until a separately justified scientific
  cut changes it.
- Preview data is provisional display data. Exact capture, pick, analysis, and
  export remain bound to the completed exact presentation.

### One Renderer And One Residency Authority

- `mirante4d-render-wgpu` remains the sole product renderer.
- Coarse-tail retention is a policy inside the existing global residency
  owner, not a second cache or fallback renderer.
- Terminal full-volume pages are pinned by the navigation body and sampled
  through a dedicated full-volume fast path that bypasses repeated sparse
  lookup when one complete resource covers the layer.
- Smooth-linear full-volume sampling resolves one page/resource once and
  reuses its decoded addressing facts for the complete interpolation
  footprint. Hardware-filterable representation may be used only within the
  same authority and without changing exact scientific output.
- Exact fine rendering retains the ordinary sparse page directory, canonical
  payloads, shaders, hidden candidate, and atomic promotion.

### Native Output And Stable Presentation

- `request extent == output extent == physical 3D panel extent` for every
  preview and direct volume frame.
- No controller may shrink preview width or height.
- GPU timing may diagnose and size hidden exact strips. It may not visibly
  alter preview LOD or output resolution during a gesture.
- A preview remains the visible front through loading and hidden exact work.
  Backend loading state must not replace its presentation label with
  `pending output`.
- The inspector reports shown uniform LOD, selected/ideal LOD, preview versus
  exact, and exact rows/strips completed. It does not call screen strips data
  tiles.

### Independent Target Currentness

- One monotonically allocated global sequence may provide unique values, but
  3D and linked 2D each retain their own latest target revision.
- Camera, 3D viewport, and 3D-affecting render changes advance 3D.
- Cross-section geometry and linked viewport changes advance linked 2D.
- Layer, timepoint, transfer, sampling, source, and full-layout changes
  advance every affected family once.
- The coordinated renderer already owns per-target observed frames; requests
  in one cutoff may therefore carry different frame identities.

## Deterministic Pyramid Contract

For each layer's spatial base shape, import repeats factor-two ceil reduction
while either terminal condition is still violated:

```text
while max(z, y, x) > 64 OR z * y * x > 262,144:
    generate the next spatial level
```

Generation stops only when:

```text
max(z, y, x) <= 64 AND z * y * x <= 262,144
```

The implementation deletes:

- the separate `maximum dimension <= 256` no-pyramid shortcut;
- the terminal `OR` predicate;
- the importer-local one-through-seven-level restriction; and
- a storage-profile scale ceiling that can reject a geometry-required level
  before the terminal contract is reached.

Checked scale-count and metadata-size ceilings remain finite. A factor-two
chain reaches a fixed point at `1 x 1 x 1`; failure to make geometric progress
is typed rather than looping.

## Resident Coarse Tail

Pyramid termination and GPU retention are related but distinct.

The terminal scale is always part of every nonempty 3D and linked navigation
body. In addition, the viewer may retain the contiguous coarse tail beginning
at the first scale whose decoded active-layer/timepoint bytes fit the fixed
navigation-tail budget. Because a 3D factor-two level contains approximately
one eighth as many voxels as its predecessor, retaining the entire tail costs
approximately `8/7` of its largest member before bounded metadata overhead.

The first implementation uses the existing complete navigation-floor body as
the mandatory terminal authority and admits contiguous finer coarse levels
only through the normal globally accounted union. It does not increase the
configured GPU ledger or evict an exact visible front to manufacture a second
cache.

For a complete terminal layer covered by one canonical resource, the volume
control record publishes that resource's page-record index and full-volume
fact. The volume shader then:

- resolves the page once per layer/ray rather than hashing every segment;
- reuses origin, shape, payload, dtype, and validity metadata;
- performs direct native payload loads for voxel-exact sampling; and
- performs one whole-footprint smooth interpolation without repeated page
  lookup or repeated resource-bound checks.

The generic sparse path remains authoritative for multi-page exact bodies.

## 3D Presentation Policy

### During Interaction

- If the selected complete body belongs to a statically accepted
  native-resolution navigation work class, render it directly.
- Otherwise render the complete terminal navigation body at native output
  resolution.
- Freeze the selected body for the gesture.
- Cancel a mismatched hidden exact candidate at the next existing strip
  checkpoint.
- Never probe a larger visible preview, change internal resolution, or
  alternate between bodies because of a timing observation.

### At Settlement

- Retain the exact preview texture and label.
- Prepare the selected exact body and native-size hidden candidate.
- Render deterministic horizontal strips sized by the hidden-work authority.
- Publish no partial candidate and no intermediate presentation state.
- Atomically swap once after the final matching strip.

### Smooth Linear And ISO

- Smooth-linear no longer reduces screen resolution by an eight-tap
  multiplier.
- Whole-resource interpolation hoists invariant resource metadata and avoids
  eight complete sparse sample calls.
- Conservative page skipping for smooth sampling may be enabled only with
  halo-aware bounds that cannot miss a contributing interpolation footprint.
- ISO timing/work identity includes a bounded threshold/occupancy class, so
  one display level cannot poison unrelated ISO work.
- Exact progress is named rows/strips, and its count is not presented as
  dataset loading.
- Connected-component or "dust" removal is not introduced as a hidden
  performance approximation. Any future cleanup remains an explicit
  scientific visualization feature.

## Hard Cut And Deleted Behavior

This implementation deletes:

- aspect-preserving reduced 3D preview textures;
- visible safe/probe work-envelope oscillation;
- preview scoring that trades dataset detail against screen undersampling;
- the half-linear-resolution preference and material preview upgrades;
- `preview HxW / output HxW` product state;
- global-frame invalidation of independent targets;
- replacing a retained preview label with `pending output` while exact work is
  hidden;
- describing horizontal exact strips as loaded data tiles; and
- treating the scale ordinal `S6` as the navigation contract.

The contract is the terminal geometry and resident-tail policy, regardless of
its ordinal in a particular dataset.

## Milestones

### N0 — Plan And Authority

- Record this owner-approved hard cut before source changes.
- Link it from documentation authority and mark the former reduced-resolution
  policy as superseded only after the cut is implemented.

### N1 — Geometry-Bounded Import

- Replace the current generation shortcut/terminal predicate with the exact
  deterministic contract.
- Remove the seven-level package restriction and align storage profile limits.
- Update scale transforms, package metadata checks, independent expected
  shapes, import fixtures, and focused import tests.

### N2 — Independent Target Revisions

- Split latest 3D and linked-2D frame ownership in the mailbox.
- Classify durable changes and viewport observations by affected family.
- Use the target-specific identity in demand planning, renderer matching,
  settlement, diagnostics, and automation.
- Prove linked-only input preserves an in-progress and a settled 3D
  presentation.

### N3 — Native-Resolution Stable Preview

- Remove preview extent calculation, quality scoring, visible timing probes,
  and same-camera preview upgrades.
- Select one complete uniform resident body and render it at output extent.
- Freeze gesture preview policy and preserve it through settlement.
- Retain timestamps only for diagnostics/direct facts and hidden strip sizing.

### N4 — Coarse-Tail Fast Path

- Publish complete single-resource full-volume facts in renderer control.
- Add direct whole-resource voxel and smooth-linear sampling.
- Keep the terminal body resident through the existing navigation floor and
  include useful contiguous coarse-tail resources through ordinary global
  accounting.
- Prove no sparse-directory probe occurs inside the terminal resource's sample
  loop and no upload/residency work occurs during warm navigation.

### N5 — Truthful Settlement And ISO/Smooth Work

- Keep the preview presentation visible and labelled while backend exact work
  is pending.
- Rename exact progress from data tiles to screen strips/rows.
- Separate ISO threshold behavior in bounded work identity.
- Retain one stable hidden strip height per exact candidate.

### N6 — Focused And Product Verification

- Check small, Cell-like, long-thin, odd-dimension, and larger-than-seven-level
  pyramid chains against independent expected shapes.
- Validate import cancellation, source preservation, package opening, scale
  metadata, and scientific verification.
- Check native-resolution preview invariants and absence of reduced extents.
- Check target-family revision independence and unchanged 3D candidate
  progress during linked-only input.
- On trusted Vulkan, compare terminal fast-path MIP/DVR/ISO/Mixed output with
  the ordinary exact path and independent numerical oracle.
- Measure warm native-resolution terminal rendering at 1920x1080 for
  voxel-exact and smooth-linear MIP/DVR/ISO, naming adapter, dataset shape,
  mode, sampling, trials, and GPU duration.
- Exercise the normal mapped application with the representative Cell
  dataset in four-panel and standalone 3D, including camera motion,
  interruption, linked-only motion while 3D refines, smooth-linear, ISO, and
  exact settlement.
- Do not run the quarantined linked-S0 host-stress workflow.

## Acceptance

Implementation is complete when:

- generated datasets always reach the deterministic terminal geometry;
- the arbitrary seven-level limit and early cutoff are absent;
- every visible 3D preview is native-resolution;
- no visible adaptive probe or internal-resolution oscillation remains;
- the terminal navigation path is complete, warm, and bounded through the one
  renderer authority;
- linked-only input neither restarts nor rerenders unchanged 3D work;
- preview remains visible until one exact atomic swap;
- smooth-linear and ISO no longer manufacture tiny output or misleading data
  tile counts;
- focused and broad automated checks pass; and
- the normal mapped product is exercised and its actual visible result is
  reported separately from automation.

Performance acceptance is a practical product guideline on the supported
1920x1080 Vulkan workstation, not a universal contract. A failure of the
native terminal path on that boundary is a renderer defect to diagnose; it
does not authorize silent output-resolution reduction.

## Closeout Evidence

The implementation and its required product path completed on 2026-07-30.

- Geometry tests cover single-scale, odd, long-thin, fifteen-level, and
  maximum-`u64` chains. The package profile admits 64 levels, which covers the
  59-level representable factor-two maximum.
- Focused ownership tests prove that linked-only changes preserve 3D frame
  identity, visible presentation, and hidden exact progress.
- Trusted Vulkan comparisons prove that the complete-resource fast path
  agrees with the sparse path and independent numerical oracles for
  voxel-exact and smooth-linear MIP, DVR, and ISO.
- The release timing fixture rendered a warm resident 64³ volume at
  1920x1080 on the NVIDIA GeForce RTX 3070 Ti Laptop GPU. Five-trial p95 GPU
  times were 1.247 ms MIP voxel-exact, 3.253 ms DVR voxel-exact, 9.988 ms ISO
  voxel-exact, 6.776 ms MIP smooth-linear, 10.278 ms DVR smooth-linear, and
  11.128 ms ISO smooth-linear. Every case remained below the 16.667 ms
  workstation guideline.
- The normal release application completed the 60-command
  `representative_native_navigation` Cell scenario in four-panel and
  standalone 3D. It exercised linked rotation, 3D zoom/orbit, linked-only
  input during hidden 3D settlement, both sampling modes, MIP, DVR, ISO, and
  final exact settlement. All nine GPU readback captures were nonblank, the
  final presentation was complete/current native output, target-family
  revision gaps were zero, and the report contained no validation, capacity,
  demand, or renderer fault.
- The hidden-strip continuation regression proves an unfinished atomic exact
  candidate continues scheduling until its final strip and remains invisible
  until the one atomic promotion.
- The renderer now keeps two questions separate: whether the visible front
  satisfies a request, and whether either private allocation still owns work.
  A matching preview front can therefore be adopted while its hidden exact
  candidate remains executable. A trusted Vulkan regression holds both states
  simultaneously and proves preview adoption does not suppress exact
  continuation.
- The final uninstrumented release application completed the same 60-command
  representative scenario twice consecutively. This repetition includes the
  formerly stranded interleaving of 3D camera input, linked-only input, a
  retained preview front, and an unfinished hidden exact candidate.
- The final `cargo xtask verify-pr` gate passed repository policy, lint,
  discovery, and all 1,285 unit/contract/UI cases (563/402/320) with no
  unexpected Clippy warnings.

The quarantined linked-S0 host-stress workflow was not run and is not part of
this closeout.
