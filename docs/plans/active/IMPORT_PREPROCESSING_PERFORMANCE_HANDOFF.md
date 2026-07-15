# Import And Preprocessing Performance Handoff

Status: ACTIVE — IMPLEMENTED, QUALIFICATION PENDING
Implementation authorization: IP-00 THROUGH IP-04
Last reviewed: 2026-07-15
Planning predecessor: clean `fac91fe52f752cbf30faf79f06c4406d0a4baf4c`
Target sequence: IP-00, IP-01, IP-02, IP-03, then IP-04

## Purpose

The importer at this handoff's planning predecessor was bounded, cancellable,
restartable, sharded, and scientifically validated, but its source access
pattern made a practical private DS-3 workload take multiple hours. This
handoff defines the implemented hard cutover to a locality-correct importer
and the still-required evidence for accepting preprocessing performance
without weakening source safety, identity, validation, restart, or publication
guarantees.

This document remains the approved target and acceptance record. IP-00 through
IP-03 and the IP-04 qualification tooling are implemented in the current
working state; the owning current-state and technical documents describe those
live authorities. IP-04 is not accepted: clean T2 timing, owner-bound private
T5 execution, private real-display product validation on HW-2, and acceptance
of the final deletion audit at the qualifying revision remain. The current
working-tree deletion audit found no alternate importer, predecessor
checkpoint reader, per-unit durability route, decoded publication route,
fixed-percentage progress route, post-publication exact/scientific rescan, or
hidden importer selector. The public
supporting target-source and generated-import scenarios have passed on a
mapped physical X display, but do not substitute for T5/HW-2. Detailed private
workload paths, dimensions, identities, and raw reports remain outside the
repository.

## Implementation Checkpoint

Implemented behavior includes:

- one reviewed source index and a closed TIFF compression admission set:
  uncompressed, LZW, current/old Deflate, and PackBits; JPEG, WebP,
  Zstd-in-TIFF, fax, and other unaudited decoder paths fail closed;
- cancellation-aware fixed-read preflight of the complete primary IFD chain,
  with tight eager-field and native-table cardinalities, 4,096 entries per
  IFD, a 65,536-page ceiling, and page-proportional retained-decoder charging;
- source-native exact-once strip/tile traversal, with each opened source file
  descriptor checked against guarded device, inode, size, modification, and
  change-time facts, reviewed-generation comparison around ingest, and a final
  descriptor-bound exact SHA-256 pass;
- one six-file checkpoint: a two-file descriptor-bound canonical cache and a
  four-file encoded spool, with no predecessor reader;
- canonical durability triggers of 16 completed planes, 64 MiB, and a
  15-second age check, plus spool triggers of 512 work units, 64 MiB, and the
  same age check, with earlier commits at stage or serialized-decoder
  boundaries and typed rejection of a canonical plane above 64 MiB;
- byte-ledger-admitted ordered workers, shared positional checkpoint readers,
  encoded inner-payload publication, and one-brick-once staged scientific
  validation;
- stage/CPU timing plus source, decode, checkpoint, inner-codec, durability,
  staged-object-read, file-descriptor, temporary-byte, ledger, RSS, and
  publication/open-ready evidence; and
- the release-only T2 command, normal-product T5 runner, generated import
  product scenario, qualification-profile binding, build/executable
  provenance, and sanitized/private evidence separation.

Focused automated tests, one dirty-worktree release diagnostic, and the two
public supporting product scenarios on a mapped physical X display are
implementation evidence only. The owner-accepted HW-2 profile and T5
configuration digests are unset, so the tools deliberately cannot make a
qualification claim. No public or private absolute timing gate is recorded as
passed here. [Testing and evidence](../../TESTING.md) owns the implemented
sync, inner-codec, staged-object-read, descriptor, RSS, and byte-bound counter
semantics used by both runners.

## Observed Problem

The read-only investigation found one dominant structural defect and several
secondary costs:

- Base production traverses destination `64 x 64 x 64` bricks and requests one
  small source region for each brick. A TIFF stored as one full-image strip
  must decode that whole strip for every intersecting XY brick.
- Each region request rescans inspected source records, reopens intersecting
  TIFFs, reconstructs decoders, and loses source-native locality.
- The separate source scientific-identity pass repeats TIFF decoding in
  `1 x 16 x 256 x 256` identity tiles instead of sharing the canonical base
  flow.
- Every checkpoint work unit synchronizes payload and journal separately.
- Publication decodes checkpoint payloads and encodes the same inner chunks
  again.
- Staged scientific validation uses the cold random-access brick path; the
  `z=16` identity tiles can decode one `z=64` base brick four times.
- Before the capability-transfer cutover, normal product open repeated staged
  exact and scientific validation. The implemented path now transfers that
  authority through rename and performs metadata-only currentness checks.
- Import work is effectively serial, and progress percentages do not represent
  measured remaining time.

The SHA-256 operations themselves are not the multi-hour cause. The problem is
the amplified and duplicated data traversal used to feed production,
identity, and validation. Raw investigation evidence remains ignored local
material; the public synthetic workload in IP-00 must reproduce the defect
without copying private dataset facts.

## Target Outcome

One product import route shall:

1. index reviewed source layout once;
2. traverse TIFF native strips or tiles in source order and decode each native
   chunk once;
3. normalize pixels and validity into a bounded, fixed-object,
   external-memory reblocking checkpoint;
4. calculate source statistics and scientific identity while canonical data
   is already flowing, or through one sequential canonical-checkpoint pass;
5. produce deterministic base bricks and pyramid levels through bounded,
   byte-accounted work;
6. checkpoint bounded durability batches rather than individual logical
   bricks;
7. publish canonical encoded inner payloads without a decode/re-encode cycle;
8. independently validate the staged package through a locality-aware reader;
9. publish atomically to a previously absent destination; and
10. seal the staged verified capability to the publication directory, rebind
    it across the proved atomic rename, and consume it through a metadata-only
    currentness check before direct verified product open.

The active `m4d-science-1.0` and `m4d-zarr3-local-1.0` package contracts,
scientific identity scheme, package identity scheme, runtime brick shape, and
outer sharding profile remain unchanged.

## Non-Negotiable Invariants

1. Source microscopy files are never modified. Source drift from inspection
   and review through the final prepublication source check is detected and
   fails closed.
2. The selected 256 MiB import working-memory budget covers decoder
   allocations, canonical buffers, codec buffers, worker queues, and reorder
   buffers. No full dataset or unbounded slab is materialized.
3. Open files, pending work, temporary bytes, checkpoint files, output objects,
   and directory fan-out are explicitly bounded before work starts.
4. Cancellation is checked throughout source traversal, reblocking, pyramid
   generation, publication, and validation. Stale results never publish.
5. Restart resumes only a validated durable prefix. Recovery may recompute a
   bounded uncommitted batch but may not accept ambiguous checkpoint bytes.
6. Production arrays remain sharded. No file-per-brick, record-per-brick
   manifest, or comparable sidecar layout is permitted.
7. Scientific content remains storage-independent. Exact package identity
   continues to cover the exact package closure.
8. One independent staged scientific readback remains mandatory. Optimizing
   its traversal must not turn writer/reader self-agreement into the oracle.
9. Publication remains create-only and atomic. No incomplete destination is
   exposed as complete, and an existing destination is never replaced.
10. Capacity, unsupported-source, mutation, corruption, cancellation, and
    indeterminate-durability failures remain typed and visible.
11. There is one live importer and one checkpoint schema after each authority
    cutover. No fallback, compatibility checkpoint reader, selectable old path,
    or environment-controlled alternate survives.
12. Public fixtures and documentation contain no private source path, dataset
    nickname, unpublished geometry, raw identity, or qualification payload.

## Scope

Allowed implementation scope:

- `mirante4d-import-pipeline`: source indexing and traversal, canonical
  reblocking, checkpointing, statistics, scientific hashing, pyramid
  production, counters, cancellation, and import tests;
- `mirante4d-storage`: narrow typed support for validated canonical encoded
  inner payloads and a sequential staged scientific-validation path;
- `mirante4d-app` and `mirante4d-ui-egui`: stage projection, measured progress,
  open-ready verification transfer where proved safe, and an import-specific
  product-validation scenario;
- `xtask`, verification registration, and generated public source fixtures
  needed for reproducible local performance evidence; and
- owning current-state, architecture, data-format, testing, development, and
  planning documentation when their facts actually change.

The identity crate may receive focused tests or instrumentation, but its
persisted identity algorithms and canonical domains are out of scope.

## Non-Goals

- Changing the active `.m4d` semantic or storage profile.
- Changing scientific, exact-object, package, recipe, or derivation identity
  semantics.
- Adding a generic OME-Zarr reader, proprietary source formats, remote stores,
  cloud execution, or public-data release machinery.
- Raising the default working-memory budget to hide poor locality.
- Reintroducing whole-stack or whole-timepoint residency.
- Changing renderer, analysis, project-store, platform, GPU, or display
  support claims.
- Adding segmentation, migration machinery, compatibility readers, or a
  second importer.
- Qualifying debug builds or hosted-runner timing as product performance.
- Making a relative speedup, p95, p99, or worst-case timing claim from the
  small sample counts in this handoff.

## Target Authority And Deletions

IP-02 made the accepted source-native ingest/reblocker the sole base-import and
checkpoint authority. The implementation removed or made unreachable the
following predecessors. A repository-wide working-tree deletion audit found
no alternate route; IP-04 still requires that result to be accepted at the
clean immutable qualification revision:

- destination-brick-driven TIFF decoding in production;
- production-time full source-vector scans and per-region TIFF reopen/decoder
  construction;
- the separate TIFF reread used only to compute source scientific identity;
- per-work-unit payload and journal durability barriers;
- the predecessor checkpoint schema and any compatibility reader for it;
- the decoded-checkpoint-to-re-encoded-shard publication route;
- repeated cold scientific brick reads once the sequential validator is
  active; and
- arbitrary fixed progress percentages once measured stage progress is active.

The staged verified capability is now rebound across the atomic rename through
a linear, destination-bound transfer. Within the cooperative local
destination-parent namespace assumed by the storage contract, pre-rename and
pre-admission inventory/snapshot/inventory checks reject extra or missing
paths, object mutation, and root replacement. A hostile actor able to rename or
unlink entries in that parent is outside the contract because Unix cannot
atomically bind a source name to an already-open directory descriptor. Transfer
failure within the contract is visible and does not fall back to the normal
external-open verifier. The publication-to-open-ready sub-clock remains inside
the performance gate.

Existing incomplete checkpoints may be explicitly rejected and reset after
the checkpoint hard cutover. Source files and already published packages are
never removed or rewritten by that reset.

## Target Flow

```text
reviewed TIFF files
  -> indexed source-native strip/tile traversal
  -> fixed descriptor-bound canonical base cache and durable prefix
  -> deterministic base bricks, statistics, scientific leaves, and pyramids
  -> canonical encoded inner payloads and indexed outer shards
  -> independent exact and sequential scientific validation
  -> metadata-only staged currentness proof
  -> create-only atomic publication
  -> destination-bound verified capability transfer
  -> metadata-only published currentness proof
  -> verified, open-ready product source
```

The implemented checkpoint uses a fixed six-regular-file layout, independent
of source-file, logical-brick, and dataset dimensions;
final package staging remains governed by the existing package object and
fan-out ceilings. Source and CPU task admission adapt only through checked
memory, native-chunk, and codec-workspace ceilings, not a source-specific
fallback.

## Work-Package Sequence

The sequence is dependency order, not a menu. Each package leaves the product
usable and has one revertable authority boundary.

### IP-00 — Reproducible Baseline And Observability

Implementation status: implemented. The receipt, T2 runner, qualification
profile, revision/executable/build provenance, continuous descriptor sampler,
structural descriptor bound, and reconciliation gates exist. A clean
five-session T2 qualification set has not been run or accepted.

Goal: make every material import stage and amplification source measurable in
a release build before changing the production authority.

Required work:

- Add stage wall/CPU timers for inspection, each source revalidation,
  checkpoint open/resume, base production, every pyramid scale, source
  scientific identity, shard publication, staged structure/exact/scientific
  validation, commit, and publication-to-open-ready capability transfer.
- Count source raw bytes, native decoded bytes, TIFF opens and native chunks,
  logical output bytes, checkpoint bytes, codec calls and time, sync calls and
  time, scientific brick reads, object reads, peak live file descriptors,
  peak temporary/staged bytes, peak accounted bytes, and externally observed
  process RSS.
- Record immutable revision, executable digest, build profile, toolchain,
  package profile, memory selection, filesystem class, cache condition, and
  process-session identity in local evidence.
- Add a generated T2 full-plane-strip workload with deterministic values and
  explicit validity: `uint8`, `z=65`, `y=1025`, `x=2049`, one file per plane,
  and one full-image strip per file. Fixture generation is outside the timed
  interval and generated pixels/packages are not committed.
- Freeze independently produced T2 scientific and per-pyramid-scale expected
  facts. Freeze the corresponding opaque T5 facts in private local evidence
  before optimization begins; predecessor output alone is not the oracle.
- Expose honest named progress stages. ETA may remain unavailable until enough
  measured work exists; a fabricated percentage is not acceptable.

Exit proof:

- One release run of the predecessor on T2 records every required counter and
  reproduces destination-brick/full-strip amplification.
- Counter reconciliation proves that stage totals and the final receipt agree.
- The concrete local evidence schema is frozen before IP-01 begins.
- No import output, identity, durability, or product authority changes in this
  package.

### IP-01 — Source-Native Decode-Once Ingest Candidate

Implementation status: implemented and activated by IP-02. The live path
decodes admitted native chunks into the canonical authority, derives identity
without a TIFF reread, and binds the opened source descriptor as well as its
path generation. There is no runtime selector or dormant candidate route.

Goal: build and prove one bounded source-native traversal and external-memory
reblocking candidate without creating a second product route.

Required work:

- Build a checked channel/time/Z index once and retain only bounded decoder
  state.
- Traverse every admitted TIFF strip or tile once, normalize canonical
  little-endian values and effective validity, and distribute bounded
  supertiles into the fixed-object checkpoint.
- Derive source statistics and scientific tile leaves from the canonical flow
  or one sequential canonical-checkpoint pass; do not reopen TIFFs for
  scientific identity.
- Preserve strong source mutation detection and source-file provenance while
  removing redundant full-file traversals where safe.
- Feed the existing base/pyramid semantics from the new canonical authority
  and retain deterministic order at the checkpoint boundary.
- Keep the candidate unreachable from the product and without a runtime
  selector. The predecessor remains the sole product/checkpoint authority
  until IP-02 performs the atomic activation and deletion.

Exit proof:

- T1/source-fixture facts and focused TIFF-layout tests agree with independent
  expected pixels, validity, axes, calibration, and scientific identity.
- On T2, base native decoded bytes are at most 1.10 times canonical source
  pixel bytes, and source scientific identity adds zero TIFF native decodes.
- Mutation, cancellation, source-chunk capacity, and temporary-space failures
  fail closed without source or destination mutation.
- Independent expected facts cover base scientific identity and every pyramid
  scale; candidate agreement with predecessor output alone is insufficient.

### IP-02 — Ingest, Durability, And Encoded Publication Cutover

Implementation status: implemented. The sole checkpoint is the six-file
canonical-cache/spool schema; encoded pixel and validity inners pass through to
the shard writer, recovery accepts only a validated durable prefix, and the
predecessor checkpoint and decoded publication paths are absent. Final IP-04
clean-revision deletion-audit acceptance remains pending.

Goal: activate the decode-once candidate, remove logical-brick-proportional
durability and duplicate inner-codec work, and leave one product/checkpoint
authority.

Required work:

- Define byte/time/work-bounded durability batches and a separately durable
  prefix watermark.
- On recovery, validate the prefix and discard or recompute at most one
  uncommitted batch; fault-inject every payload, journal, watermark, truncate,
  and sync boundary.
- Introduce one typed boundary for canonical encoded inner payloads. Validate
  kind, decoded length, encoded length, checksum, ordering, and codec profile
  before shard assembly.
- Keep one codec/canonicalization authority and retain independent staged
  package validation.
- Activate the IP-01 ingest candidate and its batched checkpoint schema in one
  hard cut. Explicitly reject predecessor checkpoints, delete the old ingest
  and checkpoint paths, delete per-work-unit sync behavior, and delete
  publication decode/re-encode behavior.

Exit proof:

- Every injected crash either resumes the exact durable prefix or rejects the
  checkpoint; no ambiguous suffix is accepted.
- Candidate two-run output from the same immutable executable and input is
  deterministic. Cross-revision `PackageId` equality is not required because
  executable derivation provenance intentionally changes it.
- Exact/scientific target validation and current shard/object/fan-out ceilings
  still pass.

### IP-03 — Bounded Concurrency, Sequential Validation, And Product Progress

Implementation status: implemented. CPU-heavy eligible source, base, pyramid,
scientific-tile, and codec work uses bounded ordered workers. Staged scientific
validation holds each present base brick once while preparing its intersecting
identity leaves. Capability transfer is implemented as a non-cloneable,
filesystem-identity-bound handoff with metadata-only currentness checks. The
ordinary exact/scientific verifier is not run after publication.

Goal: use available CPU cores after locality and durability are correct, and
remove avoidable validation amplification from the end-to-end product path.

Required work:

- Add a byte-ledger-accounted worker pool for admitted decode, normalization,
  pyramid, statistics, and codec work. One ordered owner commits checkpoint and
  package state through a bounded reorder buffer.
- Derive worker count and queue admission from the selected byte budget and
  native chunk ceiling, not from an unbounded task count.
- Reuse buffers where semantics permit and keep cancellation/stale suppression
  generation-aware.
- Add a sequential staged scientific scan that holds bounded shard/index state,
  reads each base brick once, and derives all intersecting `z=16` identity
  leaves without repeated decompression.
- Within the documented cooperative local namespace threat model, transfer the
  verified capability across atomic publication only through the fail-closed
  root-seal/rebind and inventory/snapshot/inventory proof. Reject the automatic
  open on failure; do not retain a verification fallback.
- Replace coarse progress projection with named stages, measured completed
  work, elapsed time, and a calibrated ETA only where its basis is valid.

Exit proof:

- Worker queues, reorder buffers, decoder allocations, and codec buffers remain
  within the 256 MiB import ledger under pressure tests.
- Candidate output remains deterministic across worker counts supported by the
  policy.
- The staged scientific scan reads each present base brick no more than once;
  exact and independent scientific validation still reject mutation and
  mismatched content.
- Product completion is reported only after the target is safely published and
  has the verified authority required for normal use.

### IP-04 — Qualification, Deletion Audit, And Handoff Closeout

Implementation status: tooling implemented, acceptance pending. The generated
normal-product import scenario and strict private T5 runner exist, and the two
public supporting scenarios have passed on a mapped physical X display. The
clean complete check set at one immutable revision, five T2 sessions, three
owner-bound T5 sessions, private real-display HW-2 exercise, and final deletion
audit at that revision are not recorded as accepted. The current working-tree
deletion audit found no alternate production authority.

Goal: prove the complete normal product path on public support fixtures and the
opaque private DS-3 qualification workload, then close the temporary plan.

Required work:

- Run all functional, structural, performance, cancellation/resume, and product
  gates below from one clean immutable release revision.
- Exercise the actual native import UI on a real mapped display, including
  review, start, progress, cancellation, resume, completion, open, and one
  rendered navigation check.
- Audit the repository for predecessor source access, old checkpoint schema,
  per-unit durability, decode/re-encode publication, arbitrary progress, hidden
  feature flags, and alternate importer routes.
- Update current-state and owning technical documentation only for behavior
  proved at the accepted revision. Remove this active handoff after closeout;
  Git history remains the plan archive.

Exit proof:

- Every mandatory gate passes with its exact revision, configuration, sample
  set, and evidence class recorded.
- The normal application imports the private T5/DS-3 workload within the
  absolute median gate and opens the verified result without a fallback.
- The owner accepts remaining risks and the deletion audit finds one importer,
  one checkpoint authority, and one verification route.

## Acceptance Workloads And Timing Protocol

### T1 — Independent Correctness Corpus

The existing independent source and target fixtures remain the scientific and
format conformance authority. Production round trips do not bless their own
expected facts.

### T2 — Public Synthetic Performance Workload

IP-00 freezes the generated full-plane-strip workload above before candidate
tuning. Each timed sample uses the same immutable generated source, a fresh
checkpoint and absent destination, one release process session, and the
selected 256 MiB import budget. Fixture generation and human interaction are
outside the timed interval.

Mandatory T2 timing runs use the same fixed, fingerprinted `HW-2` workstation,
qualified ext4 storage tuple, declared scratch location, and recorded cache and
competing-activity conditions as T5. Runs elsewhere are diagnostics and cannot
satisfy the timing gate.

### T5 — Private Product Qualification Workload

The owner-provided DS-3 workload remains private and is identified in evidence
only by an opaque local ID. Raw paths, dimensions, filenames, source hashes,
package identities, and unpublished metadata do not enter the repository.

T5 runs use the fixed trusted local `HW-2` workstation, its qualified local
ext4 storage tuple, one immutable release executable, a fresh checkpoint and
absent destination, and the normal native application path. Cache condition
and competing system activity are recorded for every sample and are not
changed selectively within a sample set.

The primary clock starts when the accepted **Start Import** command reaches the
worker and ends when the published destination is verified and open-ready for
normal product use. Inspection time is reported separately. The exact
`Published` event through capability consumption and verified runtime open is
a named sub-clock inside the primary clock. Its storage-issued execution
receipt observes separate strict-open deltas for both inventories and the
proof-derived snapshot sweep, reconciles their total, and observes zero codec
decodes. A structural call-path assertion separately forbids exact/scientific
validators and brick reads. Product runtime evidence requires no active
verifier at open-ready and zero started, failed, accepted-progress, cancelled,
or accepted-success ordinary-verifier runs. Human review time is never
included.

`source traffic bytes` means application-level bytes returned by reads of the
reviewed TIFF files during the primary clock, including TIFF headers, IFDs,
encoded payload, and raw integrity traversals. It excludes the separately
reported pre-review inspection and does not mean physical device bytes or page
cache misses. Its denominator is the sum of unique reviewed source-file byte
lengths. Any inspection or reinspection performed after **Start Import** is
inside both the clock and the traffic counter.

## Mandatory Gates

| Gate | Workload and sampling | Required result |
| --- | --- | --- |
| Public absolute timing | T2, five independent release process sessions | Median primary-clock time at most 60 seconds |
| Private absolute timing | T5/DS-3, three independent release process sessions | Median primary-clock time at most 15 minutes |
| Base decode amplification | Every timed T2 and T5 run | Native decoded pixel bytes at most 1.10 times canonical source pixel bytes |
| Source scientific traversal | Every timed T2 and T5 run | Zero additional TIFF native decodes for scientific identity |
| Timed import source traffic | Every timed T2 and T5 run | At most 2.50 times unique raw source bytes, including one strong integrity traversal |
| Durability calls | Every timed T5 run | Fewer than 5,000 aggregate import-process `sync_calls` (file/directory `sync_all` and `fsync`) and no work-unit-proportional sync path |
| Working memory | Pressure tests and every timed run | Import-ledger peak at most 256 MiB with all import buffers and queues accounted |
| External memory check | Every timed T2 and T5 run | Peak process RSS minus the pre-import idle baseline at most 384 MiB, with ledger-external overhead reconciled |
| Open-file bound | Pressure tests and every timed run | At most 64 simultaneously open import-owned file descriptors |
| Temporary-byte bound | Pressure tests and every timed run | Peak checkpoint plus owned stage bytes do not exceed the preflight-declared bound |
| Temporary object bound | Pressure tests and every timed run | At most eight regular checkpoint files; final stage remains within target-profile object/fan-out ceilings |
| Scientific validation locality | T1, T2, and T5 | Each present base brick decoded at most once per staged scientific validation |
| Publication capability transfer | T1, T2, and T5 | Root binding and inventory/snapshot/inventory currentness pass; its storage-issued receipt has equal positive inventory strict-open deltas, an observed snapshot delta equal to the exact proof's expected object count, an exactly reconciled total, and zero codec decodes; the structural call-path assertion forbids exact/scientific validators and brick reads; product runs have no active verifier at open-ready and zero started, failed, accepted-progress, cancelled, or accepted-success ordinary-verifier runs |
| Scientific correctness | T1 and every successful T2/T5 run | Independent expected facts or staged independent reader agrees on values, validity, axes, calibration, and scientific ID |
| Semantic regression | T1, T2, and T5 | Candidate scientific ID and independently checked values/digests at every pyramid scale match the frozen expected facts |
| Exact correctness | T1 and every successful T2/T5 run | Exact package closure validates; missing, extra, truncated, changed, or corrupt objects fail |
| Determinism | Two fresh candidate runs from one immutable executable and input | Same scientific ID and same exact package ID; no cross-revision package-ID equality claim |
| Source preservation | T1, T2, and T5 | Before/after source inventory and SHA-256 facts are unchanged |
| Cancellation and resume | Fault matrix plus product exercise | Cancellation completes after at most one admitted native chunk; resume recomputes at most one declared durability batch |
| Progress truthfulness | Product exercise | Every material stage is visible; complete appears only after verified publication; any ETA names a measured basis |

The non-blocking stretch target for the three-run T5 median is 10 minutes. It
does not replace or weaken the 15-minute gate.

These are absolute median gates for the exact declared workloads. This handoff
makes no relative speedup or timing-tail claim. A later relative claim requires
at least twenty interleaved independent baseline/candidate pairs; a later tail
gate requires sixty independent sessions and the accepted statistical policy.
Historical multi-hour observations are diagnostic context only.

## Verification

Focused automated coverage must include:

- full-plane, narrow-strip, and tiled TIFFs;
- uncompressed, LZW, current/old Deflate, and PackBits TIFFs, plus rejection of
  JPEG, WebP, Zstd-in-TIFF, fax, and other unsupported codecs;
- single-page plane series and multipage stacks;
- 2D and 3D inputs, endian variants, supported integer and finite-float dtypes,
  explicit validity, and exact/over-boundary dimensions;
- source mutation before, during, and after ingest;
- cancellation at every stage and stale-result suppression;
- fresh, resumed, truncated, corrupt, reordered, and wrong-source checkpoints;
- durability failpoints around payload, journal, watermark, sync, validation,
  publication, and rename;
- deterministic output across repeated runs and admitted worker counts;
- independent expected values or digests for every generated pyramid scale;
- independent exact/scientific rejection after package mutation; and
- source, temporary-object, shard, total-object, fan-out, and memory bounds.

Required repository checks at the final affected revision:

```bash
python3 tools/source-fixtures/validate.py \
  --manifest fixtures/source/manifest.json --self-test
python3 tools/target-fixtures/t1/validate.py \
  --manifest fixtures/target/manifest.json --self-test
cargo fmt --all
cargo xtask verify-pr
cargo xtask verify-local format-lifecycle
cargo xtask product-validate target_source_verification
cargo xtask product-validate import_preprocessing
```

The encoded-writer and sequential-reader changes make the local
`format-lifecycle` check mandatory. The existing target-source and generated
import scenarios passed on a mapped physical X display as supporting
regression evidence, not preprocessing qualification. IP-04 must still
exercise the normal application on a real mapped display with T5/HW-2.

Public CI receives only bounded source, target, and functional checks. T2 and
T5 performance evidence runs locally; T5 raw evidence stays ignored and
private. Any sanitized completion record names the revision, executable
digest, build, toolchain, opaque workload/profile, hardware class, filesystem,
memory budget, cache condition, commands, samples, failures, skips, waivers,
and remaining risks without publishing raw private identities.

## Risks And Stop Conditions

Stop implementation and return for owner review if any package requires:

- changing the `.m4d` storage profile or an identity/canonicalization contract;
- weakening exact or independent scientific validation;
- raising the 256 MiB import budget to meet timing;
- retaining a compatibility checkpoint reader, selectable predecessor, hidden
  fallback, or dual product route;
- mutating source data or replacing an existing destination;
- an unbounded queue, decoder allocation, Z slab, temporary-object relation, or
  file-per-brick scratch layout; or
- materially broadening source-format, platform, hardware, or public-data
  scope.

Known implementation risks and required controls:

- **Source mutation race:** retain exact source-file provenance and a strong
  begin/end authority check; do not replace content verification with size or
  mtime alone.
- **Oversized native TIFF chunk:** reject through the typed capacity path unless
  the same approved path can stream it within budget; never allocate past the
  ledger or select a fallback.
- **External-memory reblocking:** preflight its worst-case bytes, cap checkpoint
  objects, checksum records, and clean only importer-owned stages.
- **Batched durability:** define the loss/recompute bound precisely and prove
  every crash boundary with fault injection.
- **Encoded pass-through:** preserve one codec authority and validate kind,
  length, checksum, order, and profile before shard assembly.
- **Parallel determinism:** keep one ordered commit owner, bounded reorder
  state, explicit cancellation generations, and byte-ledger admission.
- **Validation caching:** cache only immutable proved snapshots; never skip the
  independent semantic comparison.
- **Capability transfer:** within the cooperative local namespace threat model,
  fail closed on observed rename ambiguity, filesystem substitution, source
  drift, inventory drift, or snapshot mismatch. Return no open-ready capability
  and never select the normal external verifier as an automatic fallback. The
  contract explicitly excludes a hostile actor able to concurrently rename or
  unlink entries in the destination parent.
- **Performance miss:** do not remove safety checks or hide work from the
  primary clock. Reopen the architecture or threshold with measured stage
  evidence.

## Checkpoint, Rollback, And Branch Policy

- Begin implementation only after explicit owner approval from a clean named
  predecessor revision.
- Use short dependency-ordered packages. Preparatory code may exist only while
  unreachable; each activation package deletes its predecessor in the same
  accepted change.
- Rollback is a source revision revert or fix-forward. It is never an
  in-product fallback or retained compatibility path.
- An IP-02 checkpoint-schema cutover may invalidate incomplete predecessor
  checkpoints. The product must report that fact and offer explicit reset and
  restart; it must not guess or migrate checkpoint bytes.
- A failed package may clean only its owned temporary stage. It never deletes
  source microscopy data, an existing destination, or a successfully published
  package.
- Do not create a performance claim from a dirty worktree, debug build,
  mutable executable, mixed configuration, censored sample set, or stale
  report.

## Entry And Completion

Implementation entry requires:

- explicit owner approval of this handoff or a reviewed revision of it;
- a clean named predecessor commit and tree;
- the IP-00 field list, T2 geometry, clock boundaries, and sampling protocol
  declared here; IP-00 freezes the concrete evidence schema before IP-01; and
- confirmation that no concurrent change owns the importer, storage writer,
  source verification, or import product path.

The handoff is complete only when:

- IP-00 through IP-04 are accepted in order;
- every mandatory gate passes at one immutable final revision;
- the normal product workflow is product-validated on T5/HW-2;
- source nonmutation, bounded resources, restart, deterministic output,
  sharding, independent scientific validation, and atomic publication remain
  directly proved;
- the predecessor deletion audit finds no alternate importer, checkpoint,
  durability, publication, progress, or verification path;
- current-state and owning technical documentation state only the behavior
  actually proved; and
- the owner accepts reported skips, waivers, and remaining risks.

Completion claims must distinguish **implemented**, **automated-verified**, and
**product-validated**. Once current authorities record the accepted result,
delete this active handoff; Git history is its archive.
