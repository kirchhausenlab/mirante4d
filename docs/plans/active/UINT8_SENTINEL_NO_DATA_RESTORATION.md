# Uint8 Sentinel No-Data Restoration Plan

Status: ACTIVE — IMPLEMENTED; PRIVATE T5 FACT CUTOVER AND FINAL QUALIFICATION PENDING
Implementation authorization: OWNER APPROVED 2026-07-16
Last reviewed: 2026-07-16
Planning predecessor: clean `85350219efcc0c96b492f9a5029ba80752b49306`
Target sequence: ND-00, ND-01, ND-02, ND-03, then ND-04

## Purpose

The pre-cutover importer treated a reviewed uint8 sentinel as invalid only when a
source byte exactly equals that sentinel. It then constructs every coarse LOD
by copying one even-origin parent sample and its validity bit. The predecessor
importer instead added a one-voxel invalid guard at the base and every LOD and
formed coarse values from valid-only block means. The current behavior can
therefore expose a bright, non-sentinel boundary fringe produced next to a
`cval=255` region and alias that fringe into interrupted horizontal or vertical
lines at coarse LODs.

This plan restores the predecessor's reviewed uint8-sentinel semantics while
retaining the current bounded, cancellable, restartable, sharded importer and
canonical-zero representation for invalid samples. It optimizes the exact
policy through fused, validity-only halos rather than substituting a weaker
mask rule.

ND-00 through ND-03 are implemented in the current source. ND-04 remains open:
the public T2 workload qualified at interim clean revision `f73cb36`, and the
owner reports that the visible boundary defect is fixed on the affected private
dataset, but the private T5 expected-fact authority and final-revision
qualification have not yet been cut over. [Current
state](../../CURRENT_STATE.md) owns the implemented behavior, and [Testing and
evidence](../../TESTING.md) distinguishes the new diagnostic evidence from the
older immutable qualification.

## Selected Outcome

For every reviewed `U8Sentinel(s)` import, the sole product policy shall:

1. classify exact source bytes equal to `s` as source-invalid;
2. dilate source-invalidity by one voxel before storing LOD 0 validity;
3. form each coarse value as the half-up-rounded mean of valid contributors in
   its aligned factor-two parent block;
4. classify a coarse sample with no valid contributors as unsupported;
5. dilate unsupported coarse samples by one voxel at every LOD;
6. store zero for every final invalid sample while preserving valid zero as
   scientifically distinct from invalid; and
7. publish centered, axis-aware transforms appropriate for block means.

The existing factor-two pyramid shape sequence, scale count, chunk shapes, and
sharding remain unchanged; only the sentinel-bearing reduction, validity, and
corresponding transform semantics change.

The intended output is predecessor-equivalent validity and predecessor-
equivalent values at every final valid voxel. Byte-for-byte equality with an
old package is not a goal: the predecessor could retain a hidden sentinel or
other value in an invalid pixel, while the active target profile requires the
canonical stored value of an invalid integer sample to be zero.

## Exact Semantic Contract

Let `s` be the reviewed uint8 sentinel, `D_l` the in-bounds coordinate domain
at level `l`, `V_l` final validity, and `P_l` canonical uint8 values.

At LOD 0:

```text
source_valid(p) = source(p) != s
V_0(p) = AND source_valid(q)
         for every in-bounds q with Chebyshev distance(q, p) <= 1
P_0(p) = source(p) when V_0(p), otherwise 0
```

Out-of-dataset coordinates are ignored rather than treated as invalid. For a
2D level, the Z radius is zero. For a 3D level, the neighborhood is at most
`3 x 3 x 3`.

For every next LOD, let `B_l(p)` be the aligned parent block beginning at
`2 * p`, with extent two on each axis reduced at that step and extent one on
an unreduced axis. Blocks are clipped at odd dataset tails.

```text
support(p) = OR V_l(q) for q in B_l(p)
count(p)   = number of valid q in B_l(p)
sum(p)     = sum of P_l(q) for valid q in B_l(p)
mean(p)    = floor((sum(p) + count(p) / 2) / count(p)) when count(p) > 0
V_l+1(p)   = AND support(r)
             for every in-bounds r with Chebyshev distance(r, p) <= 1
P_l+1(p)   = mean(p) when V_l+1(p), otherwise 0
```

The operation order is normative: previous final validity, valid-only aligned
reduction, support classification, then child-level invalid dilation. A base-
only guard, point-selected values, all-parent-valid reduction, coverage
threshold, or renderer-only mask is not equivalent.

Sentinel equality is applied only to original source samples at LOD 0. A valid
coarse mean that numerically equals `s` remains valid; later levels consume
explicit prior validity and never reclassify a derived intensity by sentinel
equality. Timepoints and logical layers are classified, dilated, and reduced
independently, with no neighborhood crossing a time or layer boundary.

The predecessor reference is commit `61cd392`, especially
`crates/mirante4d-import/src/multiscale.rs:1083-1228` and
`:1295-1320`. Independent expected facts shall encode the contract above
rather than invoke that deleted implementation or copy production output.

## Private T5 Authority Correction

Owner review on 2026-07-16 confirmed that the pinned private T5 workload is the
affected dataset and is imported with an explicit uint8 sentinel. The earlier
ND-04 statement that T5 was no-sentinel was wrong. Its frozen scientific ID,
layer root, and seven per-scale digests describe the pre-restoration
exact-only/point-LOD output and cannot serve as regression facts for this
cutover.

Before final T5 qualification, an xtask-only source oracle shall independently
enumerate the pinned one-plane grayscale uint8 TIFF stack, decode source pixels
without invoking importer normalization or package output, and apply the exact
contract in this plan. It shall derive the base scientific identity and layer
root, every recursive scale digest, and centered transform facts directly from
source-derived value/validity records. The oracle, frozen private config, and
candidate package must agree three ways. Candidate package readback or a first
diagnostic run is never the source of expected facts.

The oracle keeps at most two create-new row-major scratch levels, never a full
level in RAM, and uses fixed plane/slab rings admitted below 256 MiB. It
preflights the constant two-file scratch bound, deletes parent state as each
child completes, and proves zero remnants after success, error, or
cancellation. Private paths, geometry, identities, and facts remain outside
the repository. The old private configuration commitment is rejected through
a new fact-authority/schema binding rather than accepted by compatibility.

## Non-Negotiable Invariants

1. Source microscopy data is never modified.
2. The selected 256 MiB import budget continues to cover every source buffer,
   dense and packed mask, codec allocation, descriptor list, worker result,
   queue, and reorder buffer.
3. Work remains bounded and cancellable across base production, scientific
   identity, every pyramid level, checkpointing, publication, and validation.
4. The production checkpoint remains one fixed two-file canonical cache plus
   one fixed four-file spool. No level-proportional scratch files or mask
   sidecars are introduced by the product design. The private qualification
   oracle's two external, create-new, self-cleaning level files are evidence
   work outside the importer/checkpoint authority.
5. Final package arrays remain sharded, and physical-object count remains
   independent of logical brick count.
6. Validity remains one shared scientific authority for import, storage,
   rendering, statistics, analysis, and scientific identity.
7. Every invalid integer sample is stored as canonical zero. Valid zero remains
   valid when its validity bit is set.
8. Scientific identity continues to include actual canonical base values and
   the effective dilated base validity. Writer/reader agreement is not the
   oracle.
9. Publication remains validated, create-only, and atomic. Incomplete output
   never appears complete.
10. One sentinel-policy implementation survives the hard cut. There is no
    exact-only compatibility branch, base-only fallback, environment selector,
    or predecessor checkpoint reader for sentinel imports.
11. Existing complete packages remain readable under their recorded arrays and
    transforms. They are not migrated or silently reinterpreted.
12. Public fixtures and documentation disclose no private dataset path,
    label, geometry, or scientific identity.
13. Private sentinel qualification facts come from the bounded source oracle,
    not from copying candidate package output into the accepted configuration.

## Scope

Allowed implementation scope:

- `mirante4d-import-pipeline`: policy modeling, base and scientific-tile halo
  reads, validity morphology, masked reduction, selective spool-component
  reads, byte admission, checkpoint binding, recipe construction, transforms,
  observability, and focused tests;
- `mirante4d-storage`: only narrow support or conformance changes required to
  validate already-supported explicit validity and nonzero centered
  transforms; no new package reader or storage layout;
- `mirante4d-ui-egui` and `mirante4d-app`: reviewed policy wording and normal
  product-validation projection, without a second import authority;
- `xtask` and generated public fixtures: independent semantic facts,
  per-scale readback, resource evidence, and qualification updates; and
- owning current-state, architecture, data-format, testing, development, and
  planning documentation when their facts actually change.

The semantic cutover is limited to imports with an explicit reviewed uint8
sentinel. Imports without that policy, including ordinary uint8, uint16, and
finite float32 sources, retain their current reduction, transform, identity,
and product behavior. Any proposal to restore predecessor mean pyramids for
those inputs is a separate scope decision.

## Non-Goals

- Adding sentinel ranges, fuzzy equality, `>= N` thresholds, or inferred
  validity from brightness.
- Adding a separate user-supplied validity-mask format or resampling API.
- Changing renderer sampling or hiding the defect only in the shader.
- Changing analysis rules to disagree with stored effective validity.
- Changing the active M4D storage profile, sharding, brick shape, or package
  reader compatibility surface.
- Migrating or rewriting already published packages.
- Adding a second full-scale support pass, new checkpoint files, or a
  level-proportional scratch authority without a separately approved revision
  of this plan.
- Raising memory, file-descriptor, object-count, or timing gates to make the
  implementation pass.
- Making a relative performance or tail-latency claim.

## Target Authority And Deletions

After ND-03, the recorded `U8Sentinel(s)` policy is the sole selector for the
contract in this plan. The cutover shall remove or make impossible:

- exact-equality-only validity as the complete sentinel policy;
- selected-even-parent validity and point values for sentinel-bearing LODs;
- zero-translation metadata for sentinel-bearing mean LODs;
- the ambiguous recipe description `sentinel-to-invalid` without its dilation,
  reduction, rounding, and dimensionality semantics;
- acceptance of a checkpoint bound to the old sentinel/point algorithm; and
- current T2 expected facts that freeze point sampling for a sentinel-bearing
  workload.

The point-decimation implementation may remain only as the sole recorded
policy for non-sentinel imports. Shared helpers must not permit a sentinel
import to select it through a hidden option or failure fallback.

Incomplete checkpoints from the predecessor algorithm are rejected through a
new plan/algorithm binding and require an explicit reset and restart. Source
files, complete packages, and existing destinations are never deleted by that
reset. No compatibility checkpoint reader or migration shim is added.

## Fused Bounded Implementation

### Base And Scientific Identity

Each sentinel-bearing base task reads its logical core plus a clipped
one-source-voxel halo from the canonical cache. It classifies exact sentinel
bytes over that window, applies the Chebyshev-radius-one invalid dilation,
crops to the core, zeros newly invalid core values, and encodes the final
pixel and validity components.

Scientific-identity tiles independently apply the same clamped halo policy to
canonical source data. They hash final canonical-zero values and dilated base
validity, so identity agrees with stored LOD 0 semantics without deriving its
answer from the writer's encoded output.

For the current chunk shapes, the logical input window grows from `64^3` to
at most `66^3` in 3D, approximately 9.7%, and from `256^2` to at most `258^2`
in 2D, approximately 1.6%. Edge clipping reduces those maxima.

### Coarse Levels

Each coarse target task remains one final work unit:

1. Read parent pixels and final validity for the ordinary aligned two-times
   core footprint.
2. Compute valid-only half-up-rounded means for core outputs.
3. Compute raw support for the target core plus one target-voxel halo.
4. Obtain that support halo from final parent validity extending two parent
   voxels beyond each side of the ordinary footprint, clipped only at the true
   dataset boundary.
5. Dilate child invalidity, crop final validity to the target core, zero newly
   invalid values, and encode once.

A one-parent-voxel halo is insufficient: one halo child support cell owns a
factor-two parent block. For a full 3D target chunk, the dense logical mask
window grows from `128^3` to at most `132^3`, also approximately 9.7%.

Chunk alignment can increase the referenced parent set from `2^3 = 8` to at
most `4^3 = 64` in 3D, or from four to sixteen in 2D. The extra halo-only
parents provide validity only. Their pixel components must never be read or
decoded.

The spool already records pixel and validity components separately. The
target reader shall expose bounded selective component reads and use packed
index facts to synthesize all-valid or all-invalid overlap without decoding a
validity payload. Mixed masks may decode one packed validity component at a
time and copy only required bits into the charged halo buffer. Pixel reads
remain limited to the ordinary core parent set.

Invalid dilation may use separable Boolean erosion of validity or equivalent
packed-bit operations, provided focused tests prove exact equality with the
normative neighborhood definition. Uniform all-valid and all-invalid
neighborhoods should have explicit fast paths. Optimization must not change
true-dataset-edge treatment, odd-tail clipping, 2D Z radius, or operation
order.

This fused design writes no temporary support level, creates no new durable
object, and preserves current work-unit and publication order. A two-pass
support/dilation design is a contingency requiring owner review because it
would add checkpoint/scratch, free-space, cancellation, durability, progress,
and recovery authorities.

### Transforms, Recipe, And Checkpoint Binding

For every mean LOD and axis `a`, let `F_l,a` be the cumulative factor on that
axis, doubling only at steps where that axis is reduced. The target transform
uses:

```text
scale_l,a       = base_spacing_a * F_l,a
translation_l,a = base_spacing_a * (F_l,a - 1) / 2
```

An axis that is not reduced retains factor one and zero translation. Tests
must cover 2D, a 3D Z axis that reaches length one before X/Y, and odd clipped
tails. The package format already supports explicit validity and nonzero OME
translations; the expected target does not require a new storage schema.

The import recipe shall name the sentinel classification, Chebyshev radius,
2D/3D rule, base-and-every-LOD application, valid-only mean, half-up rounding,
unsupported rule, and canonical-zero output. Its operation semantic version
and registry commitment change rather than silently redefining the existing
operation.

The checkpoint plan digest shall bind an explicit algorithm identifier. The
old exact-sentinel/point-decimation binding cannot resume after activation.
Scientific-content IDs change because the effective base values and validity
change; the existing scientific hash scheme need not change if conformance
review confirms that it already hashes those canonical facts. A proposal to
reinterpret an identity algorithm or profile schema is a stop condition.

## Work-Package Sequence

The sequence is dependency order, not a menu. Preparatory production code may
remain only test-scoped or unreachable without a runtime selector until the
single ND-03 activation. Combining ND-01 through ND-03 in one reviewed change
is preferable to leaving dormant production paths.

### ND-00 — Freeze Contract, Oracle, And Baseline

Implementation status: implemented and covered by independent focused facts.

Goal: freeze independent expected semantics and resource observations before
changing production output.

Required work:

- Encode the exact contract above in a small independent oracle that does not
  call production normalization, morphology, pyramid, transform, or package
  construction code.
- Add a generated boundary fixture containing exact sentinel exterior,
  adjacent non-sentinel bright values, interrupted horizontal and vertical
  runs, diagonal/curved edges, holes, thin data islands, and legitimate bright
  values away from invalidity.
- Freeze expected values, validity, transforms, and per-scale digests for 2D
  and 3D, odd shapes, every sentinel parity modulo `2^LOD`, chunk faces,
  edges, and corners.
- Record a diagnostic current T2 run and the affected private product symptom
  without publishing private facts. This is baseline context, not
  qualification.
- Freeze intended recipe and checkpoint algorithm identifiers before ND-03.

Exit proof:

- The independent oracle reproduces the predecessor's dedicated corner,
  dilation, and masked-mean facts without invoking predecessor or candidate
  code.
- The current production implementation demonstrably fails the new boundary
  expected facts for the known semantic reason.
- The owner accepts any revision to the semantic formulas before production
  work begins.

### ND-01 — Bounded Base And Morphology Kernels

Implementation status: implemented and covered by focused tests on the current
dirty worktree.

Goal: implement exact base/scientific halo semantics and reusable morphology
within explicit byte and cancellation bounds, without partially activating the
product policy.

Required work:

- Add clipped halo planning and region mapping for base chunks and scientific
  tiles.
- Implement exact source classification, 2D/3D invalid dilation, crop, and
  canonical-zero output.
- Implement a separable or packed-bit morphology candidate and compare it
  exhaustively with a direct neighborhood oracle on bounded shapes.
- Extend task-byte and preflight authorities for maximum halo buffers,
  descriptors, packed masks, codec scratch, queued results, and cancellation.
- Preserve source-generation checks and canonical-cache descriptor ownership.

Exit proof:

- Base values, final validity, statistics, and scientific tile facts match the
  independent oracle across chunk boundaries and dataset edges.
- Direct and optimized morphology are bit-identical in 2D and 3D.
- Capacity and cancellation failures remain typed and do not mutate source or
  publish output.

### ND-02 — Selective Validity Halo And Fused Coarse Candidate

Implementation status: implemented and covered by focused tests on the current
dirty worktree.

Goal: prove exact recursive LOD semantics without a temporary full-scale pass
or halo pixel amplification.

Required work:

- Add selective pixel or validity reads at the immutable spool descriptor
  boundary.
- Infer uniform mask regions from packed-index facts without payload decode.
- Assemble ordinary parent pixels separately from the expanded parent
  validity window.
- Implement valid-only half-up means, support for target-plus-one, child
  dilation, crop, and canonical-zero output in one charged task.
- Preserve deterministic ordered commit and checkpoint recovery across worker
  counts, cancellation, and resume.
- Add counters or structural assertions proving halo-only parent descriptors
  perform zero pixel component reads and decodes.

Exit proof:

- Every LOD value and validity bit matches the independent recursive oracle.
- One-chunk and multi-chunk partitions are identical, including odd tails and
  true dataset boundaries.
- Pixel component reads remain confined to the ordinary 8-parent/4-parent
  core; descriptor, mask, memory, queue, and codec maxima are admitted before
  work starts.
- The fixed six-file checkpoint and current work-unit count remain unchanged.

### ND-03 — Atomic Sentinel-Policy Cutover

Implementation status: implemented and covered by focused tests on the current
dirty worktree.

Goal: activate the complete policy and delete the old sentinel semantics in
one product revision.

Required work:

- Wire ND-01 and ND-02 into every reviewed uint8-sentinel base, identity, and
  pyramid route.
- Activate centered axis-aware transforms and the explicit recipe operation.
- Bump checkpoint plan/algorithm binding and reject incomplete predecessor
  checkpoints with an explicit reset path.
- Update reviewed UI wording so the selected policy states exact sentinel plus
  one-voxel invalid dilation at base and every LOD.
- Replace T2 sentinel expected facts and scale-digest scheme with independent
  restored-policy facts.
- Replace the current selected-sample sentinel tests rather than retaining a
  contradictory test authority beside the restored contract.
- Delete or make unreachable exact-only/point sentinel routes and audit for
  hidden selectors, base-only shortcuts, or renderer-only suppression.
- Prove non-sentinel inputs retain their frozen current values, validity,
  transforms, and scientific identities.

Exit proof:

- The normal importer has exactly one route for a recorded uint8 sentinel and
  one unchanged route for no-sentinel inputs.
- Staged validation, exact closure, scientific identity, package admission,
  and direct verified open all accept the restored output.
- Existing complete packages still open without migration or reinterpretation.
- The deletion audit finds no weaker sentinel fallback or compatible old
  checkpoint reader.

### ND-04 — Qualification, Product Validation, And Closeout

Implementation status: in progress. Clean revision `f73cb36` passed the public
T2 qualification and the complete repository/local automated set. The private
T5 source-oracle fact cutover and qualification at the final oracle-bearing
revision remain.

Goal: prove scientific correctness, bounded performance, and the visible
product outcome on public and relevant private data before closing the plan.

Required work:

- Run the complete functional and structural check set at one clean immutable
  revision.
- Run five independent release T2 sessions on the accepted HW-2/ext4 profile
  under the existing absolute gate and new independent sentinel facts.
- Replace the affected sentinel-bearing T5 workload's pre-restoration semantic
  facts through two identical bounded source-oracle runs, hard-cut the private
  config/fact-authority commitment, and compare oracle, config, and diagnostic
  package readback before qualification.
- Re-run both five-session T2 and three-session T5 qualification at the final
  clean immutable oracle-bearing revision. Preserve all existing absolute
  timing, memory, RSS, descriptor, checkpoint, source, publication, and normal-
  product gates.
- Exercise the normal native import and rendering workflow on the affected
  sentinel-bearing dataset on the relevant real display and hardware. Inspect
  LOD 0 and every coarse LOD at the data/no-data boundary and retain private
  raw evidence outside the repository.
- Update owning current documents only with behavior and evidence proved at
  the accepted revision, then delete this active plan; Git history is its
  archive.

Exit proof:

- Every mandatory gate below passes with revision, executable, workload,
  sampling, hardware, filesystem, failures, skips, waivers, and remaining
  risks reported.
- The previously visible bright interrupted boundary fringe is absent at every
  exercised LOD, and package readback independently matches expected validity
  and values.
- The owner accepts remaining risks and the final deletion audit.

## Mandatory Gates

| Gate | Evidence | Required result |
| --- | --- | --- |
| Base semantics | Independent 2D/3D fixtures | Exact sentinel classification, one-voxel invalid dilation, canonical-zero invalid values, and legitimate bright values away from invalidity match frozen facts |
| Recursive LOD semantics | Independent oracle at every scale | Valid-only half-up means, no-support invalidity, per-LOD dilation, odd-tail clipping, and 2D Z behavior match every value and validity bit |
| Partition independence | One-chunk versus tiled runs | Identical canonical values, validity, transforms, statistics, and scientific identity across faces, edges, corners, and worker counts |
| Transform correctness | Independent physical-coordinate facts | Axis-aware factors and centered translations match the mean footprint at every LOD |
| Scientific identity | Independent scientific reader/facts | Dilated base validity and canonical values produce the expected scientific ID and layer roots |
| Private T5 source oracle | Two fresh source-derived oracle runs plus package comparison | Exact agreement for scientific ID, layer root, every scale digest, scale shape/count, and centered transform; no candidate-output bootstrap |
| Canonical invalid representation | Full package readback | Every invalid integer sample is zero; valid zero remains distinguishable |
| Sentinel-disabled regression | Frozen uint8/uint16/float32 facts | Non-sentinel values, validity, transforms, scale digests, and scientific identity remain unchanged |
| Halo pixel isolation | Structural test and counters | Halo-only parents cause zero pixel component reads/decodes; core pixel parent fan-in remains at most eight in 3D or four in 2D |
| Bounded halo authority | Preflight, pressure tests, and receipts | Parent descriptors are at most 64 in 3D or 16 in 2D; every dense/packed mask and sequential decode buffer is charged before admission |
| Working memory | Pressure tests and every timed run | Import-ledger peak remains at most 256 MiB |
| External memory | Every timed T2 and required T5 run | Peak RSS delta remains at most 384 MiB and reconciles with ledger-external overhead |
| Files and temporary state | Pressure tests and every timed run | At most 64 import-owned file descriptors, exactly the current six checkpoint files, and no new level- or chunk-proportional physical objects |
| Private oracle resources | Preflight, receipt, and cleanup audit | At most 256 MiB RAM, two level scratch files, bounded descriptors, admitted free space, and zero remnants before timed product samples |
| Timing | Five T2 sessions and three T5 sessions | T2 median remains at most 60 seconds; T5 median remains at most 15 minutes |
| Determinism and resume | Repeated fresh/resumed imports | Same-revision exact package and scientific IDs agree; resumed output equals fresh output; old checkpoints reject explicitly |
| Source and publication safety | Fixtures, mutation tests, and product runs | Source closure is unchanged; cancellation never publishes; validated output publishes create-only and opens through the existing verified capability route |
| Visible product result | Normal native real-display workflow | No bright no-data fringe at LOD 0 or any exercised coarse LOD; no renderer fallback or repeated product error |

The performance gates are absolute for the existing declared workloads. This
plan makes no relative speedup claim. A diagnostic fused-halo benchmark may
guide optimization but cannot replace correctness or qualification evidence.

## Verification

Focused automated coverage shall include:

- the predecessor's `3 x 3 x 3` corner fact: one corner sentinel produces
  eight invalid and nineteen valid voxels after base dilation, plus the 2D
  corner fact of four invalid voxels;
- direct-versus-separable morphology for all bounded masks through useful
  exhaustive dimensions, plus randomized larger masks;
- exact sentinel values, adjacent `253/254` values for sentinel `255`, and the
  same bright values sufficiently far from invalidity;
- sentinel values other than `255`, including sentinel zero; sentinel disabled;
  all-valid, all-invalid, mixed, sparse, contiguous, and interrupted masks;
- 2D and 3D, all sentinel parities, odd dimensions, short axes, dataset edges,
  chunk faces/edges/corners, shard boundaries, and every generated LOD;
- valid-only rounding, partially supported blocks, unsupported blocks, holes,
  thin structures, axes that stop reducing, and a valid coarse mean numerically
  equal to the source sentinel;
- multiple timepoints and logical layers proving that no validity neighborhood
  crosses their boundaries;
- one-chunk/tiled equality, serial/parallel equality, cancellation, resume,
  truncated/corrupt/wrong-plan checkpoints, and deterministic publication;
- scientific statistics and analysis excluding final invalidity; and
- existing complete-package reads plus frozen non-sentinel import behavior.

Required repository and local checks at the final affected revision include:

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
cargo run --release -p xtask -- import-performance-t2 \
  --samples 5 --scratch <owner-qualified-ext4-scratch> \
  --qualification-profile <owner-pinned-profile> \
  --cache-condition warm --competing-activity none
```

At clean revision `f73cb36`, both fixture validators, formatting, qualifying
`verify-pr` (904 passed, three skipped), the qualifying six-phase local format
lifecycle, and both E1 product-automation scenarios passed. The five T2 release
sessions were 13.654825657, 14.477087517, 14.033167153, 16.611035435, and
13.707822937 seconds, for a 14.033167153-second median against the 60-second
gate. Every runtime/resource gate passed, source inventory was unchanged, and
package/scientific identities were deterministic. This interim qualification
does not remove the requirement to rerun T2 at the final oracle-bearing
revision.

Also run the owner-pinned private T5 command from
[Testing and evidence](../../TESTING.md). Private source, package, screenshots,
paths, geometry, identities, and raw reports remain outside the repository.

## Risks And Stop Conditions

Stop implementation and return for owner review before:

- weakening the exact semantic contract to meet performance;
- adding threshold/range inference, base-only dilation, point-selected
  sentinel LODs, all-parent-valid reduction, or renderer-only suppression;
- adding a second full-level pass, new scratch/checkpoint files, a validity
  sidecar hierarchy, or a compatibility checkpoint reader to the production
  importer; the explicitly bounded two-file qualification oracle is not a
  product/checkpoint path;
- changing the M4D storage profile or scientific identity algorithm rather
  than changing the canonical content and recipe;
- broadening mean reduction to non-sentinel datasets;
- raising the 256 MiB ledger, 384 MiB RSS, 64-file, physical-object, T2, or T5
  gate;
- accepting halo pixel reads, an uncharged 64-parent descriptor/mask window,
  an unbounded cache, or work proportional to full dataset dimensions in RAM;
  or
- weakening independent staged validation, source mutation detection,
  create-only publication, or verified capability transfer.

Known risks and controls:

- **Halo off by one:** child support halo one maps to parent halo two. Prove
  faces, edges, corners, odd tails, and every parity against the oracle.
- **Chunk-boundary erosion:** chunk boundaries are never dataset boundaries.
  Only true global out-of-bounds neighbors are ignored.
- **Decode amplification:** enumerate up to 64/16 parent descriptors, but read
  pixels only for the core and skip uniform validity payloads. Count and
  benchmark component reads.
- **Memory under concurrency:** charge expanded masks, sequential decoded
  components, descriptor vectors, encoded results, and codec scratch in both
  preflight and worker policy before increasing parallelism.
- **Scientific-ID divergence:** base production and independent scientific
  tiling use the same normative policy through separately exercised paths.
- **Private-fact self-blessing:** the predecessor T5 bootstrap copied candidate
  package facts. Replace it with two stable source-oracle runs and require
  oracle/config/package agreement before pinning the new opaque commitment.
- **Spatial shift:** mean LODs require centered axis-aware transforms; do not
  retain point-sampling translation.
- **Checkpoint ambiguity:** bind the algorithm version and reject old durable
  prefixes; never infer which semantics produced them.
- **Fast-path drift:** all-valid/all-invalid and packed/separable paths must be
  bit-identical to the direct oracle.
- **Performance miss:** preserve the policy and collect stage/component
  evidence. A two-pass design or semantic alternative requires a revised,
  owner-approved plan.
- **Residual real-data fringe:** if oracle-conformant one-voxel dilation still
  exposes a wider upstream interpolation band, stop and review that distinct
  source/preprocessing policy. Do not introduce an unrecorded threshold or
  silently enlarge the guard.

## Entry, Rollback, And Completion

Implementation entry requires:

- explicit owner approval of this plan or an approved revision of it;
- a clean named predecessor commit and tree;
- ND-00 independent facts and identifiers frozen before production activation;
  and
- confirmation that no concurrent work owns importer semantics, recipe,
  transforms, checkpoint binding, or import qualification.

Rollback is a source revision revert or fix-forward before publication. It is
never a runtime fallback. A failed import may clean only its owned temporary
stage and explicitly reset an incompatible incomplete checkpoint. It never
deletes source data, an existing destination, or a complete package.

The plan is complete only when:

- ND-00 through ND-04 are accepted in order;
- the restored policy is the sole sentinel authority and the deletion audit
  finds no weaker alternate;
- independent values, validity, transforms, scientific identity, and package
  readback pass;
- bounded memory, descriptors, checkpoint files, temporary state,
  cancellation, deterministic resume, sharding, validation, and publication
  remain directly proved;
- T2 and T5 gates pass at one clean immutable revision;
- the normal product is validated on the relevant sentinel-bearing dataset and
  real display at every LOD; and
- current-state and owning technical documentation record only proved facts,
  reported skips/waivers/risks are accepted, and this active plan is deleted.

Completion reports must distinguish **implemented**, **automated-verified**,
and **product-validated**. Git history is the archive after closeout.
