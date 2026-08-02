# Large-Dataset Preprocessing And Storage Cutover Plan

- Status: COMPLETE; AUTOMATED-VERIFIED; OWNER PRODUCT-VALIDATED
- Planning requested by owner: 2026-08-01
- Target approved by owner: 2026-08-01
- Implementation authorized by owner: 2026-08-01
- Owner product validation accepted: 2026-08-01
- Last reviewed: 2026-08-01

This is the implementation handoff for making preprocessing scale with the
largest spatial unit being processed instead of multiplying temporary work by
the complete channel/time axis. It also replaces fixture-calibrated aggregate
storage admission with one compositional safety contract.

The change is one hard cutover. Raising a DS ceiling, adding another profile
for a newly observed dataset, weakening free-space preflight, or catching the
current errors and continuing would leave the same defects in place and is not
an implementation of this plan.

The owner subsequently authorized full implementation. The first
implementation completed the temporal-unit payload cutover, but owner product
testing exposed that its disk admission still multiplied every addressed
outer shard by the maximum completely occupied shard size and then required
that whole-package ceiling before Start. That behavior contradicts the
per-temporal-unit product outcome and the per-object/liveness requirements
already recorded below. Its automated closeout is withdrawn for disk
admission. This approved amendment makes incremental headroom the sole hard
capacity gate and keeps whole-package size only as non-reserved guidance.

## Outcome

After this cutover:

1. a dataset whose individual volumes are supported is not rejected merely
   because it has more channels or timepoints than a historical reference
   fixture;
2. storage still rejects malformed, unsafe, unaddressable, or operationally
   unbounded packages through limits derived from real format and execution
   constraints;
3. normal preprocessing decodes each source payload once and produces each
   pyramid payload once through a bounded temporal/slab pipeline;
4. no complete decoded base cache and no second dataset-scale encoded payload
   copy are required;
5. the hidden final-layout stage is also the resumable payload authority, and
   successful publication remains validated, create-only, durable, and
   atomic;
6. working RAM and non-output payload scratch are functions of the largest
   admitted spatial unit and safe concurrency, not `T * C`;
7. the final package still grows with the scientific dataset, as it must, but
   preprocessing never requires free space for hypothetical future encoded
   timepoints before they are produced;
8. hard disk admission proves only the current temporal unit, its durable
   commit, compact incremental control, and bounded finalization reserve;
9. whole-package logical size and a conservative encoded ceiling remain
   informational and are never passed to the free-space rejection boundary;
   and
10. the setup and running UI distinguish informational final-size guidance
    from the immediate additional headroom that is actually required.

## Corrective Storage-Admission Amendment

The temporal-unit execution boundary was present in the original plan and in
the first implementation, but its disk contract was interpreted too narrowly.
Memory, decoded cache, and encoded-inner spool were bounded to one unit while
the Start gate still demanded proof that every future final-layout object
would fit. That retained the practical failure the cutover was intended to
remove.

The first implementation also failed its own tighter object-geometry target.
It computed `number of addressed outer shards * maximum fully occupied outer
shard bytes`. Edge-heavy objects, including very coarse levels with only one
small logical chunk, were charged as if all 64 inner slots contained maximum
float32 payloads. The test oracle repeated that multiplication, and the native
fixture checked only that its inflated requirement happened to fit the local
disk. Those checks established self-consistency, not useful admission.

This amendment makes the intended distinction explicit:

- **persistent output:** actual encoded objects already committed in the
  hidden stage; these consume real disk and remain there;
- **current-unit headroom:** additional bytes sufficient to finish the active
  `(timepoint, channel)` cache/spool, commit every object for that unit, append
  its compact records, and retain bounded finalization capacity;
- **whole-package guidance:** exact logical output bytes and a conservative
  per-object encoded ceiling for explanation only; and
- **capacity pause:** a typed, non-destructive interruption when the next
  durable step genuinely cannot fit, followed by ordinary checkpoint resume
  after capacity becomes available.

No full-package output reservation, fallocate call, placeholder file, or
remaining-worst-case `statvfs` comparison is permitted. Temporal length may
increase actual final bytes and total duration, but it may not multiply the
hard Start requirement for an unchanged spatial unit.

The corrective implementation now follows that contract. The planner sums
each actual outer object's occupied inner slots, exposes the complete package
only as guidance, and derives hard Start/next-unit headroom independently.
The resumable stage retains cumulative durable-payload prefixes by input
ordinal, so a partial unit's committed output is not charged twice. Runtime
checks only unfinished unit growth and then the actual missing finalization
suffix. Safe prepublication `ENOSPC` maps to a typed capacity pause; native UI
recovery offers Resume without deletion, while invalid checkpoints retain the
confirmed Reset-and-Restart path.

## Diagnosed Predecessor Failures

### Aggregate DS envelopes are reference fixtures, not safety proofs

The current `DS-0` through `DS-4` admission classes contain exact aggregate
logical-brick and shard ceilings calibrated from a small set of representative
geometries. Tests deliberately equate those fixture counts with the profile
limits. Increasing `T` or `C` multiplies otherwise valid per-volume bricks and
shards until no class admits the package.

That rejection does not identify a TIFF, decoder, renderer, address-width, or
per-frame spatial limit. It only says that the new dataset has more aggregate
resources than a previous fixture. Adding `DS-5` or enlarging the last observed
constant would repeat the mistake for the next long acquisition.

Temporal and channel extent may legitimately increase final object count,
output bytes, processing duration, and directory fan-out. Those consequences
must be checked directly. They must not be represented as an unexplained
renderer or storage-profile ceiling.

### Free-space preflight reserves three dataset-scale representations

The current free-space equation includes:

```text
full decoded canonical S0 cache
  + 125% of the complete logical pyramid for the encoded checkpoint
  + 125% of the complete logical pyramid for the final package
  + mask spill, journal/index records, shard tails, and fixed overhead
```

This is why a compressed TIFF source can produce a requirement many times
larger than its source size. TIFF compression is only part of the difference:
the source expands to decoded voxels, the pyramid adds levels and validity,
and the importer then budgets two complete encoded copies plus the complete
decoded base.

The check is faithfully reporting the current architecture. It is not proof
that the final `.m4d` package needs that much space, and it cannot be corrected
by comparing the estimate with compressed source bytes or by skipping
preflight.

### The failures share one ownership error

Both paths treat the whole temporal dataset as one indivisible working object:
profile selection admits its total brick inventory against one fixture, and
preprocessing retains whole-dataset intermediate representations before
publication. The correct boundary is a bounded spatial production unit inside
an aggregate, paged package.

## Non-Negotiable Invariants

### Scientific and source correctness

- Source TIFFs remain read-only. Import never repairs, rewrites, renames, or
  reorders them.
- The validated explicit channel manifest remains the sole source-order
  authority.
- Dtype, shape, channel labels, calibration, temporal order, spatial
  transforms, pyramid terminal geometry, validity semantics, no-data policy,
  and canonical invalid values do not change in this cutover.
- Automatic no-data detection still derives one immutable spatial policy from
  channel zero at timepoint zero and applies it dataset-wide without inspecting
  later timepoints for agreement.
- Scientific content identity is accumulated in canonical order over exactly
  the values that are published. Streaming and resume may not make identity
  depend on worker completion order.
- Existing complete packages retain their current experimental-format meaning.
  This plan adds no legacy reader, migration path, or alternate semantic
  interpretation.

### Bounded resources and hostile-input safety

- Every size/count calculation uses checked arithmetic before allocation or
  path construction.
- No catalog, admission, validation, import, resume, or audit operation
  allocates a vector, queue, or task set proportional to an untrusted package's
  total brick or object count.
- Codec input/output, decoded chunk, individual shard, manifest page, path
  length, directory depth/fan-out, scale count, and per-frame spatial work keep
  explicit finite ceilings.
- Every retained ceiling must cite one concrete authority: a serialized field
  width, codec bound, maximum bounded batch, per-frame renderer/storage
  envelope, path/filesystem constraint, or measured product resource policy.
  A number copied from a workload is not a valid authority.
- Aggregate work is streamed, paged, cancellable, and observable. Supporting a
  long dataset does not permit unbounded eager inventory or verification.
- The process-wide resource broker and its non-stealable import progress lane
  remain the RAM/concurrency authority. The removed import working-memory UI
  does not return.

### Publication, cancellation, and resume

- A public destination is create-only. Existing destinations are never
  overwritten.
- Incomplete work remains under one hidden, no-follow, destination-bound stage
  and is never discoverable as a complete package.
- Normal production keeps at most one durable encoded copy of each completed
  payload. A temporary object file may coexist only for the one bounded object
  currently being committed.
- A completed journal record names an already durable final-layout object and
  its exact binding facts. A journal entry never claims bytes that have not
  crossed the required durability boundary.
- Cancellation or process loss discards at most one bounded incomplete suffix.
  Completed durable units remain reusable.
- The checkpoint schema is private and current-only. The cut invalidates
  predecessor canonical-cache/spool checkpoints with a clear restart action;
  it does not carry a migration reader.
- Publication still requires streaming structural, exact, and scientific
  validation followed by a create-only atomic rename on the destination
  filesystem.

### Performance

- Normal setup reads TIFF metadata only.
- During a normal uninterrupted run, every admitted native TIFF strip/tile is
  decoded once.
- Every logical S0/pyramid chunk is numerically produced once and encoded once.
- Encoded payload is not decoded and re-encoded to move from checkpoint to
  package.
- No full dataset is copied merely to change from "checkpoint" to "stage" or
  from "stage" to "final".
- Parallelism expands to the largest useful bounded window admitted by CPU,
  memory, codec, descriptor, and I/O policy. Bounded processing is not a reason
  to serialize independent volumes unnecessarily.
- Required independent final validation remains visible as its own measured
  phase; it is not hidden inside a throughput claim.

## Target Architecture

### 1. Replace DS fixture selection with compositional admission

`mirante4d-storage` will stop selecting the first aggregate `DS-0` through
`DS-4` envelope. Those classes and their representative-count equality tests
are deleted as product admission authorities.

One storage admission proof will instead have three explicit layers:

1. **Format proof** — the declared format tuple, arrays, shapes, chunk grids,
   indexes, manifests, paths, transforms, and checked aggregate counts are
   internally consistent and representable.
2. **Unit-safety proof** — every individual encoded/decoded inner chunk, outer
   shard, index page, metadata object, and requested spatial operation stays
   within its codec and bounded-work envelope.
3. **Aggregate traversal proof** — total arrays, shards, bricks, pages, and
   objects can be enumerated through bounded pages and checked arithmetic
   without eager allocation. Physical directory fan-out and addressability are
   checked against their actual independent constraints.

The active spatial geometry is validated once per image/layer shape. `T` and
`C` then multiply checked aggregate output counts. They affect output size and
work duration, but they do not retroactively make an otherwise supported
spatial frame unsafe.

If an aggregate ceiling remains necessary, implementation must derive it from
an actual field width or bounded traversal contract and include a boundary
test proving that derivation. It may not be calibrated to make a named dataset
pass. The change does not introduce `DS-5`.

Strict external package opening remains fail-closed. Metadata is checked before
use, inventories are paged, ordinary payload integrity remains lazy per
consumed object, and the explicit full audit remains cancellable. Larger
legitimate packages must not expand the attack surface into unbounded memory.

### 2. Make a temporal production unit the payload boundary

Import planning will construct the complete deterministic logical plan, but
execution will retain only a bounded window of spatial production units. The
default unit is one `(channel, timepoint)` volume; a volume whose decoded
workspace is too large for the broker uses the already-proved bounded Z-slab
path. The window may contain several independent units when resources and I/O
topology make that faster.

Channel zero/timepoint zero is processed first when automatic no-data work is
requested:

1. decode it once through bounded slab buffers;
2. accumulate uniform-block and constant-plane facts during that traversal;
3. reconstruct the exact fixed six-connected mask using row-packed state and
   its existing bounded spill authority;
4. durably checkpoint the resolved policy and mask digest; and
5. reuse that immutable spatial policy for every later unit.

The fixed mask may occupy storage proportional to one spatial volume. It may
not be duplicated per channel or timepoint.

For each subsequent production unit, a cascading pyramid pipeline consumes S0
slabs, produces completed aligned parent slabs, encodes complete logical
chunks, and releases finer input as soon as no later child depends on it.
Workers may prepare units out of order inside the bounded window, but one
commit sequencer publishes canonical ordinals, scientific-hash bytes, stage
objects, and journal records in deterministic order.

The asymptotic contract is:

```text
managed RAM = O(bounded worker window * largest admitted slab/unit)
payload scratch = O(largest admitted unit + bounded in-flight objects)
control journal = O(number of completed output objects)
final package = O(total scientific dataset and pyramid)
```

The small control journal may grow with the output inventory; full decoded or
encoded temporary payload may not.

### 3. Turn the hidden final-layout stage into the checkpoint

The destination parent owns one hidden sibling stage created with no-follow,
create-only semantics. Completed encoded shards are written directly to their
final package-relative paths inside that stage:

```text
validated source manifest
  -> bounded native decode
  -> bounded no-data/base/pyramid production
  -> one inner/outer encoding
  -> temporary single-object write
  -> durable rename into final-layout hidden stage
  -> compact durable completion journal
  -> streaming package validation
  -> create-only atomic publication rename
```

The stage's private control area contains the current plan binding, source
generation facts, resolved no-data state, committed-object records, canonical
scientific-hash checkpoint, and deterministic next ordinal. It is removed
before final package validation/publication and is never part of `.m4d`.

The hash checkpoint is an explicitly encoded private state owned by the
identity implementation, including the canonical byte count and partial-block
state needed to resume in order. It is accepted only with the matching import
plan and durable object prefix. This avoids re-decoding source or re-reading
all completed S0 data merely to reconstruct a sequential digest after a crash.

On resume, the importer validates the stage/control binding through bounded
paged records, removes any incomplete object temporary, restores the last
durable ordinal/hash state, revalidates source generations, and continues.
Normal resume redoes at most the incomplete spatial unit/object suffix. A
corrupt or mismatched stage fails closed and requires explicit reset.

### 4. Replace whole-package admission with incremental placement accounting

Import planning will produce a typed `StoragePlacementPlan` with at least:

- compressed selected-source bytes, informational only;
- decoded canonical S0 bytes, informational and useful for explaining codec
  expansion;
- exact logical final pixel and validity bytes, informational only;
- a summed, per-object conservative final-package ceiling, informational only;
- a maximum encoded-output bound for one temporal unit;
- the bounded remaining cache/spool/mask scratch for that unit;
- the compact control increment for one committed unit;
- a bounded finalization reserve for packed-index output, metadata, manifests,
  allocation granularity, and one incomplete-object commit; and
- the immediate additional free bytes required on each distinct filesystem.

The informational output ceiling is computed from each actual object's
occupied inner slots and the codec authority's encoded bound, including edge
objects and index/tail bytes. A partially occupied outer shard is never
charged as a fully occupied one. The calculation is not a blanket percentage
applied to the complete logical pyramid, and an empirical compression ratio is
never presented as guaranteed capacity. This ceiling explains scale; it is not
a hard reservation and is never compared with available space.

Placement accounting follows simultaneous liveness:

- objects already written into the hidden final-layout stage count zero
  additional bytes because `statvfs` already reflects them;
- the full decoded S0 cache contributes zero because it no longer exists;
- no separate dataset-scale encoded spool contributes;
- existing current-unit scratch also counts zero additional bytes, while only
  its bounded unfinished suffix is required;
- the next unit's not-yet-written final objects and compact control increment
  are included once;
- a bounded finalization reserve remains protected throughout production;
- scratch on a different filesystem is checked only against that filesystem;
- filesystem allocation granularity and bounded metadata/control bytes are
  included explicitly; and
- the destination must retain enough space to finish the current unit and the
  later atomic finalization, not every unprocessed timepoint.

The destination stage must remain on the final destination filesystem so its
publication rename is atomic. A scratch root may differ only through an
explicit placement authority that checks both filesystems independently.

Preflight remains mandatory, but it checks the first unit's immediate headroom
plus finalization reserve only. During execution, the writer tracks actual
stage and scratch bytes, subtracts already durable/current bytes from the next
step's bound, and rechecks before each temporal-unit and finalization commit.
The whole-package ceiling and all remaining future units are absent from that
comparison.

If available space falls below the immediate requirement, preprocessing keeps
the valid stage and reports a typed capacity pause with required and available
additional bytes. The product offers Resume without deleting the checkpoint.
An `ENOSPC` race during a write receives the same resumable treatment whenever
publication has not occurred. Invalid checkpoint state remains a distinct
fail-closed Reset-and-Restart action.

### 5. Make capacity truth visible in the preprocessing UI

The reviewed import summary will distinguish:

- `Selected TIFF files (compressed)`;
- `Decoded base data`;
- `Conservative final package ceiling (guidance; not reserved)`;
- `Maximum active-unit scratch`;
- `Maximum next-unit encoded commit`;
- `Finalization reserve`;
- `Immediate headroom required / available`; and
- a separate scratch-filesystem line only when placement differs.

During a run, progress reports the current temporal/channel unit, real stage
bytes written, non-reserved remaining package ceiling, actual scratch use,
immediate additional headroom, and the active phase. It does not show logical
bricks as though they were user work items, fabricate an ETA, ask the user to
choose working memory, or label a whole-package estimate as required space.

Failures are divided by control:

- source inconsistency, unsupported dtype/codec, missing calibration,
  duplicate destination, and insufficient selected-filesystem space remain
  actionable setup blockers;
- ordinary RAM/queue/descriptor contention remains broker-managed
  backpressure after Start;
- source mutation, persistent I/O failure, external disk exhaustion,
  corruption, or validation failure is a typed interruption with the valid
  stage retained whenever that state is safe; capacity exhaustion specifically
  offers non-destructive Resume; and
- no failure recommends changing the TIFF source when the actual constraint is
  package addressability, spatial support, or destination capacity.

## Authority Changes

| Concern | Target owner | Removed predecessor authority |
|---|---|---|
| Spatial pyramid geometry | `mirante4d-storage` | none; the existing shared geometry remains |
| Package safety/admission | `mirante4d-storage` compositional format, unit, and traversal proof | aggregate `ProfileKind::Ds0..Ds4` fixture selection |
| Deterministic source order | validated application/import source manifest | none; layout guessing remains deleted |
| Temporal/slab execution | `mirante4d-import-pipeline` bounded production window | whole-dataset canonical-base execution assumption |
| Encoded durable payload | destination-bound final-layout stage | separate encoded spool followed by package payload construction |
| Resume state | compact stage control journal and identity hash checkpoint | six-file canonical-cache/spool checkpoint schema |
| RAM/concurrency | process CPU broker and import progress lane | no local working-memory selector or hard category percentage |
| Disk proof | import-owned current-unit/finalization headroom using actual durable bytes | any whole-package free-space admission equation |
| Publication | storage staged validation and create-only atomic rename | none; safety semantics remain |
| User-facing capacity truth | application workflow and egui projection | generic unsupported-source guidance |

## Required Hard Deletions

Implementation is incomplete until it removes:

- aggregate `DS-0` through `DS-4` selection as package/import admission policy;
- profile-limit tests whose correctness condition is equality with one
  representative dataset's aggregate counts;
- global directory/object limits borrowed indirectly from the largest DS
  fixture when no independent field/path/traversal derivation exists;
- the complete `canonical_base` payload cache and its full-dataset size term;
- the dataset-scale encoded spool payload and checkpoint-to-writer payload
  transfer;
- the current six-file checkpoint reader/writer and any compatibility branch
  for resuming it;
- the `two_encoded_copies`/blanket whole-pyramid free-space equation;
- every Start/runtime check that compares available space with a complete
  future package or all remaining temporal units;
- full-outer-shard multiplication for partially occupied objects;
- preflight that applies one combined requirement independently to multiple
  filesystems instead of accounting each placement;
- duplicate normal-run source decode, pyramid generation, or payload encoding
  paths exposed by instrumentation; and
- tests that pass because the same production formula computes both the
  claimed requirement and its expected value, or because an inflated bound
  merely fits an unusually large development filesystem.

Small reusable codec, geometry, checksum, no-follow filesystem, broker,
no-data, and publication primitives remain. Deletion is about the obsolete
whole-dataset authorities, not rewriting sound low-level code for novelty.

## Implementation Packages

### P0 — Independent fact freeze and target interfaces

- Freeze current equations and failure reproduction with public generated
  geometries, independent arithmetic, and no owner dataset metadata.
- Record release-build throughput, source-decode, encoded-byte, temporary-byte,
  descriptor, and validation-pass baselines for existing public import
  workloads.
- Define the compositional admission facts, temporal production unit,
  final-layout stage journal, hash-checkpoint encoding, and typed placement
  plan before moving payloads.
- Prove which current ceilings derive from format/codec/path authorities and
  mark every fixture-derived ceiling for deletion.

### P1 — Storage admission cutover

- Replace DS selection with format, unit-safety, and aggregate-traversal proofs.
- Make catalog/admission iteration paged and cancellable wherever total counts
  are currently materialized.
- Retain exact shard, codec, scale, path, directory, arithmetic, and
  amplification protections under independently derived bounds.
- Update writer, catalog, source-open, inventory, package-validation, and test
  APIs to consume the single proof.
- Delete the representative-envelope constants and selection order.

### P2 — Final-layout staged checkpoint writer

- Add the destination-bound hidden stage and compact durable control journal.
- Write completed encoded objects once to final relative paths through
  temporary-object commit.
- Add stable private scientific-hash checkpoint encoding and deterministic
  ordinal resume.
- Support cancellation, process-loss suffix recovery, corruption rejection,
  explicit reset, control-area removal, streaming validation, and atomic
  publication.
- Prove that normal production has no dataset-scale encoded duplicate.

### P3 — Temporal/slab production cutover

- Route source decode, automatic no-data resolution, base production, pyramid
  cascade, scientific hashing, encoding, and stage commit through the bounded
  spatial-unit window.
- Preserve ordered identity/publication while allowing useful out-of-order
  compute.
- Release decoded/reduced buffers at their last dependent level.
- Retain only one fixed no-data mask/spill authority and bounded in-flight
  unit state.
- Remove the full canonical base cache and predecessor spool pipeline.

### P4 — Incremental placement and product diagnostics

- Compute occupied-slot per-object final codec bounds for informational
  guidance and independent current-unit/finalization hard headroom.
- Track actual stage/scratch bytes and immediate additional headroom during
  execution; never compare free space with all future output.
- Update setup, progress, and failure models with final-versus-temporary and
  filesystem-specific truth.
- Add same-filesystem placement tests, resumable capacity-pause checks, and
  bounded `ENOSPC` fault injection. This cutover exposes no split-filesystem
  scratch authority: checkpoint and destination remain siblings for atomic
  publication. Any future explicit split authority must add independent
  per-filesystem tests before it is admitted.

### P5 — Integration and predecessor deletion

- Move every application, automation, benchmark, writer, validation, and
  source-open caller to the new authorities.
- Hard-delete predecessor types, files, UI strings, checkpoint fixtures, and
  fallback branches.
- Update experimental format/checkpoint fixtures as one cut; add no migration
  or dual path.
- Synchronize architecture, data-format, current-state, testing, development,
  and user documentation with implemented facts.

### P6 — Qualification and product closeout

- Run focused storage/import/application/UI tests, format lifecycle checks,
  documentation checks, and the required repository gate.
- Compare clean-revision release medians against the frozen public baselines.
- Exercise generated long multichannel/time-series fixtures with independent
  output oracles and fault injection.
- Ask the owner to run the normal preprocessing workflow on the representative
  large local sources only after automated gates pass. Agent automation must
  not launch an unattended high-load private import on the workstation.
- Open each resulting package through the normal product, traverse distant
  timepoints/channels, and confirm exact metadata, representative values,
  validity, LODs, playback, source preservation, and atomic publication.
- Record only public-safe conclusions in the repository; private paths,
  identities, acquisition labels, and unpublished geometry remain outside it.

## Verification And Acceptance Gates

### Admission and security

- Fixed supported spatial geometry remains admitted as synthetic `T` and `C`
  grow, until an independently derived address, path, directory, or filesystem
  constraint is actually reached.
- Increasing only `T`/`C` changes aggregate counts and final bytes linearly; it
  does not change per-unit codec, decoder, renderer, or managed-RAM admission.
- Every remaining ceiling has a derivation test and one-above-boundary test.
- Malformed metadata with enormous counts fails checked arithmetic or bounded
  traversal before allocation.
- Package open, validation, and explicit audit stay bounded and cancellable on
  a generated large inventory.

### Memory, temporary space, and work

- Peak decoded/pyramid payload memory is bounded by the declared spatial-unit
  window and does not grow with completed timepoints.
- Peak non-output payload scratch excludes any complete decoded base or
  dataset-scale encoded spool. Only journal/control metadata may grow with the
  completed inventory.
- Exactly one durable final-layout encoded copy exists after each object
  commit; instrumentation permits only one bounded incomplete-object
  temporary.
- Setup performs zero pixel decode and zero complete-source hash pass.
- A normal uninterrupted import records one native decode and one logical
  encode per required payload, with no package-construction re-encode.
- Reverse worker completion, capacity withdrawal, cancellation, and restart do
  not reorder identity or expose partial output.

### Disk accounting

- An independent enumerating oracle derives occupied inner slots, edge-object
  bounds, one-unit output, finalization, and immediate headroom without calling
  or duplicating the production aggregate formula.
- Same-filesystem placement sums only simultaneously live bytes there. There
  is no split-filesystem branch in the current product; introducing one would
  require checking each filesystem's own burden.
- Increasing only `T/C` for unchanged spatial geometry increases
  informational final size and compact total control but does not change the
  hard Start/next-unit headroom.
- Edge-heavy coarse levels are bounded by their occupied inner slots, not one
  fully populated outer shard per temporal unit.
- Preflight accepts generated long cases when one unit plus bounded
  finalization fits even if the informational whole-package ceiling exceeds
  currently available space.
- A capacity interruption preserves its durable prefix and Resume continues
  without checkpoint deletion or duplicate publication.
- Fault injection at every commit boundary either completes or retains a
  resumable valid prefix; it never publishes a partial package.

### Scientific and package correctness

- Independent generated-fixture readers verify exact `T/C/Z/Y/X`, dtype,
  calibration, labels, transforms, time/Z order, representative/all values on
  tractable fixtures, validity at every LOD, and scientific content address.
- Automatic and manual no-data modes match their existing independent oracles,
  including disconnected equal-valued data and plane-local hiding.
- A clean run and multiple interruption/resume schedules produce the same
  package identity and scientific content.
- Source fixture bytes hash identically before and after import as an external
  test oracle; product setup does not add that whole-source hash pass.
- The stage is absent or private before publication, the destination appears
  only at atomic commit, and a pre-existing destination is unchanged.

### Performance and product evidence

- Existing predecessor-supported public release workloads do not regress by
  more than five percent in five-run median end-to-end time without a precise
  diagnosed cause and explicit owner acceptance.
- Component evidence reports metadata inspection, native decode, no-data,
  reduction, encoding, stage I/O, journal/durability, final validation, and
  capacity wait separately.
- Large synthetic scaling demonstrates useful bounded concurrency rather than
  one-volume forced serialization or unbounded read/write thrashing.
- A speed claim requires shorter verified end-to-end time or removal of a
  structurally redundant pass/copy. Thread count and CPU utilization alone are
  not success.
- Product closeout requires the normal app and actual preprocessing workflow;
  unit arithmetic or a self-reported success field cannot substitute for an
  opened, navigable result.

## Risks And Mitigations

- **A permissive profile becomes a denial-of-service path.** Keep strict
  per-object limits, checked counts, paged/cancellable traversal, and prohibit
  total-count-sized allocations.
- **Streaming changes numeric or identity order.** Separate parallel compute
  from one canonical commit sequencer and verify clean/resumed outputs against
  an independent oracle.
- **One-volume processing becomes needless serialization.** Use a broker-sized
  sliding window and phase-specific concurrency; measure source, CPU, and
  destination contention.
- **Pyramid slabs are released too early.** Encode dependency/lifetime rules in
  the cascade and test odd tails, boundaries, cancellation, and reversed
  completion.
- **Stage journaling claims non-durable data.** Commit object bytes and required
  directory durability before advancing the chained journal watermark.
- **Resume trusts corrupted state.** Bind plan, source generations, journal
  chain, object sizes/checksums, hash state, and next ordinal; reject rather
  than repair ambiguity.
- **Incremental admission reaches genuine disk exhaustion.** Use codec bounds
  for the current durable step, preserve a bounded finalization reserve,
  recheck before every unit, retain the valid stage, and expose Resume. Do not
  turn uncertainty about future compression into a false Start rejection.
- **Final validation erases throughput gains.** Measure it independently and
  keep it bounded/streaming; do not weaken publication correctness to improve
  a headline time.
- **The refactor recreates framework overhead.** Keep one temporal/slab owner,
  one stage writer, one compact journal, and the existing process broker. Do
  not introduce a general workflow engine or provenance system.
- **Private product evidence leaks into the repository.** Use generated public
  fixtures for committed facts and record owner data only as a private local
  validation result.

## Explicitly Out Of Scope

- TIFF compression-policy changes or a new `.m4d` codec.
- Reconsidering the current scientific pyramid, no-data semantics, transforms,
  validity representation, or dtype support.
- Float16 or mixed-dtype channel publication.
- TIFF source-layout inference, filename semantics, or wizard redesign beyond
  capacity/progress truth.
- Renderer, GPU residency, playback, or LOD changes.
- Making all timepoints resident in RAM or GPU memory.
- Stable public-format promises, legacy checkpoint migration, or repair of
  malformed packages.
- Automatically preprocessing owner/private datasets as part of implementation
  automation.

## Relationship To Completed Plans

This plan retains the explicit source manifest, dataset-optional application
shell, process broker, decode-once intent, automatic no-data semantics, shared
pyramid geometry, actionable error projection, staged validation, and atomic
publication established by the
[preprocessing application and importer cutover](PREPROCESSING_APPLICATION_AND_IMPORTER_CUTOVER.md),
[generalized no-data plan](GENERALIZED_NO_DATA_IMPORT.md), and
[time-series profile correction](TIME_SERIES_IMPORT_PROFILE_CORRECTION.md).

It supersedes only these predecessor conclusions:

- representative aggregate DS envelopes are sufficient long-term admission
  policy;
- a full canonical base cache is an acceptable dataset-scale checkpoint;
- a separate complete encoded spool should precede final package construction;
  and
- free-space safety should budget two blanket 125-percent encoded copies.

Those completed documents remain historical implementation records. This
document is the sole target authority for their large-dataset storage and
temporary-payload replacement.

## First Implementation Result And Withdrawn Capacity Closeout

The first implementation completed these retained P1 through P3/P5 facts:

- `M4D-COMPOSITIONAL-1` is the only package-admission contract; aggregate
  `DS-0` through `DS-4` selection and its fixture-equality tests are absent;
- format, per-object, shard, manifest, path, and aggregate traversal bounds
  are independently derived, including a 65,536-descriptor ceiling from the
  explicit 64 MiB manifest working-set authority;
- import retains one active `(timepoint, channel)` canonical cache and one
  bounded encoded-inner spool, while committed outer shards live only in the
  destination-bound resumable final-layout stage;
- the unit journal, sparse decoded-digest store, packed records, resolved
  no-data policy, stage journal, and resumable scientific hasher are the sole
  checkpoint control authorities; the predecessor whole-dataset checkpoint
  has no reader or migration branch;
- setup and running projections removed the user working-memory control and
  exposed temporal-unit and durable-stage facts.

The first capacity implementation is not accepted. It retained whole-package
hard admission, charged every outer shard at full occupancy, and used an
expected-value test with the same conceptual error. Its previously recorded
1,336-case Rust pass remains evidence only for the mechanics it actually
exercised. The correction has separate automated evidence: an independently
enumerating object oracle, edge-heavy geometry checks, fixed-spatial `T/C`
growth checks, exact durable-range accounting, typed setup-versus-runtime
capacity tests, and native Reset-versus-Resume UI tests.

The bounded native `import_preprocessing` scenario was rerun after the
correction and passed on its public
generated source. It selected the immediate TIFF directory through the same
explicit channel manifest used to derive the destination, cancelled after a
512-work-unit durable prefix, resumed exactly those 512 units, published once,
transferred the self-consistent capability into the normal product open path,
navigated a nonblank rendering, and preserved all 65 source TIFF files byte
for byte. The successful receipt reported a 478,741,792-byte bounded working
peak below the scenario's 512 MiB execution limit. Its receipt now reports
`preflight_required_headroom_bytes` rather than pretending that observed total
stage growth must remain below a whole-package preflight reservation. This is
supporting native automation evidence, not owner visual validation or a
current throughput qualification; the independent planner tests, not the
fixture's available disk, own the corrected admission proof.

The final automated closeout also passed the complete public pull-request
gate: zero-warning workspace Clippy, exact test discovery, and all 1,347
assigned unit, contract, and UI tests. The storage-format lifecycle lane
separately passed promoted-fixture mutation checks, representative
large-manifest traversal, and an independent production reader. Documentation
inventory/link checks and generated verification synchronization also passed.
These results establish the implemented contracts in the current working
tree.

No agent-run private import, private path, dataset identity, or unpublished
geometry is repository evidence. On 2026-08-01 the owner reported that the
normal preprocessing workflows exercised after the completed cutovers all
worked correctly and explicitly accepted the result as validated. That
attestation closes product validation without publishing private source facts
or turning it into a universal throughput or maximum-dataset claim.

## Completion Standard

The cutover is complete only when compositional admission replaces DS fixture
selection; long `T/C` no longer causes a false profile rejection; the normal
import path has neither a complete decoded base cache nor a second
dataset-scale encoded payload; the hidden final-layout stage resumes and
publishes atomically; hard capacity admission is per current temporal unit plus
bounded finalization and never the complete future package; occupied edge
objects and simultaneous liveness have independent expected facts; a genuine
capacity pause resumes without deleting its durable prefix; the UI labels
whole-package guidance as non-reserved; scientific/package/source invariants
pass independent checks;
existing public performance remains inside the accepted boundary; generated
large scaling and fault tests pass; predecessor code and checkpoints are
deleted; documentation describes implemented facts; and the owner validates
the normal product workflow on representative large local data.
