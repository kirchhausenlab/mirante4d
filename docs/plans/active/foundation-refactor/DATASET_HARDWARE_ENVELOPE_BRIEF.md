# Foundation Refactor — Dataset And Hardware Envelope Brief

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-10
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: dataset profiles, claim ladder, hardware classes, resource ledgers, and measurable acceptance seeds

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

## Approved Dataset And Hardware Envelope

Status: OWNER-APPROVED TARGET POLICY under OD-016/OD-017. D-004, D-005, D-006,
and D-015 are resolved. This section defines the implementation target, not a
claim that the current application or existing baselines already satisfy it.
Performance qualification still requires the named clean evidence.

The `T0` through `T5` tiers defined in the
[verification brief](VERIFICATION_EVIDENCE_BRIEF.md) classify evidence,
rights, and hosting. The `DS-*` profiles below classify product workload. They
are intentionally different taxonomies: a tiny restricted fixture and a tiny
public fixture can share a dataset profile while belonging to different
evidence tiers.

### Support Claim Ladder

Every dataset claim must name one of these levels:

1. **Representable**: the schema can describe the dimensions and values. This
   says nothing about acceptable open time, memory, or interaction.
2. **Functionally supported**: strict validation and open succeed, or invalid
   input receives the required typed rejection; scientific values, transforms,
   and state remain correct through the named workflows.
3. **Usable**: the normal product stays responsive, cancellable, progressive,
   and inside the named resource ledgers. Missing occupied data is never shown
   as empty, and the UI reports incomplete or budget-limited state truthfully.
4. **Performance-qualified**: a named dataset profile, hardware class,
   viewport, build, budget configuration, and clean revision pass explicit
   repeated thresholds. Qualification never transfers silently to another
   profile or machine.

Public documentation must use the strongest level actually proved, not the
strongest level the format could theoretically represent.

### Approved Dataset Profile Union

Logical size means uncompressed source-scale (`s0`) values across the stated
timepoints and channels. It is the stable planning measure; compressed package
size varies with the data and is not a substitute for workload complexity.

| Profile | Boundary case | Foundation acceptance role | Current evidence state |
| --- | --- | --- | --- |
| `DS-0 portable-conformance` | Rights-cleared deterministic cases, each at most `64 MiB` logical `s0`; 2D (`z=1`) and 3D; `uint8`, `uint16`, and finite `float32`; 1/2/4 channels and 1/3 timepoints | Every-commit independent format/import/scientific facts plus generated component cases | Partial: existing tiny fixtures cover the three dtypes, anisotropy, 2 channels x 3 timepoints, and 4 channels separately; independent public goldens and several failure cases are missing |
| `DS-1 ordinary` | Two separately required cases: `4t x 119 x 383 x 518`, 1-channel `uint16` (about `180 MiB` logical `s0`), and `1t x 600 x 1148 x 998`, 1-channel `uint8` (about `656 MiB`) | Representative import, exact open/render/analysis, bounded-runtime, and packaged-product workflows | Diagnostic single-machine baselines exist but are stale and are not credible closure gates |
| `DS-2 combined` | Deterministic scheduled stress case: `8t x 4c x 256 x 256 x 256`, with one dtype per case and `float32` as the widest case (`2 GiB` logical `s0`) | Prove channel/time scheduling, hidden-channel exclusion, mixed display, playback, and resource accounting together without combining the extreme profiles | New target; current deterministic evidence reaches only 2 channels x 3 timepoints and 4 channels separately at `8^3` |
| `DS-3 spatial-extreme` | Opaque restricted reference profile: `1t x 1c`, `uint8`, `2563 x 2240 x 4183`, seven scales, about `22.366 GiB` logical `s0` | Progressive large-volume open, 2D/3D navigation, MIP/DVR/ISO, truthful LOD/capacity state, cancellation, and bounded RSS/VRAM | Useful private product diagnostics exist; clean revision-bound qualification does not |
| `DS-4 temporal-extreme` | Opaque restricted reference profile: `365t x 1c`, `float32`, `74 x 608 x 600` per timepoint, four scales, about `36.706 GiB` logical `s0` across time | Progressive open, timepoint switching, playback/prefetch, exact analysis, cancellation, and bounded RSS/VRAM | Useful private product diagnostics exist; clean revision-bound qualification does not |

Checked count, overflow, and lazy-planning tests use small inputs and explicit
arithmetic bounds. The foundation does not advertise simulated TiB datasets or
materialize large packages merely to test scaling.

The supported envelope is a **union of named profiles, not a Cartesian product
of maxima**. In particular, `365` timepoints, four channels, `float32`, and the
`DS-3` spatial dimensions do not combine into an implied supported dataset.
Any new combination must receive its own named profile and acceptance evidence.

Real packages beyond the named profiles remain an architectural expectation,
not a foundation-release performance claim. The architecture must reject
full-dataset materialization and scale total metadata lazily, but that alone
does not qualify hundreds-of-gigabytes or terabyte-scale real data.

### Approved Value, Layout, And Import Boundary

- Dense intensity values are lossless `uint8`, `uint16`, or finite `float32`.
  Channels remain independent logical app layers over `t,z,y,x`. Under
  resolved D-007, co-registered equal-dtype channels share physical
  `t,c,z,y,x` OME arrays with `c=1` chunks/shards; heterogeneous dtype/grid
  images use separate image groups. Segmentation/label data is absent after
  WP-02 and is not part of any foundation profile.
- The foundation importer accepts reviewed grayscale TIFF/OME-TIFF input as a
  single stack, a directory with one stack per channel/timepoint, or the
  explicitly documented plane-series layout. Shapes and dtypes must agree
  where the selected layout requires it.
- Proprietary CZI/ND2/LIF input, arbitrary OME-Zarr ingestion, remote/object
  stores, mixed-dtype source groups, and lossy conversion remain outside the
  foundation envelope unless a later approved decision expands it.
- The storage design continues to target small independently cancellable
  logical bricks and bounded shards. D-007 and D-008 fix the approved target
  native/OME-NGFF boundary and lifecycle; this envelope must not accidentally
  treat that target as implemented or stabilize the current `mirante4d-v1`
  byte layout.
- There is no single fixed package-byte maximum. Open and import preflight use
  measured package/source facts, declared resource budgets, and free-space
  formulas. A new import requires at least `1.2 * estimated new-package bytes`;
  replacement additionally requires the existing-package backup bytes.
- Bootstrap metadata has a provisional hard target of at most `4 MiB`; index
  pages are at most `1 MiB`; and metadata/index working state before the first
  useful frame is at most `64 MiB`. Large indexes are external, lazy, and
  byte-budgeted. Full validation may scan all payloads only as a cancellable
  background operation.

### Focused Pathology Coverage

A small set of fixtures collectively covers valid zero and sparse data, the
supported dtypes and source layouts, anisotropy and non-divisible edges,
validity, representative malformed grouping/metadata/corruption,
cancellation/restart, and insufficient space. No Cartesian product is
required. Rendering, playback, GPU upload, and scheduler-pressure cases belong
to their owning packages.

Valid zero is scientific data. Only explicit validated validity metadata may
classify a brick as having no renderable samples.

### Approved Hardware Classes

| Class | Proposed contract | Qualification status |
| --- | --- | --- |
| `HW-0 CPU-verification` | Hosted/local CPU suitable for static, unit, property, format, importer, and CPU-oracle tests | Not a supported interactive viewer machine and not a performance comparator |
| `HW-1 minimum-candidate` | Linux x86_64; at least 4 physical/8 logical CPU threads; `16 GiB` installed RAM; local SSD; mapped `1280x720` display; non-CPU Vulkan adapter with at least `4 GiB` dedicated VRAM and renderer limits of at least `256 MiB` max buffer, `256 MiB` storage-buffer binding, and 8 storage buffers per shader stage | Design target only. It must be bound to and pass on an exact weaker machine before it becomes a public minimum; integrated/unified-memory GPUs remain unclaimed |
| `HW-2 bootstrap-reference` | One externally resolved Linux x86_64/Vulkan machine with a discrete GPU, local SSD, a `1280x720` gate viewport, and a `1920x1080` product exercise; the exact sanitized manifest is recaptured for each qualification epoch | Initial reference class only. It does not establish a public minimum; no hardware purchase is required for the foundation |
| `HW-3 capacity-candidate` | At least 8 cores/16 threads, `64 GiB` RAM, `16 GiB` discrete VRAM, NVMe, and mapped `1920x1080` display | Optional future import/soak/capacity characterization, not a minimum, release blocker, or current support claim until an exact machine exists |

Linux x86_64/Vulkan is the sole foundation release/product claim.
Hosted macOS/Windows and other Linux machines remain portability lanes;
Windows, Apple Silicon/Metal, AMD/Intel GPU equivalence, integrated GPUs, and
unified-memory policy require real platform hardware before support is claimed.
NVIDIA and CUDA are never architectural requirements.

Resolved D-006 requires a qualifying hardware GPU for the
interactive product, show an explicit pre-viewer unsupported-adapter diagnostic
when none exists, and keep CPU rendering only for reference, tests, diagnostics,
or export. It must not be a silent degraded product path. The current
contradictory fallback code is a deletion/cutover target, not authority to
reinterpret this decision.

### Approved Seed Resource Policy

The current independent cache and queue budgets do not bound total RSS or VRAM,
and the GPU cache is partly derived from system RAM. The foundation target is
one byte authority per resource domain:

- CPU dataset ledger: `clamp(40% of detected RAM, 2 GiB, 32 GiB)`; use `4 GiB`
  when RAM is unknown. Decoded residency receives at most 50%, upload staging
  at most 12.5%, in-flight decode at most 12.5%, and metadata/indexes/queues/
  results/prefetch retain at least 25%. Delete the separate whole-volume cache.
- Dedicated-GPU ledger:
  `min(8 GiB, 50% of VRAM, max(0, VRAM - 2 GiB))`; use a
  conservative `1 GiB` until capacity is known or explicitly configured.
  Payload/atlas residency receives at most 75%, transfer staging at most 10%,
  and at least 15% remains inside the ledger for display targets, page tables,
  and scratch. Allocation is lazy, and reported resident bytes mean allocated
  or committed bytes rather than configured capacity.
- Unified memory, once a real supported platform exists, must be charged to
  both ledgers under a separately measured cap; it cannot be double-promised.
- Every large allocation, task, queue entry, decoded result, upload, analysis
  artifact, and import buffer must acquire a byte/count lease. Each ledger and
  category must remain within its cap plus the existing `10%` accounting
  tolerance, while `HW-1`/`HW-2` acceptance scenarios target process RSS no
  greater than `8 GiB`.

These formulas are approved seed policy, not proof that unmeasured constants
are correct. WP-08/WP-09 must calibrate them against explicit failure behavior
and preserve one architecture across hardware classes. A material formula or
support-boundary change reopens D-005 rather than landing silently.

### Approved Measurable Acceptance Seeds

- First useful frame means a nonblank, valid current partial/coarse or exact
  frame in the mapped normal product window with truthful completeness/LOD
  state. Targets on `HW-2`: `DS-0 <= 2 s`; `DS-1` and `DS-2 <= 5 s`; `DS-3`
  and `DS-4 <= 15 s`. A future `HW-1` qualification may use `10 s` and `30 s`
  respectively but cannot publish those limits until measured.
- Normal command acknowledgement p95 is at most `100 ms`; active progress is
  refreshed at least every `250 ms`; cancellation acknowledgement is at most
  `500 ms`; current input/update work is bounded and never waits on I/O,
  decode, or GPU completion.
- Warm 2D/3D input-to-current-partial p95 on `HW-2` at `1280x720` is at most
  `250 ms`; cold `DS-4` timepoint-to-current-partial p95 is at most `2 s`;
  all-panel visible-work planning is at most `16 ms`; a valid prior frame is
  retained while replacement data is incomplete.
- A scenario may be called interactive only when measured presented-frame
  interval p95 is at most `50 ms` (`20 FPS`). `30 FPS` is preferred and
  `60 FPS` is a stretch target. Resident small/medium playback remains at
  least `10 FPS`. None of these thresholds authorizes hidden LOD, sampling,
  mode, channel, or correctness reduction.
- Import gates measure peak RSS, read/write throughput, output bytes,
  deterministic identity, cancellation/restart, source immutability, and the
  free-space formula. Throughput is reported before it becomes a hard target.
- Relative slowdown policy (`>10%` explain, `>20%` block) activates only after
  clean current baselines exist for the same dataset/hardware/scenario and
  metric. Shared hosted runners never provide performance gates.

The current committed performance corpus is diagnostic only: all ten baselines
come from `HW-2`, predate the current revision, and the testing audit rejected
them as closure-quality gates. Their three historically named `high-pixel`
files contain no current 3840-wide or 4K scenario; the largest recorded
viewport is `1920x1080`. Under OD-016, no replacement 4K run belongs in the
foundation program.

### Envelope Resolution

The owner approved this envelope on 2026-07-09 through OD-016/OD-017. That
resolves the product policy, not the measurements. `HW-1` remains deliberately
unadvertised until an exact weaker machine qualifies, and no dataset or
performance level may be claimed from representability or old diagnostics.
The implementation handoff must still bind opaque dataset IDs/digests, the
exact reference-machine manifest and calibration procedure, clean commands,
sample counts, cold/warm rules, and evidence freshness before thresholds become
closure gates.

### HW-2 Promotion Binding And Calibration Contract

The promotion binding uses opaque public ID `HW-2` and records a sanitized
manifest digest. It excludes hostname, username, serial numbers, absolute home
paths, monitor identifiers, and storage device model. A private resolver may
retain those operational facts when needed. Public evidence records only the
scientifically relevant OS/kernel/architecture, CPU topology, visible memory,
Vulkan adapter/limits/features, driver, dedicated VRAM, filesystem semantics,
one selected display mode/scaling, power profile, and tool/executable
identities. Those values are recaptured for the exact qualification run rather
than frozen as workstation metadata in source.

Before a performance or product claim uses `HW-2`, its owning entry brief must:

1. recapture the sanitized manifest and prove its digest;
2. use AC power, record the CPU/GPU power/performance profile and temperatures,
   and stop on thermal/power throttling or unrelated GPU load;
3. select one mapped `1280x720` gate or `1920x1080` product viewport and record
   physical mode, logical scaling, refresh, compositor/session, and other
   connected-output state;
4. record free memory/storage, filesystem/mount options, background-load
   bounds, executable/package digest, dataset identity, and clean worktree;
5. define cold as a new process with application caches/scratch absent and OS
   page-cache state recorded, never silently claiming a privileged global cache
   drop; define warm as the named repeated operation after the exact warm-up;
6. follow the verification brief's sample-count, median/p95/confidence,
   absolute/relative threshold, zero-retry, and outlier rules; and
7. invalidate comparability after any material OS/kernel/driver/GPU/display/
   power/filesystem/tool/dataset/scenario change until recalibrated.
