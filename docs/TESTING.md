# Testing And Evidence

Last updated: 2026-07-12

## Claim Language

- **Implemented:** the change exists.
- **Automated-verified:** the named automated checks passed for the stated
  revision.
- **Product-validated:** the normal native application ran on a real mapped
  display and the affected workflow was exercised on the relevant dataset and
  hardware, with logs and evidence inspected.

Do not collapse these claims. Unit tests, smoke tests, virtual/no-display
automation, snapshots, benchmarks, preflight runs, and render readbacks are
supporting evidence, not product validation.

## WP-06 Checks

This revision exposes six independently selectable leaves:

```bash
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
cargo xtask verify-leaf doctest
```

Run both public groups locally with:

```bash
cargo xtask verify-pr
```

`verify-pr policy` and `verify-pr rust` select one group when focused feedback
is useful. The Rust group performs one discovery/build, checks exact ownership,
then runs the unit/contract/UI union once before doctests. Tests are never
retried.

The generated selectors and Nextest configuration must match their registry:

```bash
cargo xtask verification-sync --check
```

The live test inventory is discovered and assigned by the verification
registry; generated selectors must not be hand-edited. The old recursive
aggregates, `verify-fast`, and target-directory `report-audit` are not live
authorities.

Documentation alone can be checked with:

```bash
cargo xtask docs-check
```

That command validates the exact documentation inventory, authority ownership,
navigation, local links, and heading anchors. Command discovery is owned by
`cargo xtask --help`; this document does not duplicate the full command list.

## Hosted And Trusted-Local Boundary

The protected repository requires exactly `PR / policy` and `PR / rust`.
Matching non-required `Main / policy` and `Main / rust` jobs run on protected
main. The twenty-attempt cache-free calibration passed before the required-
context list was replaced and read back; the transitional Bootstrap bridge is
no longer part of the repository.

Hosted verification has a hard `$0` budget, no public self-hosted workstation,
no private data, no automatic retry, and no cache or artifact storage.

GPU, product E1-E4, packaged E2E, performance, stress, private T5, and
scientific evidence are separate trusted-local lanes. The current GPU command
is deliberately local-only:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu
```

Run it only on the designated clean Vulkan workstation. Private dataset paths
stay in local resolvers and never enter the public tree or hosted logs. The
package-capability lane is registered as pending; WP-06 does not invent a
passing package claim before an honest unsupported-GPU command exists.

WP-09A adds one ignored aggregate successor case to this lane while retaining
the 34 predecessor GPU cases. It starts the real Vulkan runtime once per
logical ledger, consumes only dataset-runtime-issued leases, and covers the
semantic-small, 8-MiB upload, and 128-resource work boundaries. The verifier
refuses dirty revisions and requires exactly one sanitized
`wp09a-evidence-json` record. That record keeps the main and small-capacity
ledgers separate, validates the exact case facts and per-frame maxima, and
includes a render after every dataset lease is released. This is off-product
component evidence, not viewer or performance validation.

No benchmark baseline is currently authoritative. The temporary tool-owned
[baseline directory](benchmarks/baselines/README.md) remains diagnostic until
WP-14 owns its final disposition.

## Product Validation

Rendering, viewport, GPU, data-loading, interaction, and large-dataset changes
are incomplete until the actual viewer is opened on a real display with the
relevant dataset and hardware, unless the user explicitly waives the gate.
Validation must exercise the changed workflow, confirm the app remains alive
without a hidden fallback or repeated GPU error, and inspect the resulting
logs and evidence. Packaging or release changes use the packaged application.

## Current Boundaries

WP-06 is complete: its exact protected-main revision passed the real Vulkan
viewer exercise at 1280x720 and 1920x1080 before
`foundation-wp-06-exit-1` was created. WP-07A is accepted at
`foundation-wp-07a-exit-1` (`5383cbb93c13c59e6f035bfa551356c75fb426dc`).
WP-07B is accepted at `foundation-wp-07b-exit-1`. WP-08A's corrected contract
is accepted at `foundation-wp-08a-exit-2`
(`f2e520da891134d1b3f65d8fcac7afb4140579a2`). WP-08B is accepted at
`foundation-wp-08b-exit-1`
(`0e3bdb0f5257c820841cee215cee38747efbda75`) after exact-main public,
trusted-GPU, source-nonmutation, and real-display 1280x720 T2 product checks.
WP-10A is accepted at `foundation-wp-10a-exit-1`
(`9b3a81d79a50027c0a8ddedc535021809a99d928`) after exact protected-main
policy, Rust, and local format-lifecycle evidence.
WP-09A is accepted at `foundation-wp-09a-exit-1`
(`1b1e7d5534f29b010cc346d434811a3906fb40e1`) after exact protected-main
policy/Rust and trusted-Vulkan evidence. Its owner-approved product-open
deferral applies only to this deliberately unreachable successor; WP-09B still
owns the real-viewer cutover gate.
Internal automation remains supporting evidence, not product-open proof.

WP-10B's independent project-store wire authority is checked in the public
policy group with `python3 tools/project-fixtures/validate.py --manifest
fixtures/project/manifest.json --self-test`. It proves exact canonical bytes,
refs, object/page closure, recovery classification, and declared corruptions;
its self-test accepts manual and autosave recovery-ahead states both with and
without `head.previous`, and rejects unrelated same-lane recovery targets. It
does not independently conform repeated reuse of one physical page within a
generation and does not claim filesystem durability or product save/open
support.

The unreachable `mirante4d-project-store` crate is assigned to the hosted
contract lane. Its current tests cover the frozen API/limits, canonical
envelope/ref identities, typed canonical generations, direct and deterministic
paged closure, held-descriptor traversal, bounded immutable object and
generation-last publication, process lease contention, and exact reproduction
of the fixture's initial and established manual refs. Established-manual cases
cover exact g1-to-g2 recovery-before-head bytes, sequence derivation across an
autosave head, zero immutable rewrites on full reuse, rejection before source
reads, exact global-entry/fan-out capacity rejection without a ref change,
cancellation/retry through recovery-ahead, retry after either recovery sync
leg fails, and either final head sync-leg failure as write-suspending
`CommitIndeterminate`. Established-autosave cases reproduce the independent
first and advancing autosave generations and refs, replace a divergent lane
against the current manual base, accept a lower revision with a non-regressing
high-water mark, reject stale parent/base and invalid recovery/capacity state,
retry an exact recovery-ahead cancellation, and distinguish recovery-sync
failure from write-suspending head indeterminacy. Five actor cases exercise the
real established manual/autosave primitives under one worker and prove exact
request correlation, the request/completion bounds, queued-autosave coalescing,
active and queued cancellation, close rejection, writer-lease lifetime, and
joined or nonblocking shutdown. They do not claim Create/Open/Save As
execution, provisional autosave, recovery selection/open, timers, garbage
collection, full verification, public actor wiring, the exhaustive fault matrix
or power-cut durability, product reachability, or product-open validation.

The checked independent source report supports only the WP-03 source-TIFF
archive. WP-10A is accepted off-product and its target authority is promoted.
`mirante4d-storage` is assigned to the existing contract leaf. Its lower-level
tests prove profile, path, arithmetic, supporting exact-identity, scalar-wire,
and restricted-JCS
contracts, including the closed profile, canonical-value, scientific, and
display-defaults grammars, verified recipe payloads, and exact manifest
descriptor/page/root bytes. Closed portable-record tests prove structural
canonical bytes; derivation and detached-release tests additionally prove typed
identity verification. Packed-index and shard-codec tests prove the closed
record layout, bounded zstd/CRC32C pipeline, exact end-index sizes, and strict
structural rejection. Storage-metadata and range-read tests prove the nine
closed Zarr rows, the closed OME axes/transform projection, semantic JSON
validation, exact range bounds, and rejection of symlink, hardlink, and non-
regular objects. Catalog tests additionally prove canonical manifest-page
authentication, exact opening-metadata bytes, and initial cross-object layer
and storage-shape rejection. The directory-inventory test proves exact positive
closure counts, cancellation, extra-directory rejection, and post-open length-
drift rejection, including same-length manifest-authority drift. These lower-
level tests alone make no DS-specific admission, shard-payload, official-
schema, complete-package, T1 conformance, independent-reader, lifecycle, or
product-support claim. Current schema-1 packages remain non-authoritative T2
support fixtures; the promoted target authority is separate and off-product.

Address-planning tests prove 2D/3D grid, C-order ordinal, inner-slot, and edge-
extent arithmetic plus the exact baseline catalog-derived paths and packed-
record offset. They also prove coordinate/overflow rejection, mandatory packed-
index descriptors, and optional fill-elided pixel descriptors. They read no
shard bytes and make no payload-integrity claim.

The bounded brick-core test covers 2D uint8 pixel-present, all-fill, and
explicit-validity cases with exact two-, four-, and six-request accounting.
It also checks edge extent, required descriptor/inner failures, packed-record
cross-checks, length drift, and selected packed-index/pixel corruption. A
focused boundary test accepts each exact absolute amplification ceiling and
rejects one unit above it. These core tests alone do not prove 3D,
incompressible, whole-object SHA-256, DS admission, complete-package, or
PackageId-attributed reads.

Dataset-profile admission tests prove explicit profile selection, exact
logical/addressed/actual counts for tiny 2D pixel, all-fill, and explicit-
validity fixtures, zero-file pixel elision, packed-index coverage, shard-grid
rejection, cancellation, and per-image rather than summed scale rules. They do
not materialize advertised keys; a pure admission-arithmetic test directly
reproduces every frozen 3D/multiscale logical-brick, addressed-shard, and
logical-S0 boundary vector. These tests do not qualify an exact DS fixture or
validate packed records, payload digests, or scientific identity.

The package-wide structural reconciliation test covers pixel-present, all-
fill, explicit-validity, and explicit-all-invalid packages. It rejects record
coordinate/validity drift, missing or extra packed slots, nonzero final packed
padding, missing/extra pixel and validity payload slots, all-missing shard
objects, out-of-grid payload slots, packed-index digest drift, mid-pass/final-
sweep cancellation, and same-length replacement before the last snapshot
gate. A focused arithmetic test covers nondivisible edge capacities, C-order
slot masks, and packed records 255/256 and 16383/16384. The pass reads packed-
index shards completely and only pixel/validity shard tails, so it does not
validate pixel/validity payload digests or values, prove the declared PackageId
closure, recompute scientific identity, or qualify an exact DS fixture.

The exact-package validation test composes explicit DS admission and structural
reconciliation with fixed-buffer streaming SHA-256 over the root, every page,
and every descriptor object. It proves digest-drift rejection for opening and
shard objects, phase-coherent snapshots, immediate and final-sweep
cancellation, final inventory, mutation rejection before capability issuance,
and PackageId-attributed pixel/validity reads. The range-I/O test separately
proves multi-buffer hashing and mid-stream cancellation. Capability freshness
tests reject replaced consumed shards and exercise the explicit complete-
snapshot sweep without imposing an all-object scan on every brick. The sweep
is deliberately a sequential mutation check, not an atomic snapshot of a
concurrently writable directory. This is exact package-byte integrity only: it
does not parse lazy portable-record semantics, recompute ScientificContentId,
qualify IO-3 or independent T1, make a product/performance claim, or implement
an importer.

The consuming scientific validator adds one bounded, cancellable base-scale
scan after exact validation. Its stronger capability exposes the independently
matched ScientificContentId and layer roots plus exact tile, brick, voxel,
canonical-byte, and validity-byte work counters. The target integration reads
every brick in all three promoted packages and matches every full-array and
per-layer raw/canonical/validity digest, selected value, brick statistic,
addressed/actual shard count, object/depth/fan-out count, and observed
one-brick amplification maximum. The production mutation suite rejects all 15
promoted cases at typed boundaries. A separate subprocess test opens
2,750/5,500/11,000-descriptor catalogs and enforces fixed linear metadata-work
bounds. The largest case also enforces 10-second and 64-MiB post-open RSS stop
ceilings; these are contract limits, not product benchmark claims.

Writer tests prove byte-identical package trees and PackageId values across
different parents and reversed input order, then reopen output through full
exact validation and PackageId-attributed brick reads. They cover pixel-
present, all-fill, explicit-validity, and explicit-all-invalid storage, plus
cancellation cleanup, create-only collision safety, source nonmutation,
private mode-0700 staging, no-replace symlink races, precommit cleanup, and
post-rename durability-indeterminate handling. A selected-profile limit-plus-
one case proves that the writer stops consuming a lazy shard input and
publishes nothing. This is a T2 writer/reader component proof, not independent
T1 conformance, import, replacement, product support, or a performance claim.
Separately, the production writer reconstructs all three T1 cases and the
hash-locked zarr-python reader compares their complete semantic image and
scientific facts with the promoted authority. Encoded shard bytes and exact
PackageId may differ without weakening the semantic comparison.

The WP-10A-C standards check verifies an exact 12-file, 162,831-byte offline
mirror against immutable OME and Zarr revisions, lengths, and SHA-256 values.
The diagnostic external-reader probe builds the same tiny shard twice without
Mirante code and observes its exact shape, chunk/shard geometry, dtype, and
values through a hash-locked zarr-python 3.2.1 environment. That older probe
remains only a selected-codec feasibility result; the separate promoted corpus
carries the target T1 authority.

Validate the promoted authority with:

```bash
python3 tools/target-fixtures/t1/validate.py \
  --manifest fixtures/target/manifest.json --self-test
```

`target-m4d-v1` proves bounded archive safety and exact closure, independent
lineage bindings and expected facts, full-array readback, pinned OME-schema
results, critical identity vectors, exact rejection of 15 mutations, and
byte-identical two-run reproduction for the frozen EXPERIMENTAL profile.

Re-run the accepted WP-10A evidence with:

```bash
cargo xtask verify-local format-lifecycle
```

That real local lane validates the promoted authority, runs the three positive
and 15 negative production cases, and performs writer-to-pinned-reader
readback. WP-10A remains off-product and EXPERIMENTAL; it makes no
stable-format, generic OME-Zarr, importer, product-support, product-open, or
product-activation claim.

The exact thresholds live in the
[verification brief](plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md).

## Reporting

Completion reports name the revision, commands, fixtures or datasets,
hardware/display where relevant, results, failures, skipped checks, waivers,
and remaining risk. Performance claims also name the workload, metric,
sampling method, and threshold.
