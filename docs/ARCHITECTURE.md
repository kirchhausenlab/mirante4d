# Architecture

Last updated: 2026-07-14

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

`mirante4d-core` and the predecessor application/session/preferences models
do not exist. Lower crates do not depend on the app/UI layer; the renderer
does not read files; format code does not own viewer state.
The predecessor data, format, import, and renderer crates do not exist. The
product uses `mirante4d-storage`, `mirante4d-import-pipeline`, and
`mirante4d-render-wgpu`; the CPU reference renderer is test-only.

## Application Composition

`MiranteWorkbenchApp` holds `ApplicationState`, bounded
`DatasetDemandState`, process diagnostics, egui state owned by
`mirante4d-ui-egui`, the opt-in product-validation controller, and narrow
project-store/settings/source-open handles. It is a
composition root, not a second model.

There are no remaining temporary runtime owners. The former
`CurrentValidationRuntime` wrapper is deleted; product automation is composed
directly and its render-size override exists only in test builds.

The temporary egui bridge and render owner are deleted. The native app projects
one immutable workbench view, calls `mirante4d-ui-egui` once, and resolves the
returned typed commands, service requests, and opaque presentation paints.
Widget layout and interaction state do not have a second native path.
`ProjectStoreApplicationService` is the sole product project I/O route; its
actor owns project roots, sessions, leases, refs, recovery, and filesystem
mutation. The project-v15 bridge and `CurrentProjectRuntime` are deleted, with
no compatibility reader or fallback.

The native `ImportWorkflow` owns TIFF worker cancellation, bounded terminal
results, retry options, and explicit joining. It projects immutable import
facts through `ApplicationSnapshot`; egui owns only the editable review draft
and returns ID-checked import commands. Egui owns no path, TIFF inspection,
worker channel, or thread handle.

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
state.

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
