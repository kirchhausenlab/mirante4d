# Architecture

Last updated: 2026-07-13

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict `.m4d` packages; source microscopy data enters through explicit
import/preprocessing workflows.

## Workspace Boundaries

The workspace has nineteen packages (eighteen `mirante4d-*` crates plus
`xtask`):

- `mirante4d-domain`: validated framework-neutral geometry, view, transfer,
  render-intent, and tool values.
- `mirante4d-identity`: strict typed identities plus pure SHA-256, NFC, and
  scientific-tree primitives; no filesystem I/O.
- `mirante4d-project-model`: canonical durable project/view state and
  persistence-neutral generation projections.
- `mirante4d-project-store`: private off-product successor project storage;
  currently unreachable from the application.
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
- `mirante4d-render-api`: backend-neutral intent, requirements, progressive
  frame status, opaque presentation lifecycle, and camera math.
- `mirante4d-render-reference`: unpublished, bounded CPU oracle for renderer
  correctness; it owns no product route or GPU authority.
- `mirante4d-render-wgpu`: off-product progressive GPU successor built only
  against dataset leases and render contracts; WP-09B owns its future product
  activation.
- `mirante4d-storage`: off-product target-profile facts, checked ceilings,
  portable package paths, bounded local validation/reads, an exact-package
  capability, and a deterministic create-only local writer; currently no
  product authority.
- `mirante4d-data`, `mirante4d-format`, `mirante4d-import`,
  `mirante4d-analysis`, and `mirante4d-renderer`: current storage/runtime,
  format, import, analysis, and rendering implementations.
- `mirante4d-app`: native composition and egui shell.
- `xtask`: developer and verification tooling, never a product mode.

`mirante4d-core` and the predecessor application/session/preferences models
do not exist. Lower crates do not depend on the app/UI layer; the renderer
does not read files; format code does not own viewer state.
No product crate depends on `mirante4d-storage`, `mirante4d-render-reference`,
or `mirante4d-render-wgpu`. WP-10C owns the storage cutover and WP-09B owns the
render cutover.

## Application Composition

`MiranteWorkbenchApp` holds `ApplicationState`, payload-free
`DatasetDemandState`, process diagnostics, six remaining temporary runtime
owners, and narrow persistence/settings/source-open handles. It is a
composition root, not a second model.

The temporary owners and deletion gates are:

| Owner | Scope | Gate |
|---|---|---|
| `CurrentRenderRuntime` | render status, frames, GPU and presentation resources | WP-09B |
| `CurrentUiRuntime` | egui-local drafts and interaction facts | WP-09C |
| `CurrentProjectRuntime` | current project package path only | WP-10B |
| `CurrentImportRuntime` | current import execution | WP-10C |
| `CurrentAnalysisRuntime` | passive tables, plots, artifacts, and exports | WP-12 |
| `CurrentValidationRuntime` | product-validation harness only | WP-14 |

The private egui bridge translates UI input to `ApplicationCommand` and reads
snapshots/events. The private project-v15 bridge is the sole temporary project
I/O route and has no compatibility reader. Both are mandatory-deletion
bridges, not permanent public APIs.

`DatasetRequestDispatcher` is the sole application poll owner. It keeps only
bounded request correlation and cancellation generations; decoded allocations
remain owned and byte-accounted by `mirante4d-dataset-runtime`.
`CurrentDatasetSource` is the one temporary current-storage bridge until
WP-10C. `CurrentLeaseBridge` retains runtime leases without copying their
payloads and is the one temporary current-renderer bridge until WP-09B. There
is no alternate reader, scheduler, CPU display fallback, or app-owned payload
map.

Payload validity is explicit, so valid zero, invalid/no-data, and missing are
distinct. Cancellation generations are ordered only within their scope;
unrelated view and playback demand cannot cancel each other. Unverified reads
use an opaque per-open source ID, never a fabricated scientific-content ID.

## Runtime Flow

```text
native package
  -> CurrentDatasetSource and immutable logical catalog
  -> canonical application snapshot
  -> semantic 3D / linked-panel / playback demand
  -> one bounded scheduler and CPU byte ledger
  -> immutable accounted leases
  -> current lease renderer bridge and GPU residency
  -> renderer-owned GPU target
  -> egui-wgpu presentation and diagnostics
```

Small fixtures and large datasets use the same path. Whole-volume residency
for a tiny fixture is an optimization inside that path, not a second product
architecture. Missing occupied data is loading/incomplete, never empty.
An explicit zero-resource plan means the view is outside selected data (or no
layer is visible); it is terminal and distinct from missing occupied data.

The WP-09A successor is deliberately outside this product flow. It owns one
bounded WGPU arena, progressive residency, current-frame suppression, and
asynchronous validation capture; the independent CPU oracle owns expected
RGBA, coverage, and validity facts. The current `mirante4d-renderer` remains
the only reachable product renderer until WP-09B. WP-09A qualification covers
voxel-exact sampling, flat ISO shading, and one semantic scale per layer; other
intent variants are rejected explicitly rather than silently approximated.
Its fixed input ceilings are 256 requirement records and 128 supplied leases
per call. Resident-resource metadata is capped at 256; GPU control and reported
coverage include at most 128 resources.

## Persistence And Settings

Unverified sources are unbound workspaces. Project attach/open/save rejects at
the typed identity gate because current sources do not expose a verified
scientific-content ID. The private project-v15 actor exists only to exercise
the future boundary; it is deleted by WP-10B.

WP-10B B1 freezes the successor's canonical envelope, generation, ref, object,
payload-paging, API, and failure-transition contract plus an independent
project fixture. B2 now has an unreachable `mirante4d-project-store` crate with
the frozen public boundary, typed canonical generation records, deterministic
direct and paged object closure, and descriptor-relative immutable object and
generation-last publication. Its private lease core now enforces shared
maintenance and one writer. A prepared fresh store can publish its initial
manual head, and the private transaction core can advance an established
manual head through bounded live-ref/closure validation, a descriptor-relative
global-entry/fan-out inventory, and exact recovery-before-head replacement. The
same crate-private transaction can create the first established-project
autosave or advance its autosave lane, including changed-base divergence and
recovery-ahead retry. One shared crate-private inspection core opens established
stores, holds their maintenance/writer leases, validates the bounded envelope,
ref, generation, continuity, and physical-object metadata graph, and reports
writable contention as an explicit read-only mode. Transactions and actor
startup consume that same authority. Bulk payload digests remain streaming or
explicit verification work. A bounded graph extension recognizes healthy
provisional stores and strictly enumerates the immutable generation/object
namespaces, validates every generation metadata closure, and reports exact
ref/pin roots plus capped orphan generations. Parent and autosave-base IDs are
relations, not liveness edges. The result is recovery/compaction input, not a
trash authorization. The corrected public contract makes successful Open and
OpenRecovery return the held session together with the validated loaded
projection. Recovery inspection remains metadata-only; manual fallback uses a
distinct manual-branch classification, and selection never rewrites refs. A
separate bounded recovery reader tolerates invalid head bytes or generation
targets while keeping malformed namespaces, mixed lineage, capacity, and
provenance fail-closed. The private actor can retain a corrupt-head or
writer-contended read-only session for InspectRecovery/OpenRecovery and returns
the actual ref facts with an explicitly selected projection without changing
authority. It otherwise solely owns its opened root and leases, serializes
manual/autosave transactions, authenticates Save As
against the live session, and transfers ownership to the durably installed fork
only on success. It also enforces bounded requests, completions, autosave
coalescing, cancellation, close, and shutdown. Private Pin/Unpin commands now
atomically create, replace, or remove exact checkpoint roots after fresh graph
validation, and suspend writes if post-mutation durability is indeterminate.
The accepted transition inventory names pin, unpin, and Purge mutation phases;
Pin/Unpin exhaustive injection evidence remains later B2 work. Private
FullVerify now takes a bounded stable snapshot of every active generation and
object outside staging and trash, hashes every physical object, reconstructs
paged logical objects, and repeats the snapshot before success. It is
cancellable, available to read-only sessions, and changes no store authority.
It does not validate artifact scientific semantics, repair data, verify trash,
or prove durability. Private PlanCompaction now repeats the bounded graph
snapshot and returns deterministic metadata-only recovery-review candidates
for every orphan generation. It is cancellable, available read-only, and
changes no authority. Its result is not Trash authorization, a physical
object/byte plan, a reclaim estimate, or backup approval. The accepted Trash
safety correction freezes a mirrored quarantine layout, fresh exclusive
preflight, retained-closure subtraction, bounded synced batches, exact retry,
and requires `ConfirmationRequired` rejection when any selected generation
declares a non-regenerable artifact. The private actor now routes that subset
under the same-descriptor maintenance transition with bounded selection,
correlated completion and cancellation, generation-first no-replace
quarantine, retained-object preservation, exact retry/deduplication, bounded
synced batches, and fail-closed active-plus-trash inventory. Its ten frozen
transitions have exact before/after occurrence hooks and fresh-process
kill/retry coverage; that evidence does not simulate power loss or qualify
filesystem durability. The accepted Purge safety correction freezes whole-
trash selection, strict zero-non-regenerable authorization, object-first
synced deletion with generation metadata retained through the phase barrier,
bounded cancellation, and fresh-process retry. The private actor now routes
that exact Purge subset. Strict active-plus-trash preflight rejects malformed,
unreferenced, incomplete, foreign, linked, or non-regenerable content before
unlink; bounded digest-ordered object batches cross a revalidated synced empty-
object barrier before generation records are removed in generation-ID order.
Directory hierarchies remain in place, retained active copies are untouched,
and exact retry includes required empty-fan-out sync sweeps. All observed Purge
maintenance, remove, and directory-sync occurrences have exact before/after
callback and fresh-process kill/reopen/retry coverage: 16 cases in each matrix.
This proves logic and process-crash recovery only, not power-loss durability or
filesystem qualification. The frozen public actor remains non-constructible. The crate
still owns no public Create/Open/
Save As execution, provisional autosave publication, product recovery workflow,
timers, public/product garbage-collection wiring, qualified durability, or
product path.

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

The frozen subsystem boundary remains in
[`architecture/wp08a-subsystem-contract.json`](../architecture/wp08a-subsystem-contract.json).
The accepted off-product storage successor boundary is
[`architecture/wp10a-storage-contract.json`](../architecture/wp10a-storage-contract.json).
The accepted off-product render successor boundary is
[`architecture/wp09a-render-contract.json`](../architecture/wp09a-render-contract.json).
Within that successor, `mirante4d-storage::PackagePath` is the sole package-path
authority. `mirante4d-identity` owns raw typed object facts and exact hashing,
but no parallel path type.
The concise live owner/deletion ledger is
[`architecture/current-state-field-ledger.json`](../architecture/current-state-field-ledger.json).
Later target ownership is defined by the
[workspace architecture brief](plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md).
