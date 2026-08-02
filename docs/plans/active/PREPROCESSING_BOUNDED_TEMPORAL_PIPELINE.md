# Preprocessing Bounded Temporal Pipeline Plan

- Status: COMPLETE; AUTOMATED-VERIFIED; OWNER PRODUCT-VALIDATED
- Planning requested by owner: 2026-08-01
- Target approved by owner: 2026-08-01
- Implementation authorization: GRANTED 2026-08-01
- Implemented: 2026-08-01
- Owner product validation accepted: 2026-08-01
- Last reviewed: 2026-08-01

This is the implementation handoff for improving long TIFF preprocessing by
overlapping independent temporal work without reopening the completed
large-dataset storage architecture. It is deliberately a small pipeline, not a
general workflow engine: one canonical unit remains the sole production,
scientific-identity, spool, shard, journal, and publication authority, while a
strictly bounded number of later units may decode into their existing
unit-local caches ahead of it.

The plan retains every currently working scientific, source-safety, capacity,
resume, and publication contract. It changes scheduling only. Package bytes,
package identity, pyramid values, validity, TIFF ordering, no-data behavior,
and the normal serial execution result must be identical.

## Implemented Result

The shipped scheduler has exactly one canonical current unit and at most one
future decoded cache: maximum temporal pipeline width two. The future lane has
no spool, shard writer, scientific hasher, journal authority, or publication
authority. It starts only after automatic no-data bootstrap is durable,
borrows only surplus CPU bytes and disk headroom, and is discarded or joined
on every cancellation and failure path. The canonical owner consumes results
strictly by unit ordinal. There is no production concurrency setting and no
second decode-ahead lane.

The release qualification used a generated public workload of twenty
timepoints, each a Deflate-compressed multipage TIFF with logical shape
`[32, 257, 257]`, one `uint8` channel, on an AMD Ryzen 7 6800H (8 cores/16
threads), 14 GiB RAM, Linux 7.0, and an ext4 NVMe source/destination
filesystem. Three fresh width-one runs took
`[5.776860602, 5.822835258, 5.864333981]` seconds; three one-ahead runs took
`[4.504497134, 4.635602870, 4.711963488]` seconds. Median primary wall time
improved by 20.39 percent. Baseline source ingest and canonical production
accounted for at least 24.49 and 22.39 percent respectively, so the fixture
exercised both sides of the overlap. Byte-identical package IDs and scientific
content IDs were required for every pair.

The three-run single-unit control changed from a 342.777 ms median to
324.740 ms, a 5.26 percent improvement rather than a regression. Because one
decode-ahead lane exceeded the required 15-percent throughput gain and the
single-unit gate passed, the evidence rule stopped the design there; a second
lane was neither implemented nor retained.

Independent automated coverage also verifies forced-serial/pipelined package
and scientific identity equality, exact source-decode counts, no-data
bootstrap ordering, strict consumption order, exact concurrent CPU-ledger
peak, optional-disk admission, current-progress byte protection,
cancellation/resume, source replacement during speculative decode, bounded
descriptor/scratch accounting, and application/UI projection. A normal
owner-driven workflow is not inferred from that automated evidence. On
2026-08-01 the owner separately reported that the completed preprocessing
workflows all worked correctly and explicitly accepted the result as
validated. No private dataset facts or universal throughput claim follow from
that attestation.

## Decision Summary

The accepted direction is:

1. finish automatic no-data bootstrap for channel zero/timepoint zero before
   allowing any speculative temporal work;
2. keep exactly one canonical production/commit owner and exactly one active
   unit spool;
3. decode at most one later `(timepoint, channel)` unit concurrently at first;
4. allow a second decode-ahead lane only if the measured one-lane result is
   still ingest-bound and the second lane passes the same correctness,
   resource, and regression gates;
5. admit decode-ahead work only from surplus CPU, RAM, descriptor, and disk
   capacity after the current unit's guaranteed progress path is protected;
6. consume decoded units and publish output strictly in canonical unit order;
   and
7. retain the current one-unit schedule automatically whenever overlap cannot
   be admitted safely.

The evaluation ceiling was one current production unit plus two decode-ahead
caches. The measured shipped design stops at one current unit plus one
decode-ahead cache. It is not proportional to timepoint count, channel count,
core count, or free memory. There is no user-facing concurrency selector and
no automatic throughput-tuning subsystem.

## Why This Work Is Justified

The current importer already parallelizes scientific-tile preparation, base
production, each pyramid scale, inner encoding, and eligible single-plane TIFF
files through byte-accounted ordered workers. It uses at most 16 CPU workers,
and one owner commits their results deterministically.

The remaining temporal loop is nevertheless fully sequential. One unit must
finish source ingest, scientific identity, base production, every pyramid
scale, packed-record persistence, shard construction, durability, and unit
journaling before source ingest begins for the next unit. A multipage 3D TIFF
also retains one streaming decoder within its unit. The resulting execution
alternates between relatively serial source/cache I/O and highly parallel CPU
production instead of overlapping them.

A short read-only observation of the owner's active normal import on the
16-logical-CPU workstation showed that alternation directly. Several samples
used approximately 1.2 CPU cores while reading about 95 MB/s and writing about
139 MB/s; a later sample used about 13 CPU cores with almost no I/O. The NVMe
reported roughly 7--8 percent utilization and negligible wait during the I/O
samples. This five-second observation is diagnostic motivation, not a
performance qualification or a promise of a particular speedup.

The correct conclusion is not "raise the worker cap." The existing CPU phases
already use most cores when they have work. The missing boundary is bounded
overlap between independent temporal units.

## Outcome

After this cutover:

- while unit `N` performs scientific hashing, base/pyramid production, and
  final-layout commit, one admitted worker may decode unit `N+1` into its
  ordinary canonical cache;
- if evidence justifies the second lane, unit `N+2` may decode independently
  under the same fixed bound;
- when `N` commits, the canonical owner consumes the already prepared cache
  for `N+1` without decoding its TIFF again;
- only the canonical current unit may own a spool, append final-layout shards,
  advance scientific identity, or append the unit journal;
- current-unit progress cannot be blocked by memory or disk space borrowed by
  speculative work;
- cancellation, process loss, source change, or capacity loss leaves the same
  valid committed prefix as the current importer;
- a complete uninterrupted run still decodes every source-native payload once
  and produces every logical payload once; and
- small, single-unit, memory-constrained, or disk-constrained imports retain
  the present execution path and performance characteristics.

## Non-Negotiable Invariants

### Scientific and source behavior

- The explicit reviewed channel manifest remains the only authority for
  channel, timepoint, file, and Z order.
- Source TIFF files remain read-only and generation checked before, during,
  and after their existing bounded decode route.
- Automatic no-data detection still reads only channel zero/timepoint zero,
  persists one resolved policy, and completes before any later cache is
  admitted.
- Dtype, calibration, transforms, pyramid geometry, reduction arithmetic,
  validity, hidden-plane semantics, channel labels, and canonical invalid
  values do not change.
- Scientific layer hashers advance only on the canonical owner and only in
  their existing tile order. The plan does not introduce combinable hashes,
  per-unit substitute identities, or out-of-order identity commits.
- Package metadata, encoded profile, exact package digest, and scientific
  content identity are byte-for-byte independent of pipeline width and task
  completion order.

### Ownership and ordering

- One coordinator owns the next canonical unit ordinal.
- One current unit owns scientific hashing, the encoded-inner spool, packed
  record writes, outer-shard construction, and unit completion.
- One existing `ResumableLocalPackageStage` remains the final-layout and
  publication authority. No background decoder may access it.
- Prepared caches may finish out of order, but the coordinator may consume
  only the exact next ordinal. Later readiness never skips a slow predecessor.
- The current stage-journal/unit-journal relationship remains unchanged; the
  stage may never escape the existing bounded recoverable prefix.
- The worker result queue contains only the fixed decode-ahead window and
  carries unit-local results, not payload copies proportional to the dataset.

### Bounded resources and forward progress

- At most one current cache, one current spool, and two future caches may be
  live. The first implementation admits only one future cache until measured
  evidence passes the second-lane decision gate.
- Total importer threads never exceed the process parallelism policy. Each
  active ingest lane removes one slot from the current ordered-worker CPU
  allowance rather than oversubscribing the machine.
- Decoder buffers and worker results retain their exact CPU-ledger charges.
  The current unit's resident state and minimum complete transform/commit path
  are protected before any decode-ahead lease can borrow surplus capacity.
- Optional overlap is never a start requirement. Failure to admit speculative
  work narrows scheduling to one unit; it does not fail preprocessing.
- Decode-ahead disk admission includes the exact missing canonical-cache
  ceiling in addition to the current unit's mandatory immediate headroom.
  Future package output remains absent from that comparison.
- A speculative cache may not consume the finalization reserve or space
  required to finish the current unit. If capacity contracts, the coordinator
  stops new prefetch and may remove only its own unconsumed speculative cache
  before reporting a genuine capacity pause.
- File descriptors, cache objects, queues, and progress messages have fixed
  bounds derived from the two-lane maximum. No inventory or allocation scales
  with total `T` or `C`.

### Cancellation, failure, and resume

- One cancellation token stops the canonical owner and every decode-ahead
  worker. All workers are joined before the import worker returns.
- A background source change or decode error is delivered with its exact unit
  identity, cancels later speculative work, and cannot mutate the package
  stage.
- A completed or partial future canonical cache is scratch, not committed
  scientific or package authority. Its existing plan/source/unit binding is
  validated before reuse and it may always be discarded safely.
- Resume considers only the fixed next decode-ahead ordinals; it never scans
  all possible timepoints or trusts an unbound directory. Valid partial caches
  continue through the existing per-plane checkpoint path.
- A corrupt cache, spool, stage, or journal remains a typed invalid checkpoint.
  Scheduling overlap does not turn corruption into a retry or repair path.
- Create-only publication, staged structural/exact/scientific validation,
  atomic rename, and destination-parent durability remain unchanged.

## Target Architecture

### 1. Preserve the current unit processor

First extract the body of the current temporal loop into one explicit
canonical `process_unit` operation. That operation receives a validated
canonical cache and performs the existing work in the existing order:

```text
scientific identity
  -> base production and inner encoding
  -> each geometry-defined pyramid level
  -> packed-record persistence
  -> outer-shard append and durability
  -> unit-journal append
  -> cache/spool cleanup
```

This extraction must pass package-identity, counter, cancellation, resume, and
publication tests with forced pipeline width one before overlap is introduced.
It must not rewrite the numerical kernels, spool format, stage writer, or
scientific hasher.

### 2. Add one small decode-ahead coordinator

The import owner keeps a fixed in-memory queue keyed by canonical unit ordinal.
Each slot has only the states needed by this pipeline:

```text
not admitted -> ingesting -> cache ready -> consumed
                              \-> failed
```

This is not persisted as a new workflow database. Existing unit cache files
remain the only durable scratch, and the existing unit/stage journals remain
the only completed-prefix authorities.

After no-data policy resolution, the coordinator:

1. protects resources needed to process the current unit;
2. admits the next source ordinal when surplus capacity and disk headroom fit;
3. lets a scoped worker run the existing `decode_unit_if_needed` route into
   that unit's deterministic cache directory;
4. continues processing the current unit with the existing ordered workers;
5. drains bounded progress and failures while current workers run; and
6. consumes the exact next ready cache when the current unit commits.

The first product policy has one ingest lane and one future slot. No second
lane is enabled merely because the machine has unused cores.

### 3. Make a second ingest lane evidence-gated

After the one-lane pipeline is correct and measured, a second independent
multipage TIFF decoder may be enabled only when all of these are true:

- source ingest remains the largest steady-state bottleneck;
- one-lane production still leaves material CPU and destination-I/O capacity;
- the CPU broker can protect both decoder charges plus current-unit progress;
- immediate filesystem headroom covers both missing future caches without
  touching mandatory current-unit or finalization space;
- total thread and descriptor bounds remain inside their existing process
  authorities; and
- the second lane improves the representative long workload while staying
  inside every regression and resource gate below.

The production constant is at most two ingest lanes. There is no feedback
controller, moving autotuner, hardware benchmark at startup, or arbitrary
worker-count heuristic. If the second lane does not earn its complexity, it is
not shipped and the final documented maximum remains one.

### 4. Share CPU capacity without oversubscription

The coordinator derives one run-local CPU slot count from the same system
parallelism authority used today. Active source-ingest lanes reserve one slot
each. The existing ordered-worker policy receives the remaining slot ceiling
in addition to its byte limit and continues to choose the smaller CPU- or
memory-admitted count.

This is a small extension of the current ordered-worker policy, not a global
thread-pool replacement. Worker creation/reuse, Rayon adoption, NUMA policy,
thread affinity, and operating-system priority changes are outside this plan.

### 5. Keep optional cache headroom separate from hard admission

The placement plan adds an exact bound for one additional canonical unit cache
and its fixed control bytes. Before starting a speculative cache, runtime
checks:

```text
available space
  >= mandatory additional headroom for the current unit
   + missing bytes for the proposed future cache
```

For two lanes, each proposed cache is admitted incrementally using the same
rule. Existing bytes in a resumable future cache are deducted exactly. No
future spool or package output is reserved because speculative units do not
own spools or final-layout objects.

The setup review continues to show the serial minimum required headroom. The
optional pipeline width is a runtime throughput fact, not a new reason to
reject Start. A real capacity pause is reported only when the current serial
progress path cannot fit after owned speculative scratch has been reclaimed.

### 6. Keep presentation and evidence honest

The existing primary stage remains the canonical unit's user-visible stage.
Concurrent source preparation is projected separately as bounded storage/work
status, for example:

- `preparing next volume 14/150`;
- `1 volume prepared ahead`; and
- `pipeline width 2 (1 current + 1 preparing)`.

The UI does not oscillate the primary stage between two concurrently active
units and does not fabricate a package-wide fraction or ETA.

Successful receipts add only the diagnostics needed to evaluate this cut:

- maximum current-plus-prefetch width;
- admitted and consumed prefetched units;
- cache hits from durable decode-ahead work;
- aggregate ingest busy time;
- wall time during which ingest and canonical processing overlapped; and
- time spent unable to admit optional prefetch because of CPU bytes, disk
  headroom, or queue capacity.

Those counters are observations, not correctness authorities. End-to-end
primary wall time remains the performance outcome. Concurrent lane timings may
overlap and therefore must not be summed as if they were a serial duration.

## Implementation Sequence

### P0: Freeze the serial baseline

- Record a release baseline using the current width-one implementation on one
  generated multi-timepoint, multipage-TIFF workload.
- Require the fixture to spend material time in both source ingest and CPU
  production; a workload dominated by only one trivial phase cannot validate
  overlap.
- Retain exact package identity, source-read/decode counts, stage timing,
  working bytes, checkpoint bytes, descriptor peak, and durability facts.
- Add no qualification framework and do not use a private dataset as committed
  evidence.

### P1: Isolate canonical unit processing without changing behavior

- Extract one-unit processing from the temporal loop.
- Keep width one and preserve the exact call order and owners.
- Prove clean, cancelled, resumed, automatic-no-data, and no-mask outputs are
  unchanged.
- Delete the inlined predecessor loop body after the extraction; do not retain
  two unit-processing paths.

### P2: Add one decode-ahead lane

- Add the fixed ordinal queue and one scoped ingest worker.
- Start overlap only after no-data resolution is durable.
- Reserve current progress before borrowing source-decode capacity.
- Reduce ordered-worker CPU slots while the ingest worker is active.
- Consume ready caches only in canonical order and merge source statistics
  exactly once.
- Preserve or safely discard bound future cache scratch on cancellation and
  resume.

### P3: Integrate disk headroom, errors, and observability

- Add incremental optional-cache headroom checks without changing hard Start.
- Prevent speculative work from causing a capacity pause for work that the
  width-one path could complete.
- Project bounded prefetch facts separately from the primary stage.
- Propagate background source/decode failures through one typed owner route.
- Join all workers on success, cancellation, failure, and application exit.

### P4: Decide whether a second lane is warranted

- Compare width one and one-ahead on the frozen release workload.
- Inspect primary wall time, ingest/production balance, CPU use, I/O wait,
  working bytes, scratch, and descriptor count.
- Enable at most one additional lane only if every evidence gate in the target
  architecture is met.
- If one lane meets the throughput target or a second lane regresses resource
  behavior, stop at one and remove any experimental second-lane branch.

### P5: Close the cutover

- Run focused import, storage, application, UI, cancellation/resume, and
  package-lifecycle checks.
- Run the ordinary public PR gate and the bounded normal-app preprocessing
  scenario.
- Have the owner exercise the normal wizard on a representative long local
  acquisition, observe several steady-state units, cancel/resume if useful,
  and open the result.
- Update current-state and architecture authorities with the actual shipped
  lane maximum and measured evidence. Do not claim the planned maximum if the
  second lane was not retained.

## Verification And Acceptance

### Independent correctness

- Forced width one and the shipped pipeline produce byte-identical packages
  and identical scientific content IDs from the same generated input.
- A deliberately delayed earlier decode and faster later decode prove that
  ready work cannot reorder scientific identity, packed records, shards, or
  journals.
- Automatic no-data tests prove that no later ingest begins before the first
  policy is resolved and persisted.
- Normal uninterrupted execution records exactly one native decode for every
  admitted source payload; consuming a prefetched cache cannot decode again.
- Cancellation at ingest-ready, canonical-processing, shard-commit, and
  journal boundaries resumes to the same package identity.
- Source replacement during speculative decode fails with the existing typed
  source-change result and publishes no partial package.

### Resource and liveness

- Tests independently bound current caches, future caches, spools, result
  slots, workers, descriptors, and scratch for zero, one, and—if retained—two
  ingest lanes.
- Synthetic `T` and `C` growth does not change those bounds.
- Under minimum admitted RAM or disk, the same run continues at width one
  rather than failing because optional overlap was unavailable.
- Withdrawing borrowed CPU capacity stops new prefetch, drains or cancels
  bounded speculative work, and leaves the current progress lane usable.
- Disk-capacity fault injection proves that speculative cache reclamation
  precedes a genuine capacity pause and never removes committed stage bytes.
- Every exit path joins all ingest and ordered workers and leaves no growing
  queue or orphaned thread.

### Performance

Performance evidence is developer-local and product-focused; it is not added
to every public test run.

- Use three fresh release sessions for width one and the shipped pipeline on
  the same public generated multi-timepoint source, filesystem, build, and
  otherwise idle machine.
- The representative workload must have at least ten temporal/channel units
  and baseline source ingest and CPU production must each account for at least
  20 percent of primary wall time.
- The shipped pipeline must improve the three-run median primary wall time by
  at least 15 percent to justify the scheduling complexity.
- A single-unit control must not regress its three-run median by more than five
  percent.
- Package identity, decoded/encoded work counts, source preservation,
  publication, peak managed memory, scratch, descriptors, and typed failures
  must all remain inside their exact contracts.
- Any owner-observed long-dataset claim must state the actual workload,
  hardware, completed-unit interval, elapsed time, pipeline width, and whether
  source and destination shared a filesystem.

If the 15-percent improvement is not reached, implementation does not conceal
that outcome behind CPU-utilization or thread-count numbers. The overlap code
must either be simplified until it earns its cost or removed.

## Risks And Mitigations

- **Nested concurrency oversubscribes the CPU.** Reserve ingest CPU slots and
  pass the remainder to the existing worker policy.
- **Speculative decode steals memory needed for forward progress.** Protect
  the current unit minimum first; prefetch borrows only surplus leases.
- **Extra cache writes cause disk contention.** Keep the queue at one initially,
  bound the maximum at two, measure I/O wait, and reject a second lane that
  does not improve primary wall time.
- **A future cache consumes commit/finalization space.** Admit it only above
  mandatory current headroom and reclaim only owned speculative scratch before
  pausing.
- **Out-of-order completion changes scientific identity.** Keep hashing,
  spooling, stage append, and journals on the single canonical owner.
- **Concurrent progress makes the UI misleading.** Keep one primary stage and
  expose decode-ahead as a separate bounded status fact.
- **Resume grows a new checkpoint model.** Reuse ordinal-bound canonical cache
  scratch; add no new persisted scheduler authority.
- **The change expands into a generic pipeline framework.** Keep one concrete
  coordinator in the import pipeline with a fixed queue and two-lane ceiling.
- **Tests recreate the implementation instead of checking it.** Use delayed
  completion, independent package identity/content, exact resource ceilings,
  and normal-product observation rather than a duplicated scheduler model.

## Explicitly Out Of Scope

- Package-format, shard-layout, chunk-size, pyramid-geometry, validity, or
  compression-level changes.
- Parallel decoding of pages inside one multipage TIFF decoder.
- A general DAG scheduler, work-stealing runtime, persistent workflow engine,
  or new third-party thread-pool dependency.
- More than two decode-ahead lanes or a user-facing worker/memory control.
- Concurrent spools, concurrent outer-shard writers, out-of-order unit commit,
  or a second package-stage authority.
- Fusing scientific hashing with base production, eliminating the canonical
  cache, or streaming one pyramid scale directly into the next.
- Batching durability across units or weakening crash-loss, source-generation,
  staged-validation, exactness, scientific-validation, or atomic-publication
  guarantees.
- Overlapping final package validation with production.
- GPU preprocessing, renderer, playback, analysis, project-store, GUI
  redesign, smooth-linear rendering, or release packaging work.
- A broad benchmark/qualification framework or unattended private-dataset
  automation.

The excluded items may be reconsidered only if the bounded temporal pipeline
passes correctness but measured phase evidence shows that one of them is the
next dominant cost. They are not hidden implementation tasks in this plan.

## Authority And Supersession

The
[large-dataset preprocessing and storage cutover](LARGE_DATASET_PREPROCESSING_AND_STORAGE_CUTOVER.md)
remains authoritative for compositional package admission, per-unit scratch,
incremental headroom, final-layout resume, and capacity pause. This plan
changes its current scheduling statement from "only one unit may be doing any
work" to "only one unit may own canonical production and commit, while a
fixed number of future canonical caches may decode ahead."

The
[preprocessing application and importer cutover](PREPROCESSING_APPLICATION_AND_IMPORTER_CUTOVER.md)
remains authoritative for the explicit source manifest, process CPU broker,
decode-once goal, application shell, and normal preprocessing workflow.

After implementation there must be one scheduling authority. The inlined
fully sequential temporal-loop orchestration is deleted when the bounded
coordinator replaces it; a test-only forced-width policy may remain solely for
identity and performance comparison. No production setting, compatibility
branch, or legacy scheduler survives the cut.

## Completion Standard

The work is complete only when the existing unit processor is preserved behind
one canonical owner; bounded decode-ahead overlaps independent units without
oversubscription; optional overlap cannot steal serial forward progress;
scientific and package output is identical across widths and completion order;
cancellation/resume and source-change handling retain one valid prefix; hard
capacity remains independent of total `T/C`; the shipped lane count earns the
performance threshold without single-unit regression; predecessor scheduling
code is deleted; current authorities record the actual implementation; public
automated checks pass; and the owner validates the normal long-dataset product
workflow.
