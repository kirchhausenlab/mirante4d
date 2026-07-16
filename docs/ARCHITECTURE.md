# Architecture

Last updated: 2026-07-15

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict `.m4d` packages; source microscopy data enters through explicit
import/preprocessing workflows.

## Workspace Boundaries

The workspace has eighteen packages (seventeen `mirante4d-*` crates plus
`xtask`):

- `mirante4d-domain`: validated framework-neutral geometry, view, transfer,
  render-intent, and tool values.
- `mirante4d-identity`: strict typed identities plus pure SHA-256, NFC, and
  scientific-tree primitives; no filesystem I/O.
- `mirante4d-project-model`: canonical durable project/view state and
  persistence-neutral generation projections.
- `mirante4d-project-store`: experimental product project storage, reached only
  through the application-owned project-store service.
- `mirante4d-application`: the sole command reducer, revision/history owner,
  transient semantic state, operations, events, snapshots, and typed faults.
- `mirante4d-settings`: closed settings document and bounded background I/O.
- `mirante4d-dataset`: immutable multiscale catalog, semantic resource keys,
  source/decode-sink boundary, value-plus-validity payload views, lease
  contract, and the dependency-inverted CPU byte-ledger admission interface.
- `mirante4d-dataset-runtime`: unified request, cancellation,
  deduplication, bounded configuration/diagnostics/progress, CPU-ledger,
  completion, fault, and accounted-lease contract plus the sole production
  scheduler and worker owner.
- `mirante4d-analysis-core`: pure exact intensity operations, deterministic
  statistics, and canonical table/plot artifact payloads.
- `mirante4d-analysis-runtime`: bounded, cancellable analysis execution over
  shared dataset-runtime requests, producing pending atomic artifact bundles.
- `mirante4d-render-api`: backend-neutral intent, requirements, progressive
  frame status, opaque presentation lifecycle, and camera math.
- `mirante4d-render-reference`: unpublished, bounded CPU oracle for renderer
  correctness; it owns no product route or GPU authority.
- `mirante4d-render-wgpu`: sole product renderer, with bounded progressive GPU
  residency and presentation built only against dataset leases and render
  contracts.
- `mirante4d-storage`: active target-profile catalog, checked ceilings,
  portable package paths, bounded local validation/reads, exact and scientific
  capabilities, dataset source, and deterministic create-only local writer.
- `mirante4d-import-pipeline`: active bounded, cancellable, restartable
  TIFF/OME-TIFF producer for validated sharded target packages.
- `mirante4d-ui-egui`: active egui visual components, UI-facing message
  projection, and transient egui interaction state; its only Mirante
  dependency is `mirante4d-application`.
- `mirante4d-app`: native process/service composition and presentation-token
  resolution.
- `xtask`: developer and verification tooling, never a product mode.

Lower crates do not depend on the app/UI layer; the renderer does not read
files; format code does not own viewer state. The product uses
`mirante4d-storage`, `mirante4d-import-pipeline`, and
`mirante4d-render-wgpu`; the CPU reference renderer is test-only.

## Application Composition

`MiranteWorkbenchApp` holds `ApplicationState`, bounded
`DatasetDemandState`, process diagnostics, egui state owned by
`mirante4d-ui-egui`, the opt-in product-validation controller, and narrow
project-store/settings/source-open handles. It is a
composition root, not a second model.

Product automation is composed directly and its render-size override exists
only in test builds. The native app projects one immutable workbench view,
calls `mirante4d-ui-egui` once, and resolves the returned typed commands,
service requests, and opaque presentation paints.
Widget layout and interaction state do not have a second native path.
`ProjectStoreApplicationService` is the sole product project I/O route; its
actor owns project roots, sessions, leases, refs, recovery, and filesystem
mutation. There is no compatibility reader or fallback.

The native `ImportWorkflow` owns TIFF worker cancellation, bounded terminal
results, retry options, and explicit joining. It projects immutable import
facts through `ApplicationSnapshot`; egui owns only the editable review draft
and returns ID-checked import commands. Egui owns no path, TIFF inspection,
worker channel, or thread handle.

The import pipeline has one source-native authority. It captures the reviewed
inventory, traverses each admitted TIFF strip/tile once, and writes canonical
little-endian `[c,t,z,y,x]` planes to a fixed two-file cache. Source admission
is deliberately closed to uncompressed, LZW, current/old Deflate, and PackBits
grayscale `uint8`, `uint16`, or finite `float32` pages. JPEG, WebP,
Zstd-in-TIFF, fax, and other unaudited decoder workspaces fail as unsupported
sources rather than selecting another path. Inspection, native decode, and the
final SHA-256 pass bind reads to the opened descriptor's generation as well as
guarding the source path before and after use; ingest and final revalidation
also compare the reviewed generation. Before decoder construction, a bounded
positional-read preflight walks the primary IFD chain, rejects oversized or
duplicate eager fields and more than 65,536 pages, and charges retained
multipage decoder state separately from one-plane parallel workers.

The canonical cache is opened relative to the spool-held checkpoint directory
descriptor with no-follow semantics. It retains the exact data/state
descriptors, shares descriptor-bound positional readers across workers, and
removes a checkpoint name only when it still resolves to the retained file
identity. The four-file spool applies the same fixed-name and descriptor-owned
authority to canonical encoded logical chunks. Canonical batch triggers are 16
completed planes, 64 MiB of pending plane bytes, and a 15-second age check;
spool triggers are 512 work units, 64 MiB of pending encoded payload, and the
same age check. Stage boundaries and entry into a serialized decoder interval
may commit earlier. A canonical plane above 64 MiB is rejected before ingest;
the byte ceiling is not expanded for a large plane. Recovery accepts only
checksummed, binding-matched durable prefixes, discards at most one bounded
incomplete batch, and has no predecessor-schema reader.

CPU work is admitted by the shared byte ledger and bounded by cores and
per-task allocation ceilings. Eligible single-plane source decodes,
normalization, downsampling, scientific-tile preparation, and inner encoding
run concurrently; multipage or over-task-ceiling TIFFs use one streaming
decoder. The calling owner alone advances checkpoint state and commits
canonical order. Canonical and spool positional readers are shared, so worker
tasks add no per-task checkpoint descriptors.

Publication streams those validated inner encodings into indexed outer shards
without decoding and re-encoding pixel or validity chunks. The storage writer
then performs structure, exact, and locality-aware scientific validation before
create-only atomic rename. Scientific validation prepares the four `z=16`
identity leaves intersecting a `z=64` brick while that brick is resident, so
each present base brick is decoded once per scan. The scientific writer keeps
the resulting non-cloneable capability, seals it to the private stage's
filesystem identity, and rebinds it only when `RENAME_NOREPLACE` publishes that
same directory. Within the cooperative local destination-parent namespace
assumed by this contract, an inventory/snapshot/inventory sandwich before
rename and again when the product consumes the transfer detects extra paths,
missing paths, object mutation, and root replacement without hashing payloads
or decoding scientific bricks again. This proof is observational rather than
adversarial namespace isolation: a hostile actor able to rename or unlink
entries in the destination parent is outside the contract because Unix cannot
atomically bind a source name to an already-open directory descriptor. Failure
within the contract is terminal for automatic import opening; it never falls
back to the external-package verifier.

Staged-validation object-read evidence counts every successful strict-reader
object open, including whole reads, ranges, hashes, and snapshot-only
revalidations, and reports structure, exact, and scientific components plus
their checked sum. Directory-only metadata inspection is not an object read.
Codec operation/time evidence distinguishes checkpoint inner encoding and
checkpoint-dependent decoding from package-construction encoding and
staged-validation decoding. Durability operation/time
evidence covers canonical-cache and spool file/directory synchronization,
one counted Linux `syncfs` barrier after all staged package objects close,
every staged directory, and the destination parent synchronization after
rename. The barrier is filesystem-wide rather than stage-scoped: unrelated
dirty data can add latency or surface a conservative writeback failure. Package
creation rejects Linux kernels older than 5.8, where `syncfs` did not reliably
report those failures.

The one-shot published-capability consumer returns a storage-issued execution
receipt for its closed inventory/snapshot/inventory route. The receipt splits
strict object-open deltas across both inventory passes and the intervening
complete snapshot sweep, binds the expected snapshot count to the retained
exact-package proof, reconciles the phase sum to the total, and records codec
decode calls. Storage and the T2, T5, and product validators require matching
inventory deltas, an observed snapshot delta equal to that independent
expected count, an exact phase sum, and zero codec decodes. A storage source
architecture test separately forbids exact-package hashing, scientific
validation, or brick-read calls in this transfer route. The workspace
architecture check also requires the imported verified-dataset completion to
have exactly one production constructor, in the current-source-open adapter.
These are structural call-path and observed-I/O facts; they are not
self-declared zero pass counts.

On the qualified Linux path, one low-overhead sampler observes descriptors
throughout the primary import clock across all worker threads. It attributes
source, checkpoint, destination, and private-stage paths every 5 ms rather than
using an end-of-stage process-wide snapshot. This is path attribution, so an
unrelated descriptor to the exact same scoped path is conservatively included.
Because an open shorter than the sampling interval can be missed, evidence also
reports a structural descriptor bound tied to the maximum admitted source
workers, fixed checkpoint handles, shared worker readers, writer handles, and
validation transients. The enforced gate value is the greater of that bound and
the sampled peak. The current deliberately phase-summed structural bound is 35,
below the gate of 64.

```text
reviewed TIFF inventory
  -> source-native decode-once traversal
  -> fixed canonical base cache
  -> bounded ordered base/pyramid workers
  -> batched encoded spool
  -> encoded shard publication
  -> staged structure + exact + scientific validation
  -> metadata-only stage currentness proof
  -> atomic create-only rename
  -> destination-bound verified capability transfer
  -> metadata-only publication currentness proof
  -> verified product runtime open
```

`DatasetRequestDispatcher` is the sole application poll owner. It keeps only
bounded request correlation and cancellation generations; decoded allocations
remain owned and byte-accounted by `mirante4d-dataset-runtime`.
`mirante4d-storage::LocalDatasetSource` is the sole product dataset source.
`DatasetDemandState` retains exact runtime lease handles without copying their
payloads and passes borrowed semantic views to `mirante4d-render-wgpu`. There is
no alternate reader, scheduler, CPU display fallback, or app-owned payload map.

`AnalysisProductRuntime` is the narrow product bridge to the analysis
runtime. It uses the shared dispatcher below interactive priority and keeps at
most two analysis blocks in flight. Exact whole-layer time traces and numeric
box statistics produce one table/plot bundle; the application exposes decoded
values only after the project store publishes that bundle atomically. Reopen
authenticates the stored source identity and both artifact payloads before
installing either result.

Payload validity is explicit, so valid zero, invalid/no-data, and missing are
distinct. Cancellation generations are ordered only within their scope;
unrelated view and playback demand cannot cancel each other. Unverified reads
use an opaque per-open source ID, never a fabricated scientific-content ID.

## Runtime Flow

```text
native package
  -> LocalPackageCatalog, LocalDatasetSource, and immutable logical catalog
  -> canonical application snapshot
  -> semantic 3D / linked-panel / playback demand
  -> one bounded scheduler and CPU byte ledger
  -> immutable accounted leases
  -> bounded render-wgpu residency and progressive frame execution
  -> renderer-owned GPU target
  -> egui-wgpu presentation and diagnostics
```

Small fixtures and large datasets use the same path. Whole-volume residency
for a tiny fixture is an optimization inside that path, not a second product
architecture. Missing occupied data is loading/incomplete, never empty.
An explicit zero-resource plan means the view is outside selected data (or no
layer is visible); it is terminal and distinct from missing occupied data.

The product renderer owns one bounded WGPU arena, progressive residency,
current-frame suppression, and automation-only asynchronous validation
capture; the independent CPU oracle owns expected RGBA, coverage, and validity
facts. Qualification covers voxel-exact sampling, flat ISO shading, and one
semantic scale per layer; other intent variants are rejected explicitly rather
than silently approximated. Fixed input ceilings are 256 requirement records
and 128 supplied leases per call. Resident-resource metadata is capped at 256;
GPU control and reported coverage include at most 128 resources.

## Persistence And Settings

Target packages open provisionally through `LocalPackageCatalog` and
`LocalDatasetSource`. Background exact-package and scientific-content
verification promotes the same source generation. Project attach, open, and
save remain identity-gated, and observed source drift invalidates the verified
state. This is the external-open route. A package produced by the active
importer instead arrives with the linear publication capability described
above and is installed through one atomic `VerifiedDatasetOpened` reducer
completion. Dirty-project deferral retains that exact capability until Save,
Discard, or Cancel; it is never reduced to a path-only request.

`mirante4d-project-store` is the sole project-storage authority, reached by
the product only through `ProjectStoreApplicationService`. Its directory-backed
format uses immutable content-addressed objects, complete immutable
generations, and small atomic manual, autosave, recovery, and pin refs. Direct
and paged object closure is deterministic and bounded. One background actor
owns filesystem mutation, open sessions, leases, requests, cancellation, and
joined shutdown.

A session holds a shared maintenance lease; writable sessions also hold the
single writer lease. Writer contention opens an existing project read-only.
Create, Open, Save, Save As, autosave, explicit recovery selection, dirty
close, and application exit all use the same service and actor. Save As copies
and verifies the authenticated source closure into destination-local staging.
Autosave runs after 30 idle seconds or 120 seconds at most while the project is
dirty. Indeterminate durability suspends further writes until reopen rather
than treating visible files as a successful commit.

Open and recovery validate bounded control records, generation closure,
continuity, provenance, and filesystem object types without repairing the
source project. A recoverable failed Open retains a recovery-only session for
inspection and explicit selection; Save As can then install the selected state
in a new project while leaving the damaged project untouched. Writable
qualification is limited to the accepted Linux ext4 filesystem tuple;
unqualified existing stores open read-only and unqualified new destinations
fail before mutation.

Maintenance features remain deliberately narrow. Full verification hashes the
stable active closure but does not validate artifact scientific semantics,
repair data, inspect trash, or establish a broader durability claim.
Compaction planning is metadata-only and does not authorize deletion, estimate
reclaimable bytes, or prove backup approval. Private Trash and Purge operations
accept only their bounded zero-non-regenerable subsets and fail closed on
unknown, linked, malformed, foreign, or unsafe content. They have no product
UI, cannot authorize removal of non-regenerable artifacts, and their
process-crash coverage does not establish power-loss durability.

Settings use `mirante4d-settings-v1` at the Linux XDG/HOME path. The UI submits
validated changes; one background actor owns persistence. Legacy preferences
files are neither read nor changed.

## Guardrails

- One live authority per model, resource, operation, and persisted identity.
- No compatibility reader, fallback renderer, or parallel old path.
- No full-dataset in-memory product path or file-per-brick layout.
- Large work is bounded, cancellable, generation-aware, and stale-suppressing.
- Normal interactive rendering requires a working GPU. CPU rendering is for
  reference, diagnostics, export, benchmark, and explicit tests.
- Rendering/loading/UI/GPU changes require real product validation under
  [testing](TESTING.md).

`mirante4d-storage::PackagePath` is the sole package-path authority.
`mirante4d-identity` owns raw typed object facts and exact hashing, but no
parallel path type.
