# Viewer GPU Memory And Asynchronous Refinement Plan

- Status: IMPLEMENTED AND VERIFIED
- Planning requested by owner: 2026-07-30
- Implementation authorized by owner: 2026-07-30
- Last reviewed: 2026-07-30

This document is the authoritative implementation and handoff plan for three
related renderer-foundation corrections:

1. replace the unknown-GPU-memory default with facts from the exact adapter
   selected by the native product;
2. stop eagerly creating the entire logical payload arena, which would turn a
   recommended 4 GiB renderer budget into approximately 3 GiB of payload
   buffers at startup; and
3. stop using visible, vsynced application frames as the clock for hidden
   exact-volume construction.

The plan follows the native-resolution navigation cut. It does not reopen
reduced-resolution previews, change the storage representation, add a second
cache or renderer, or promise a universal frame rate on arbitrary hardware.

## Owner-Observed Problems

The normal Cell workflow exposed two direct defects and one consequence:

- the native product calls `recommended_for_current_system(None)`, so the
  selected GPU's memory is never discovered and the renderer commonly runs
  with the 1 GiB unknown-device recommendation;
- increasing that recommendation on an 8 GiB adapter would currently allocate
  approximately 3 GiB of payload storage immediately, even when the active
  dataset needs only a fraction of it; and
- hidden smooth-linear exact refinement can advance by one screen row per
  visible application frame. At a 60 Hz desktop, a 946-row candidate therefore
  has a scheduling floor near 15.8 seconds even when each row takes negligible
  GPU time.

The last representative exact S2 body used 1,408 resources and approximately
366 MiB of decoded payload. The renderer reported approximately 408 MiB
resident, yet its current fixed payload arena committed its full approximately
729 MiB cap. The same run recorded thousands of visible frame/submission
cycles while hidden screen rows advanced. These are ownership and scheduling
defects, not evidence that trilinear interpolation inherently needs twenty
seconds.

## Outcome

The native product will have one truthful, bounded memory and work model:

1. The exact `wgpu::Adapter` selected for the eframe window is the sole source
   identity for native GPU facts.
2. On supported native backends, the app queries device-local heap size and,
   when the driver exposes it, the current driver memory budget and usage.
3. The recommended renderer budget is derived from those typed facts. A
   persisted user value remains an explicit override; unavailable discovery
   remains an explicitly labelled conservative default.
4. Renderer payload capacity remains a hard logical maximum, but backing
   buffers start small and grow on demand within that maximum. Geometric
   reuse is capped to at most 128 MiB of speculative headroom beyond each
   segment's proven placement high-watermark.
5. Diagnostics distinguish logical maximum, physically committed bytes,
   resident bytes, and growth/copy work.
6. Visible application presentation remains vsynced.
7. Hidden exact refinement runs under one renderer-owned, latest-only
   scheduler. It submits at most one bounded GPU batch ahead, observes
   completion, adjusts the next batch toward a fixed time budget, and checks
   cancellation between batches.
8. UI repaint is requested when a hidden candidate completes, fails, or needs
   product-visible state publication—not once per hidden row.
9. The existing preview remains visible until one matching complete candidate
   is atomically promoted.

## Non-Negotiable Invariants

### Selected-Adapter Memory Facts

- Memory discovery is performed against the exact adapter chosen for the
  product window. A separately enumerated or headless adapter cannot supply
  the product recommendation.
- Adapter identity, backend, device type, physical device-local bytes, driver
  budget, driver-reported usage, discovery source, and discovery availability
  are separate typed facts.
- On discrete Vulkan adapters, device-local heaps are reported as dedicated
  GPU memory. Unified/shared adapters are labelled as such; the UI and policy
  do not call their heap size dedicated VRAM.
- When `VK_EXT_memory_budget` is available, the usable recommendation ceiling
  is no greater than both the device-local heap size and the driver's current
  device-local budget.
- Discovery failure is nonfatal and typed. It must not invoke `nvidia-smi`,
  parse process output, guess from adapter names, or silently claim a memory
  value.
- Existing explicit settings remain authoritative. Startup defaults and the
  `Use recommended settings` action use the current selected-adapter facts.
- Unsupported backends retain the conservative unknown-device recommendation
  and expose why discovery was unavailable.

The first qualified implementation target is native Linux Vulkan, matching the
supported product workstation. Other backends may add their own typed provider
later without changing settings or renderer ownership.

### One Logical Payload Authority

- `ResidencyOwner` remains the sole owner of decoded GPU payload residency.
- The configured payload capacity is a logical hard maximum. Committed buffer
  bytes are a physical implementation fact and are never substituted for the
  maximum during demand feasibility.
- All existing offsets remain stable when a segment grows. Growth copies the
  committed prefix exactly, replaces the buffer, and rebuilds every bind group
  that references payload segments before later work uses it.
- Growth is checked, capped by each segment's logical maximum, and performed
  as one bounded renderer transaction. A geometric candidate may be reused
  only up to 128 MiB beyond that segment's exact required high-watermark, so a
  small allocation crossing 1 GiB cannot manufacture a nearly 1 GiB physical
  commitment jump.
- Empty segments use only the minimum valid binding allocation. They do not
  eagerly commit their logical maximum.
- Allocation failure first distinguishes current commitment from true logical
  exhaustion. Feasible demand may trigger growth; demand beyond the logical
  maximum retains the existing typed capacity refusal and adaptive-LOD
  behavior.
- Growth does not add a second allocator, cache, residency map, or untracked
  memory pool.
- Automatic shrinking during ordinary interaction is out of scope. Dataset
  retirement or renderer destruction releases committed buffers normally.

### Refresh-Independent Hidden Work

- Vsync remains the visible surface presentation policy. The fix does not
  disable vsync or busy-loop the GUI.
- The hidden scheduler is owned by the existing renderer runtime and uses the
  same device, queue, pipelines, bind groups, private candidate texture, and
  residency lease as coordinated rendering.
- There is one bounded latest-only job. A newer 3D request cancels or replaces
  an older candidate at the next batch boundary.
- At most one hidden batch is outstanding on the GPU queue. Interactive
  preview latency is therefore bounded by one refinement batch, not by an
  entire exact frame.
- Batch height may grow after fast completion and shrink after slow
  completion. It is bounded by nonzero minimum/maximum rows and a fixed hidden
  GPU-time target.
- Candidate identity includes every fact needed to reject stale completion:
  target allocation, texture revision, residency generation, frame,
  requirements/body identity, output extent, and render mode/sampling facts.
- Partial candidate pixels are never exposed, captured as exact, picked from,
  or labelled current.
- Completion is handed back to the coordinator. Only the coordinator validates
  identity, publishes exact presentation facts, and performs the atomic front
  swap.
- Shutdown is bounded and joins the worker. Device loss and worker failure
  become typed renderer failures rather than detached work.

## Architecture

### 1. Native GPU Memory Discovery

The app introduces a native `SelectedAdapterMemoryFacts` provider. The app
constructor reads the adapter already present in `eframe::CreationContext`,
queries it before constructing default settings and the renderer, and stores
the resulting facts for later `Use recommended settings` actions and
diagnostics.

For Linux Vulkan, the provider uses wgpu's typed Vulkan HAL access for that
adapter and Vulkan memory-property queries:

- enumerate memory heaps and sum device-local heaps with checked arithmetic;
- query `VK_EXT_memory_budget` only when the physical device advertises it;
- sum budget and usage entries corresponding to device-local heaps; and
- retain adapter identity beside every result.

The settings policy remains pure. It accepts an optional usable device-local
byte count and applies the existing headroom/cap policy. Discovery and policy
are tested independently.

Startup ordering becomes:

```text
eframe selects adapter
        |
        v
query selected-adapter memory facts
        |
        v
load persisted settings or derive recommended defaults
        |
        v
construct the one renderer with the effective budget
```

No second adapter request participates in normal product settings.

### 2. Growable Payload Segments

Each payload segment owns:

- a logical maximum;
- a currently committed capacity;
- one buffer whose binding allocation is at least wgpu's required minimum;
- one allocator covering only committed usable bytes; and
- growth diagnostics.

The payload bind-group layout declares the minimum valid binding size rather
than the logical segment maximum. The shader continues to receive stable
segment-relative offsets from the page directory.

Before upload planning rejects a body for current committed space, the
residency owner computes the additional committed capacity required under the
logical maxima. It prefers opening an empty segment, which requires no copy,
then grows populated segments toward bounded geometric targets no more than
128 MiB beyond their proven placement high-watermarks. A populated-segment
replacement:

1. creates the larger buffer;
2. copies the old committed prefix;
3. submits and tracks that bounded copy;
4. extends the existing allocator by the new tail;
5. replaces the segment handle; and
6. rebuilds presentation and pick bind groups from the one current segment
   set.

The transfer itself resumes after growth completion. Existing command buffers
retain their old reference-counted buffer handles, and queue ordering prevents
the replacement from racing earlier GPU use.

### 3. Hidden Refinement Scheduler

The current `AtomicVolumeStripState.next_y` continuation driven by
`ctx.request_repaint()` is replaced by a renderer-internal scheduler.

The coordinator prepares one immutable job snapshot after exact residency is
ready: cloned wgpu handles, the candidate texture views, control/bind group,
pipeline, extent, identity, and initial batch policy. A bounded worker:

1. records the next horizontal batch;
2. submits exactly one command buffer;
3. waits for that submission's completion without blocking the UI thread;
4. records elapsed/GPU timing and selects the next bounded batch height;
5. checks the latest generation/cancellation token; and
6. repeats until complete or stale.

Progress is published through bounded atomics or a latest-only result slot.
The worker wake callback requests one UI repaint for final handoff. Normal
visible frames may inspect progress for diagnostics but are not required to
drive it.

The coordinator owns the final validation and promotion. A stale, cancelled,
or failed job cannot mutate presentation state.

## Hard Cut And Deleted Behavior

Implementation deletes:

- `recommended_for_current_system(None)` from normal native product startup
  and the recommended-settings UI action;
- any implication that the unknown-device 1 GiB recommendation describes the
  selected GPU;
- eager creation of every payload segment at its logical maximum;
- diagnostics that equate logical payload capacity with allocated buffer
  bytes;
- the one-hidden-strip-per-visible-refresh continuation contract;
- `request_repaint()` as the clock for every hidden strip; and
- the fixed one-row trap in which only over-budget observations can change
  future hidden batch size.

There is no compatibility branch retaining either old scheduling or eager
allocation.

## Explicit Non-Goals

- No storage/package-format, bricking, brick-shape, page-directory, shader
  sampling, or pyramid-policy change.
- No contiguous-S2 special representation.
- No promise to use all physical VRAM and no automatic allocation of the
  detected total.
- No runtime memory oversubscription, sparse Vulkan binding, buffer shrinking,
  or cross-process eviction manager.
- No reduced-resolution preview, disabled vsync, uncapped GPU queue, or
  full-frame exact submission during active interaction.
- No claim that Vulkan heap facts are portable to Metal, DX12, browser WebGPU,
  or every unified-memory architecture.

## Work Packages

### M0 — Plan And Evidence Baseline

- Preserve this plan in the active documentation index.
- Record the current startup policy, payload commitment, and hidden-row
  scheduling facts in tests before deleting their implementation.

### M1 — Selected-Adapter Facts

- Add typed app-layer GPU-memory facts and Linux Vulkan discovery.
- Reorder app startup so selected-adapter facts precede default settings and
  renderer construction.
- Reuse the stored facts for the recommended-settings action and diagnostics.
- Keep pure recommendation-policy tests, plus discovery-unavailable and
  adapter-identity tests.

### M2 — Growable Payload Arena

- Separate logical maximum from committed bytes in segment and public
  diagnostics.
- Replace eager segment allocation with minimum initial bindings and bounded
  geometric growth tied to exact per-segment placement high-watermarks.
- Add checked allocator extension, replacement copy, completion tracking, and
  bind-group rebuild.
- Make feasibility use logical maxima and transfer placement use committed
  capacity after growth.
- Cover empty growth, populated growth with byte preservation, maximum
  exhaustion, fragmented placement, and truthful diagnostics.

### M3 — Asynchronous Hidden Refinement

- Introduce the bounded renderer-owned job/result scheduler and wake callback.
- Move atomic exact batches off the visible repaint cadence.
- Add bidirectional batch adaptation, one-outstanding-batch backpressure,
  latest-only cancellation, stale-result suppression, device-loss handling,
  and joined shutdown.
- Preserve retained preview, private candidate, exact capture, and one atomic
  coordinator promotion.

### M4 — Product Diagnostics And Documentation

- Report selected-adapter memory source and values, effective renderer budget,
  logical/committed/resident payload bytes, growth count/copy bytes, hidden
  batches/rows, and cancellation/completion facts.
- Name screen work as rows/batches, never dataset tiles.
- Update current state, architecture, testing, and development commands from
  implemented facts only.

### M5 — Verification

- Run focused unit and contract tests for settings, discovery fallback,
  allocator growth, scheduler adaptation, cancellation, and currentness.
- Run trusted Vulkan tests for populated buffer growth and hidden exact
  promotion.
- Run dependency-policy and documentation checks.
- Run the repository's proportionate trusted/broad gate required by the
  touched crates.
- Exercise the normal release product with the representative Cell dataset at
  the supported 1920x1080 workstation boundary:
  - confirm the selected RTX adapter and truthful memory facts;
  - confirm startup does not commit the logical multi-gigabyte payload cap;
  - exercise voxel-exact and smooth-linear MIP, DVR, and ISO;
  - interrupt hidden work with camera changes;
  - verify linked-only changes preserve 3D progress;
  - verify preview remains smooth and exact promotion occurs once; and
  - measure settlement duration and visible interaction latency.

The quarantined linked-S0 host-stress workflow is not run.

## Acceptance

Implementation is complete when:

- normal native startup derives recommendations from the exact selected
  adapter when supported and labels fallback honestly when not;
- explicit persisted GPU settings remain explicit overrides;
- logical payload capacity no longer implies an eager approximately 3 GiB
  startup allocation;
- payload buffers grow without changing resident bytes, offsets, or rendered
  output and never exceed their logical maximum;
- capacity diagnostics separately report maximum, committed, resident, and
  available/placeable facts;
- hidden exact work advances while the visible UI is idle and does not require
  one repaint or surface presentation per row/batch;
- one-row smooth-linear refinement can expand after fast observed work instead
  of remaining permanently fixed;
- at most one bounded hidden GPU batch delays newer interactive work;
- stale candidates never promote and shutdown leaves no detached worker;
- visible preview/currentness/capture semantics remain correct;
- focused, dependency, documentation, trusted Vulkan, and broad checks pass;
  and
- the normal mapped product exercise reports actual behavior separately from
  automated evidence.

Performance success is measured against the eliminated scheduling floor and
the supported workstation guideline. It is not inferred solely from unit
tests, submission counts, or the existence of a GPU timing query.

## Closeout Evidence

Implemented and verified on 2026-07-30.

Selected-adapter discovery was exercised directly with:

```bash
cargo test -p mirante4d-app \
  selected_vulkan_adapter_reports_typed_device_local_memory \
  -- --ignored --nocapture --test-threads=1
```

The exact selected adapter was the NVIDIA GeForce RTX 3070 Ti Laptop GPU on
Vulkan. The provider reported a dedicated 8,589,934,592-byte device-local heap
through `VK_EXT_memory_budget`; the driver budget is intentionally a live
fact and varied between approximately 7.03 and 7.25 GB across the verification
runs. No second adapter or subprocess participated.

The final isolated-settings `target_fixture_render_modes` product run passed
all 141 commands. It derived a 3,590,914,048-byte renderer budget from the
selected adapter and exposed a 2,652,291,072-byte logical payload maximum while
allocating only 67,108,868 physical bytes, committing 67,108,864 usable bytes,
and holding 1,242,256 resident bytes. This proves that the multi-gigabyte
logical recommendation is not an eager multi-gigabyte physical allocation.

The real growth/preservation test passed:

```bash
cargo test -p mirante4d-render-wgpu \
  segmented_payload_upload_and_sampling_cross_the_first_binding \
  -- --ignored --nocapture --test-threads=1
```

It opens an empty segment without copying, grows an occupied segment with a
GPU prefix copy, reuses the original resident pixels afterward, and keeps
physical commitment below the logical maximum. Focused tests additionally
prove exact allocator rollback, per-segment placement targets, and the
128 MiB headroom cap. The final representative Cell run used the persisted
explicit 1 GiB override and ended with a 764,411,904-byte logical payload
maximum, 542,643,228 bytes committed for 408,425,500 resident bytes, two
growth transactions, and 362,562,624 copied preservation bytes. The committed
minus resident gap is within the configured 128 MiB growth headroom rather
than an old full-arena or doubling jump.

Refresh-independent refinement was exercised with:

```bash
cargo test -p mirante4d-render-wgpu \
  coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame \
  -- --ignored --nocapture --test-threads=1
```

The test starts one renderer worker job, lets it progress without repeated
application render turns, keeps the completed result private until explicit
publication authorization, compares the result with direct MIP, DVR, ISO, and
Mixed output, and covers replacement/cancellation and capture currentness.
The final representative Cell product run passed all 60 commands in 16.69
seconds: 289 renderer frames, zero validation errors, 5 hidden jobs started
and completed, zero cancelled or failed, 688 GPU-completion-paced batches, and
3,600 exact rows. The worker accumulated 8.11 seconds of hidden GPU work while
visible presentation remained vsynced.

The product gate also exposed a validation-harness defect: an asynchronous
capture superseded by a newer frame was being treated as a fatal backend
failure. Superseded readbacks are now discarded rather than accepted as
evidence or reported as renderer faults; the scenario still waits for a
matching current capture before it can pass.

The focused non-ignored library gate passed 284 app tests (8 ignored), 69
renderer tests (17 ignored), and 36 UI tests. The final
`cargo xtask verify-pr` gate passed formatting, generated-selector discovery,
fixtures, architecture, documentation, dependency and workflow policy,
zero-warning Clippy, and all 1,292 ordinary unit/contract/UI cases. Its local
report is intentionally non-qualifying only because this implementation
remains in the owner's existing dirty worktree. The new native memory test is
registered in the existing trusted-GPU lane. The quarantined linked-S0
host-stress workflow was not run.
