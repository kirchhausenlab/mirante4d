# Foundation Refactor — Verification, Evidence, And Performance Brief

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-11
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: D-022/D-023 verification mechanics, fixtures/oracles, E0-E4, performance, and failure semantics

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

WP-14 uses only the practical acceptance boundary in the technical cutover
package: ordinary public checks, one clean-clone rehearsal, the existing local
Linux packager, and one small-fixture packaged viewer exercise at 1280x720 plus
1920x1080. The broader evidence-set, private-data, performance, coverage, fuzz,
mutation, and external-E4 designs below are not foundation-exit requirements
and remain unactivated future options.

The canonical trusted-local, public-runner, branch-protection, and zero-cost rollout sequence is owned by the parent handoff and referenced as `CI-001` through `CI-005`.

## Verification Architecture

| Lane | Purpose | Target execution | Normal trigger |
| --- | --- | --- | --- |
| Static | Formatting, clippy, dependency/license, architecture, docs/schema checks | Public hosted CPU or local | Every PR/push |
| Unit/property | Geometry, state transitions, policies, codecs, scheduling math | Public hosted CPU or local | Every PR/push |
| Component/contract | Format, storage, cancellation, scheduler, CPU oracle, import corpus | Public hosted CPU or local | Every PR/push |
| UI | Pure egui semantic/event tests and selected stable snapshots | Public hosted CPU or local | Every PR/push |
| GPU | Full explicit GPU targets, pixel/resource lifecycle, validation errors | Trusted local `HW-2` clean worktree | Trusted changes/main/milestone |
| Product E2E | Packaged binary, real input, mapped-window capture, save/reopen | Hosted E2 package/capability checks plus trusted local GPU E3 and real-display E4 | Main/milestone |
| Package/install | Build, install, launch, uninstallation/layout, artifact metadata | Public hosted platform or trusted signing host | Main/release |
| Nightly/deep | Coverage, seeded fuzz shards, rotating mutation shards, medium pressure/stress | Public standard hosted plus trusted local execution | Approved rotation on a trusted revision |
| Format lifecycle | Current stable/experimental acceptance and any separately approved converter | Public hosted CPU plus public fixtures | Format change/release |
| Performance/stress | Repeated latency, throughput, RSS/VRAM, pressure, cancellation, soak | Fixed trusted hardware | Scheduled/milestone |
| Scientific acceptance | Independent goldens and representative public/private real data | Trusted hardware plus human review where required | Milestone/release |
| Public-data reproducibility | Download, verify, derive, open, and reproduce expected facts | Clean environment and separately approved trusted execution | Dataset release/release candidate |

WP-03 closed the `PUB-004` assignments `DEP-BLOCK-001` through
`DEP-BLOCK-005` through its bound full-source audit and exact-candidate
advisory result. The accepted audit also names `VER-BLOCK-001` (the
viewport-capture report mismatch), `VER-BLOCK-002` (the broken primary fast
gate), `VER-BLOCK-003` (stale external evidence identity), and
`VER-BLOCK-004` (the unsuitable verification topology) as WP-06 work. WP-06A
closed all four by correction or deletion; a green schema or report wrapper
still cannot override a native process result.

The installed public pull-request target is p95 below ten minutes with a
fifteen-minute ceiling. Its accepted twenty-attempt cache-free Main window met
those bounds; [current state](../../../CURRENT_STATE.md) owns the measured
result. Fast profiles cannot generate large volumes or open a product window.
Per-test budgets are semantic/profile classifications measured on the named
reference environment, not a universal two-second cutoff applied to noisy
shared runners.

### CI Cost And Trust Topology

WP-04 completed the repository-publication requirements:

- hard-stop paid GitHub Actions spending;
- run the temporary fast gate locally;
- use trusted local execution outside GitHub Actions where useful;
- keep every predecessor hosted workflow and repository Actions surface out of
  the public root;
- keep heavy real-data evidence local.

In the now-public repository:

- use free standard GitHub-hosted runners for untrusted PR CPU/platform work;
- keep persistent GPU, performance, and real-data machines out of the public
  repository's runner pool;
- use minimal permissions and pinned action revisions;
- upload no Actions artifacts initially; a separate reviewed activation after
  WP-06 may permit only the bounded, sanitized diagnostic bundles in CI-003;
- keep the authoritative commands repository-owned and runnable without
  GitHub;
- use a final same-revision aggregation job only for evidence that genuinely
  needs aggregation.

Trusted GPU/performance/real-data execution uses a repository-owned local
command against a maintainer-selected immutable commit, never an untrusted PR
head. Use a clean worktree, read-only datasets, isolated scratch, no write
credential in the tested process, no untrusted cache, and an explicit network
policy. Maintainer approval alone is not isolation; any future remote executor
requires a separate ephemeral design approval.

If the separate post-WP-06 activation is approved, diagnostic artifacts are
short-lived, bounded, and sensitivity-reviewed. Durable release evidence,
fixture manifests, SBOMs, provenance, public-data validation, and signed
evidence-set manifests use a separate retention/publication policy.

### Evidence Set Contract

Each retained lane output records:

- schema/version and requirement IDs;
- exact commit and source-tree/dirty state;
- exact command, profile, scenario/tool versions, start/end timestamps, and
  result reason including failed/skipped/unsupported/cancelled semantics;
- CI run, job, attempt, or an explicit local-diagnostic identity;
- build profile, executable/package digest, toolchain, OS/runner image, and
  GPU/driver/hardware where relevant;
- fixture/dataset IDs, content digests, and sensitivity class;
- artifact digests and retention class;
- waiver ID, approver, scope, alternate proof, issue owner, and expiry where
  applicable.

The native process/CI result is authoritative. A report cannot bless a failed
or missing job. Dirty-worktree output remains useful diagnostic evidence but is
ineligible for work-package, milestone, or release closure. Release closure uses
an explicit composite evidence-set manifest that binds scheduled trusted lanes
to the exact clean committed candidate and freshness window; it never
rediscovers proof by scanning persistent paths.

### Fixture And Dataset Tiers

| Tier | Contents | Repository/hosting | Purpose |
| --- | --- | --- | --- |
| T0 pure builders | In-memory facts and state builders | Source repository | Unit and UI tests |
| T1 independent goldens | Tiny native/TIFF/corrupt fixtures with external expected facts | Committed or content-addressed guaranteed public retrieval | Format/import/scientific conformance |
| T2 generated deterministic | Small reproducible packages generated once per suite | Suite-local temporary output; optional bounded acceleration cache only if separately approved | Component and CPU/GPU correctness |
| T3 public medium | Versioned openly licensed microscopy subsets | Public dataset hosting | Integration, packaged E2E, representative correctness |
| T4 public large | Approved representative large release with checksums and citation | Public large-data hosting | Performance, stress, public reproducibility |
| T5 restricted pre-public | Data not yet cleared for publication | Trusted local storage only | Temporary validation until replaced by T3/T4 |

Writer/reader round trips are not independent conformance evidence by
themselves. T1 bytes or expected facts come from an independent specification,
tool, or oracle. Each package reuses the smallest accepted fixture set that
covers its changed behavior and adds a fixture only for a demonstrated gap; no
Cartesian validation matrix is required.

The fixture registry records ID/version, digest algorithm/value, generator and
tool versions, provenance, license, size, expected-fact version, roles, owner,
and parent public-release identity. T2 cache contents are never correctness
authority. Public docs identify T5 data only by opaque restricted ID and
sensitivity class; a private resolver maps IDs to paths. T5 reports,
screenshots, metadata, and logs require sanitization and can never be the sole
proof of a public capability claim.

OA-003-approved PH-00 public-safe T5 role set (approved 2026-07-10):

| Opaque ID | Qualification role | Public sensitivity class | Public boundary |
| --- | --- | --- | --- |
| `T5-QUAL-001` | Spatial-extreme open/navigation/render/resource workflow | `internal_lab_data` | No source name, path, digest, metadata, screenshot, or scientific content enters the public tree |
| `T5-QUAL-002` | Temporal-extreme open/playback/cancellation/resource workflow | `internal_lab_data` | Same; exact identity exists only in the private resolver/evidence manifest |
| `T5-QUAL-003` | Ordinary/multichannel scientific and HW-2 calibration workflow | `internal_lab_data` | Same; it cannot become a public T1/T3/T4 fixture by relabeling |

The role set authorizes no access, copy, mutation, upload, or public claim. Any
future private evidence may resolve exact identities only after the owner
grants access for that run. Private qualification is not part of WP-14.

## Approved Verification And Zero-Cost CI Boundary

Status: OWNER-APPROVED TARGET POLICY under OD-022. D-022 and D-023 are
resolved. This approval fixes the target semantics but authorizes no test
rewrite, workflow change, hosted run, runner registration, branch rule, fixture
generation, or production implementation outside its owning work-package entry
brief.

### D-022 Selected Verification Topology

Select structural leaf lanes rather than one recursive “fast/full/nightly”
stack. Each test case belongs to exactly one normal lane and one primary owner,
and declares one or more requirement/assertion IDs. An aggregate command or
workflow may schedule leaf lanes, but no leaf invokes another aggregate and no
CI job reruns a completed leaf merely to create a different report.

The machine-readable verification registry owns lane ID, case selector,
requirements, crate/test-target owner, fixture tier, runner capability,
timeouts, evidence schema, and retention. Selection must use crate/test-target
structure and registry metadata, not hard-coded Rust test-name arrays. A normal
case cannot hide behind `#[ignore]`; GPU/display/special cases live in explicit
targets that their owning lane enumerates completely.

The registry is the single source for a checked-in generated Nextest
configuration and structural selector manifest. `verification-audit`
has two phases: `PR / policy` regenerates schema/config/selectors and requires a
clean diff without compiling test binaries; `PR / rust`, after its sole test-
binary build, reuses that target directory for Nextest discovery and proves
that every discovered case is assigned exactly once and every selector
resolves. A separate rustdoc inventory audits registered doctests because
Nextest cannot discover them. Hand-maintained registry, target-layout, and
Nextest allowlists may not coexist.

#### Required Public Pull-Request Lanes

The six explicit leaves are independently selectable through
`cargo xtask verify-leaf <policy|lint|unit|contract|ui|doctest>`. CI uses only
two required jobs so the unit, contract, and UI test binaries are built once
rather than in three fresh jobs:

| Stable check | Contents | Runner | p95 target / ceiling |
| --- | --- | --- | --- |
| `PR / policy` | Format; docs/schema/links; dependency/API/side-effect/resource/deletion graph; license/advisory/source policy; workflow security | Standard public `ubuntu-24.04` | `4 min / 6 min` |
| `PR / rust` | `lint`; one test-binary build and no-fail-fast categorized Nextest union for `unit`/`contract`/`ui`; then `doctest` | Standard public `ubuntu-24.04` | critical path `<10 min / 15 min` |

The two jobs start concurrently. The PR critical path is their maximum, not the
sum of their ceilings.

Every standalone leaf has an aggregate subprocess ceiling, including any
compilation that invocation needs: `policy 6 min`, `lint 8 min`, `unit 12 min`,
`contract 12 min`, `ui 12 min`, and `doctest 4 min`. Inside the shared
`PR / rust` job, lint, test-build, categorized-test-union, and doctest phases
also have registry-owned ceilings of `8`, `10`, `5`, and `4` minutes; the outer
fifteen-minute job deadline always wins. These are failure ceilings, not targets
or additive time allowances.

The unit/contract/UI results are separate sections in the JUnit/evidence output
even though one runner and test-binary build produces them. `policy`, `lint`,
and `doctest` own distinct results; no claim is made that Clippy and test
profiles share compiled artifacts. `cargo xtask verify-pr` calls the same leaf
functions in-process and one categorized test union; it does not shell
recursively through the public leaf commands. It records every feasible leaf
before returning the aggregate result: a lint or individual-test failure cannot
suppress unrelated tests, while an actual compilation failure may block only
targets that could not be built.

The WP-04 cutover installed one explicitly transitional
`Bootstrap / required` check that ran the honest WP-01 bridge on a standard
public Linux runner. WP-06 retained it until both target checks completed their
twenty-run shadow window and the required-check replacement was read back.
Repository rules now require `PR / policy` and `PR / rust`, and the separate
WP-06C checkpoint removes the bridge without leaving public `main` ungated.

Semantic per-case limits are:

| Profile | Warn | Terminate |
| --- | ---: | ---: |
| Pure unit/property | `500 ms` | `2 s` |
| Component/contract | `5 s` | `20 s` |
| UI semantic | `2 s` | `10 s` |
| Deterministic CPU/headless UI snapshot | `10 s` | `30 s` |
| Trusted GPU | `20 s` | `60 s` |
| Generated packaged-product scenario | — | `5 min` |

One exact component/contract exception is registered for
`actor::tests::purge_fresh_process_kill_and_retry_matrix`: it keeps the `5 s`
warning and terminates at `40 s`. That test deliberately covers 16 real
`SIGKILL`/fresh-process recovery points by spawning 32 child processes and
performing filesystem syncs. No other contract case inherits this ceiling.

Nextest does not run doctests, so `doctest` has a separate aggregate warning at
`60 s` and hard subprocess ceiling at `120 s`; any doctest needing meaningful
per-case runtime control becomes a normal unit/contract case. Property cases
declare fixed seed, case count, shrink/replay policy, and record the failure
seed. Ambient RNG, wall-clock time, locale, home directory, or test-order state
cannot determine a PR result.

The uncached public-PR critical path must achieve p95 below ten minutes over at
least twenty qualifying runs. The fifteen-minute value is a failure ceiling,
not an acceptable target. The combined warm local pre-push profile on `HW-2`
has the same ten-minute p95 target. If a lane misses its budget, first remove
duplication, use deterministic single-thread test executors, inject tiny render
targets, generate fixtures once, split ownership, or move semantically slow
cases to the correct deep lane. The budget cannot be raised silently.

PR visual snapshots must use a deterministic CPU/headless rasterizer. Any case
that initializes WGPU, discovers an adapter, or depends on a compositor moves
to the trusted GPU or packaged E3/E4 lane; semantic UI stays in `PR / rust`.

Required PR lanes never use path filtering, `continue-on-error`, retries,
private data, GPU discovery, mapped/native windows, large generated data,
performance timing, or a mutable application test service. Pinned tool,
dependency, and advisory retrieval is infrastructure, not a test oracle. The
lanes always conclude pass or fail. GitHub can treat a skipped/neutral required
check as acceptable, so workflow-policy generation forbids every conditional or
dependency route that can skip a required job, and the aggregate command exits
nonzero unless every registered required leaf completed and passed. At the
evidence layer, `skipped`, `missing`, `cancelled`, `unsupported`, neutral, and
stale results never satisfy a requirement. This compensates explicitly for the
[documented required-check
semantics](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches)
rather than assuming the hosting UI fails closed.

The same two checks and six leaves run on exact protected-`main` revisions under
unique `Main / policy` and `Main / rust` job names. Additional non-PR lanes are:

| Lane | Trigger and execution | Role |
| --- | --- | --- |
| `Main / package-capability` | One standard public Linux job/build, protected `main` | Build/install/layout/headless smoke plus black-box unsupported-GPU diagnosis against the same package; no viewer E3/product-open claim |
| `Deep / code-coverage` | Unactivated future option | Instrumented code-gap discovery only if a later concrete need justifies it |
| `Deep / fuzz-N` | Manual/rotating standard public Linux shard | Seeded parser/state/crash discovery with corpus promotion |
| `Deep / mutation-N` | Manual/rotating standard public Linux shard | Requirement-test strength; bounded owner-specific shards |
| `Portability / windows` and `Portability / macos` | Manual or explicitly approved portability change | Non-required compile/portable-contract evidence; no support claim |
| `Format / lifecycle` | Target-format change and release candidate; public standard CPU plus T1 | Current profile/converter-if-approved acceptance and rejection matrix |
| `Scientific / acceptance` | Applicable milestone/release; local `HW-2` plus T1/T5 | Independent scientific facts and reviewed tolerance evidence |
| Trusted GPU/E3/E4/product/performance | Manual local `HW-2` clean worktree | Required by applicable work-package/milestone claims |
| Release | Accepted protected tag on standard public Linux and/or local `HW-2` | Package, SBOM, provenance, checksum, and evidence-set publication |

Total lane budgets include setup and apply per scenario/shard where the row is
parameterized:

| Lane/profile | p95 target | Hard ceiling |
| --- | ---: | ---: |
| Main package plus unsupported-GPU capability flow | `12 min` | `20 min` |
| Trusted GPU correctness | `10 min` | `15 min` |
| Trusted small-fixture E3 | `10 min` | `15 min` |
| Portability per OS | `8 min` | `12 min` |
| Format lifecycle | `10 min` | `15 min` |
| Code coverage | `12 min` | `20 min` |
| Each fuzz shard | `5 min` fuzzing | `8 min` total |
| Rotating mutation shard | `15 min` | `25 min` |
| Medium pressure/stress scenario | `20 min` | `30 min` |
| Fixed-HW performance scenario | `30 min` | `45 min` |
| Small-fixture `HW-2` E4 | `10 min` | `15 min` |
| DS-1/DS-2 E4 scenario | `15 min` | `20 min` |
| DS-3/DS-4 E4/performance scenario | `30 min` | `45 min` |
| Scientific T1 acceptance | `10 min` | `15 min` |
| Scientific T5 acceptance scenario | `30 min` | `45 min` |
| Release assembly/publication | `20 min` | `30 min` |

Calibration uses a fixed consecutive window of at least twenty valid
non-infrastructure attempts on clean revisions and reports cold and warm/cache
state separately. Functional failures and timeout-censored attempts remain in
the duration window; excluding them would create survivor bias. Hard ceilings
apply from the first implementation, and a censored ceiling event is a
violation. Percentile enforcement begins when the fixed window is complete.
The WP-06 window completed with twenty valid first-attempt successes, no
infrastructure invalidations, caches, artifacts, or censored timeouts.

No cron is enabled until the corresponding lane has a measured duration,
output size, owner, and approved cadence. Main evidence runs are not cancelled
once started; superseded PR runs use per-PR concurrency with
`cancel-in-progress: true`.

### D-023 Selected Fixture, Oracle, E2E, And Evidence Policy

#### Independent Portable Corpus

T1 is the portable conformance authority. Independence is algorithmic and
dependency-enforced: a producer, reader, expected-fact oracle, or validator may
share released normative schemas and plain value types, but cannot import,
invoke, copy, generate from, or reuse Mirante production format/storage/
identity/canonicalization/Merkle/project/persistence/transform/statistics/
renderer/import/analysis algorithms or codecs. For authoritative facts, the
fixture-byte producer, scientific expected-fact oracle, and independent reader
must be pairwise independent implementation lineages; the producer cannot also
compute the expected logical digests/statistics that certify its bytes. Selected
critical headers/identities use explicitly hand-derived vectors as an additional
check, and a hand-derived vector may serve as its own declared fact source.

The foundation-release corpus is synthetic/rights-cleared and stored as a few
deterministic SHA-256-addressed archives rather than exploded directory trees.
Across the corpus, compressed bytes are at most `64 MiB`, unpacked regular-file
bytes at most `512 MiB`, and logical voxel bytes at most `192 MiB`. Each archive
has at most `256` files, `64` directories, depth `8`, fan-out `64`, path length
`240` bytes, individual file size `128 MiB`, and compression ratio `16:1`.
Before writing, extraction rejects absolute/parent paths, symlinks, hardlinks,
devices, duplicate paths, case-fold collisions, Unicode-normalization
collisions, and any declared or streamed limit violation. Generator source and
immutable tool/container digests, provenance/license, reviewer, source/archive/
tree digests, expected-fact version, and exact unpacked limits are recorded.
Tests unpack only after validation into isolated temporary directories.
These are outer ceilings for later target-format T1 archives. The WP-03 public
source-fixture archive uses the much smaller 32-file/8-directory/2-MiB limits
fixed in its checked source-fixture manifest schema; it cannot borrow the
larger ceilings here.

The registry has three non-overlapping states:

1. WP-03 source-publication assets contain rights-cleared TIFF/scientific facts
   and make no target-M4D conformance claim.
2. WP-06 bootstrap/current-format vectors are explicitly experimental and
   expire with mandatory deletion at WP-10C.
3. WP-10A target-profile vectors become the sole candidate-format authority
   only after the schema and external-reader claim are frozen.

The completed WP-04 public cutover therefore makes no target-format conformance
claim.

The minimum positive DS-0 matrix is:

1. a 2D `uint8`, `1t x 1c` sparse-signal case with valid zeros and enough
   non-fill data to cross multiple outer shards;
2. a 3D `uint16`, `3t x 2c` anisotropic, non-identity-transform,
   non-divisible-boundary multiscale case; and
3. a 3D finite-`float32`, `1t x 4c` case covering positive/negative signed zero,
   subnormals, normal extrema, rounding boundaries, explicit invalid-value
   rejection, validity distinctions, units, and channel ordering.

Independent base TIFF/OME-TIFF bundles cover each accepted source layout,
dtype, axis/calibration case, strips/tiles where supported, and ambiguous
grouping rejection. Compact mutation manifests derive negative cases from base
archives without duplicating trees: missing/truncated/bit-flipped shards, bad
index/checksum, contradictory axes/transforms, unsupported codec/dtype/layout,
non-finite float, and incomplete/ambiguous TIFF groups.

Expected facts include full logical-value digests, selected voxel/value facts,
validity, axes, transforms/units, scale rules, statistics, scientific identity,
exact package/tree identity, and physical layout. The required interoperability
matrix is external native bytes to Mirante reader; Mirante output to pinned
released OME/Zarr schema artifacts plus an independent reader; external
TIFF/OME-TIFF to the
Mirante importer; and Mirante scientific results to independent analytic/
high-precision expected facts. Schema validation covers structure only, never
pixels, transforms, M4D extensions, identities, or sharding behavior. Before
WP-10A freezes the candidate profile, the distinct external reader must actually
read the selected Zarr-v3 sharding/codec subset or the interoperability claim is
narrowed explicitly. Mirante writer/reader round trips remain component
symmetry evidence only.

Independence also applies outside the dataset reader:

- D-009 canonical encodings, Merkle nodes, and every typed identity family use
  fixed byte/SHA-256 vectors produced by a separate implementation. Metamorphic
  pairs cover equal science after recompression/resharding, physical channel
  reordering, and validity-representation changes; package-ID change with equal
  scientific ID; and one-bit value/validity/transform changes that must change
  scientific ID;
- D-010 has independently produced canonical envelope/ref/generation/object
  bytes/digests, valid and corrupt graphs, expected recovery candidate sets,
  project identity/rebinding facts, and an independent read-only validator.
  The store is separately exercised in subprocesses with kill points after
  every durable step plus short-write, ENOSPC, corrupt-ref/object, stale-parent,
  concurrent-writer, directory-sync, interrupted-GC, and supported-filesystem
  cases;
- selected MIP/DVR/ISO, transform, validity, and compositing cases use analytic
  expected pixels or a separately implemented oracle, not only CPU/GPU paths
  sharing the same math; and
- retained measurements/analysis operations use hand-derived rational/high-
  precision expected facts. Each operation fixes accumulation precision/order,
  mask semantics, variance/percentile definition, exact/approximate tolerance,
  and artifact/provenance expectations.

Authoritative archive, generator, schema digest, expected fact, oracle
algorithm, harness assertion, or tolerance changes require a distinct reviewed
promotion manifest and explicit owner/scientific approval. The candidate under
test cannot regenerate or bless its own authority in the same evidence run; a
promotion invalidates old dependent evidence and runs before the product
candidate that consumes it.

T2 generated cases exercise broad properties, GPU differentials, resource
pressure, and round trips. Generate each once per suite/run and share it
read-only; never treat its cache or self-agreement as conformance authority.
T3/T4 remain deferred to the public-data follow-on under D-021. T5 may close
private technical qualification, but DS-3/DS-4 remain explicitly
`privately qualified; public reproducibility pending` and cannot support an
unqualified public reproducibility/support claim until T4 exists. Sanitized T5
evidence may support only that narrowly worded private-qualification statement.
Exact T5 digests/paths stay in the private manifest; its public companion uses
opaque IDs. Dataset profiles and evidence tiers remain orthogonal: deterministic
DS-2 is normally T2, while a DS-1 case may be T3 or T5. T5 is read-only, never a
public contributor prerequisite, and never relabeled into T3/T4.

#### Sharding And Filesystem Proof

For each physical scale/array, with logical brick-grid dimensions `B*` and
outer-shard brick grouping `G*`, the expected pixel-object ceiling is:

```text
T * C * ceil(Bz/Gz) * ceil(By/Gy) * ceil(Bx/Gx)
```

summed across scales/arrays, with equivalent formulas for validity and packed
indexes. The package-wide bound also counts Zarr group/array metadata, M4D
bootstrap objects, bounded canonical manifest pages and their required
per-physical-object/per-shard descriptors, provenance/recipe objects,
directories, and maximum depth/fan-out.

A non-fill DS-0 fixture forces every expected shard to exist and asserts the
exact addressed-versus-actual count. Every import enforces the accepted
storage limits. Acceptance records concise shard, object, and fan-out totals;
one focused storage test records exact one-brick read/decode amplification at
the storage boundary. Any
per-logical-brick file, checksum sidecar, manifest record, or other unbounded
physical-object relation fails. Per-shard/object descriptors in bounded
canonical manifest pages remain required by D-009. Legitimately elided all-fill
shards must be derivable from validated metadata.

D-010 has a separate filesystem-growth invariant: project object/generation
counts may scale with saved revisions, retained artifacts, and total encoded
artifact bytes divided by a bounded page/object size. They may not use one
object per semantic voxel, logical brick, table row, or timepoint. Autosave/
analysis stress records encoded bytes, page-size bounds, object count, directory
fan-out, revision coalescing, retention/GC, and interrupted-GC behavior.

#### Product-Evidence Ladder

| Level | Meaning | Closure authority |
| --- | --- | --- |
| E0 | Unit/model/component/contract | Owning low-level requirements only |
| E1 | Instrumented application commands, internal state/readback | Integration support; not black-box/product-open |
| E2 | Packaged install/layout/headless launch and hosted unsupported-adapter/capability behavior | Package/bootstrap support only; never viewer workflow proof |
| E3 | Exact packaged artifact on trusted local `HW-2` with qualifying GPU, isolated virtual/test display, OS input, and external window observation | GPU-backed black-box workflow support; not real-display product-open |
| E4 | Exact packaged artifact on real `HW-2` display, OS input and external compositor/window capture | Required product-open authority |

E4 uses a clean HOME/XDG profile and the exact package digest. It interacts
through OS keyboard, mouse, window management, and file dialogs; externally
verifies the mapped window; captures compositor/window pixels; opens data;
navigates 2D/3D; exercises MIP/DVR/ISO, channel/timepoint changes, allowed
analysis, save, normal quit, relaunch, and durable restore; then repeats the
relevant recovery path after forced termination. It scans logs for panic,
validation, WGPU, fallback, retry-loop, and corruption signatures. Internal
automation commands, renderer readback, or direct app-state inspection cannot
be its pass oracle. At least one required E4 workflow uses authoritative T1
facts and externally observable pixel/landmark/state assertions; mapped or
nonblank output alone does not prove correctness. The forced-termination E4
case is representative product evidence, never a substitute for D-010's
exhaustive subprocess/failpoint/filesystem matrix.

Define `1280x720` and `1920x1080` as externally observed physical client-area
and presented/render-target pixels, not logical window points. Record DPI scale,
client area, and actual render target so HiDPI cannot silently create a 1440p/
4K workload. Run the gating workflow at `1280x720` and exercise the same
milestone/release candidate at `1920x1080`. No 4K or segmentation scenario
exists. Small public T1/T2 data covers trusted local E3/E4 workflows; DS-2 is
normally deterministic T2, while named T5 DS-1/DS-3/DS-4 cases provide only
the explicitly private qualification allowed above. Later T3/T4 releases add
public reproducibility rather than relabeling T5 evidence.

The external OS-automation harness, image/landmark assertions, and expected
state facts are frozen by digest before the candidate run and use the same
separate promotion/approval rule as T1 oracles. Candidate-controlled harness
changes cannot certify that candidate. WP-02 used a manually executed packaged
E4-equivalent deletion-regression checklist on the real display with a
rights-cleared small current/generated input before the final E4 harness and
target-profile T1 authority existed. That transitional evidence closed only
WP-02's product-open regression obligation; it does not satisfy the final
T1-backed E4 requirement. Current internal product automation remains E1. A
future supported-release or public-data handoff may define stronger external
product evidence; WP-14 does not build that harness.

#### Performance And Resource Statistics

Shared hosted runners never gate performance. Every blocking metric is
predeclared in a registry with requirement/owner, exact start/stop and clock,
submitted/completed/presented meaning, unit/direction, dataset/scenario/
viewport, executable/package digest, build/toolchain, hardware/backend/driver/
power/thermal fingerprint, cold/warm definition, warmups/repetitions and
independence unit, statistic/confidence rule, absolute and relative thresholds,
noise floor, calibration/drift, freshness, promotion, and waiver policy.

Release-quality profiles are:

- first-useful-frame p95: record three non-gating calibration launches, then
  sixty independent fresh-process trials and require zero threshold violations;
  process-cold and storage-cold are separate claims;
- command/interaction session-p95 qualification: sixty independent launches,
  each with five recorded non-gating warmup operations and at least twenty
  measured operations; compute a nearest-rank within-launch empirical p95 and
  require zero session-level threshold violations;
- sustained presented-frame-interval session-p95 qualification: sixty
  independent thirty-second scenario runs; retain each within-run nearest-rank
  empirical p95 and require zero session-level threshold violations;
- import/throughput/RSS: assert hard byte, queue, object-count, free-space, and
  cancellation bounds with deterministic counters and focused tests. One or a
  few practical local runs may report throughput and observed RSS without a
  release-quality performance claim; and
- resource ceilings, corruption, identity, and correctness: any one valid
  violation fails and cannot be averaged away.

For first-frame p95 and the two session-p95 qualifications, use exact order-
statistic/binomial tolerance logic rather than a fragile tail bootstrap: zero
violations in sixty independent trials has a one-sided 95% Clopper-Pearson
upper violation-probability bound of
`1 - 0.05^(1/60)`, approximately `4.87%`. Any threshold violation fails the
release-quality gate. Report all raw samples, nearest-rank descriptive
percentiles, and median absolute deviation as diagnostics. For interaction and
frame metrics, this qualifies the probability that an independent session's
empirical within-session p95 breaches the threshold; it is not presented as a
direct operation-level or frame-population p95 confidence statement.

Relative comparison requires at least twenty independent, interleaved baseline/
candidate pairs on the same calibrated machine. Pair order is derived from a
versioned SHA-256 counter seed over metric ID plus baseline/candidate identities.
The direction-normalized effect is `ln(candidate / baseline)` for lower-is-
better metrics and `ln(baseline / candidate)` for higher-is-better metrics, so
positive always means regression. Zero or non-positive values use a predeclared
absolute/difference statistic and never a log ratio. The statistic is the
median paired direction-normalized effect. A fixed
`10,000` paired-cluster resamples-with-replacement uses the same published seed
and rejection-sampled SHA-256 counter indices; the one-sided percentile bound
and all paired raw values are retained. Direction-normalized regression above `10%` requires
explanation only when it also exceeds the metric's noise floor; above `20%`
blocks only when the lower one-sided 95% bound exceeds `ln(1.20)`. An
inconclusive point estimate requires more independent pairs, not a pass.

All calibration/warmup attempts are retained. Their latency values are excluded
only by the predeclared profile, while crash, correctness, resource, or timeout
violations still fail. The `30/45 min` target/ceiling is for the complete
statistical batch of one named metric/scenario, not one trial. A batch that
cannot fit is not qualified; it must be narrowed by a reviewed metric decision
or reported as capacity/correctness evidence without a performance claim.
“Storage-cold” requires a reproducible recorded cache-reset method; a fresh
process alone is only process-cold.

Never delete individual outliers. A complete trial may be invalidated only by
a predeclared, logged environmental failure before inspecting its result.
Baseline refresh is a separate reviewed change and cannot make the regressing
change pass. Driver/toolchain/metric/dataset/hardware/power-policy or material
calibration changes invalidate comparison. Release-quality performance
evidence expires after thirty days. Across hard format/project cutovers or a
metric-definition change, no compatibility shim is introduced for comparison:
absolute gates apply and a new relative baseline is established only after the
new candidate passes independent acceptance.

#### Failure, Flake, Quarantine, And Evidence Semantics

- Required tests have zero automatic retries. Assertion failure, panic,
  timeout, crash, corruption, skipped/missing/unsupported state, or required
  artifact absence fails.
- A job may be manually rerun once only for a recorded external infrastructure
  failure such as runner provisioning, GitHub outage, or package-mirror outage;
  both attempts remain visible and the first test failure is never erased.
- “Flaky” requires contradictory outcomes for the same clean tree, fixture,
  toolchain, and environment. Quarantine requires a reviewed manifest entry
  naming requirement, case, evidence, issue, owner, signature, date, alternate
  coverage, and expiry. Maximum duration is fourteen days.
- Quarantined cases run visibly in a separate non-required lane, satisfy no
  requirement, and cannot enter release evidence. A missing required assertion
  blocks closure from day one unless an already-passing independent replacement
  proves the same requirement; fourteen days is only the remediation deadline,
  never a grace period for missing proof. Expiry additionally blocks all
  milestone/release promotion. This policy does not activate a new WP-14 lane.
- Every invocation writes into a unique run-ID directory. Each case records
  requirement/case/owner/lane, exact tree and executable/package, oracle and
  fixture identities, assertions, raw result, environment, artifact digests,
  retention, and waiver. Requirement coverage is computed per passed assertion,
  not per case or report presence; hard-coded “covered” flags are forbidden.
- Native process/job exit is authoritative. Composite closure explicitly
  assembles same-revision manifests; it never scans persistent `target/` paths
  or lets a JSON report convert failed, stale, missing, or unsupported work into
  success.

Code coverage and requirement coverage are separate. Requirement coverage maps
passed assertion IDs to approved requirements and is a closure gate. Code
coverage is instrumented source line/region/function reachability used to find
gaps; a percentage cannot prove a requirement. If `Deep / code-coverage` is
activated by a future concrete need, first freeze the exact tool/version/command,
crate and target scope, reviewed generated/vendor/platform exclusions, clean
baseline, per-crate metrics, changed-code policy, allowed noise, and no-regression ratchet.
The command must reproduce from the registry and must not silently include
Criterion, GPU, E3/E4, or T5 cases in a CPU number.

### D-022/D-023 Alternatives Rejected

| Alternative | Rejection reason |
| --- | --- |
| Repair the existing `verify-fast/full/nightly` stack | Preserves recursive reruns, mixed semantics, ignored/name allowlists, report authority, and the wrong ownership graph |
| Put every test in one universal fast lane | Makes pure feedback hostage to I/O/UI/GPU/large-data behavior and recreates the predecessor eighty-minute gate |
| Make every lane a required hosted check | Trusted GPU/T5/product/performance work cannot safely or credibly run for arbitrary public PR heads |
| Attach the workstation as a public self-hosted runner | Public fork code can compromise persistent local data, credentials, and network access |
| Let retries/quarantine keep required checks green | Converts nondeterminism and missing proof into false confidence |
| Treat Mirante round trips or CPU/GPU agreement as conformance | Shared producer/consumer/math defects can agree exactly |
| Gate performance on hosted-runner scalar deltas | Runner noise, single samples, stale context, and no confidence rule make the result non-scientific |
| Upload broad `target/` reports/artifacts | Recreates large paid-retention risk, leaks private context, and lets stale files masquerade as current evidence |
