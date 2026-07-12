# Architecture

Last updated: 2026-07-12

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict `.m4d` packages; source microscopy data enters through explicit
import/preprocessing workflows.

## Workspace Boundaries

The workspace has sixteen crates:

- `mirante4d-domain`: validated framework-neutral geometry, view, transfer,
  render-intent, and tool values.
- `mirante4d-identity`: strict typed identities plus pure SHA-256, NFC, and
  scientific-tree primitives; no filesystem I/O.
- `mirante4d-project-model`: canonical durable project/view state and
  persistence-neutral generation projections.
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
- `mirante4d-storage`: off-product target-profile facts, checked ceilings and
  preflight arithmetic, and portable package paths; currently no filesystem,
  reader, writer, or product authority.
- `mirante4d-data`, `mirante4d-format`, `mirante4d-import`,
  `mirante4d-analysis`, and `mirante4d-renderer`: current storage/runtime,
  format, import, analysis, and rendering implementations.
- `mirante4d-app`: native composition and egui shell.
- `xtask`: developer and verification tooling, never a product mode.

`mirante4d-core` and the predecessor application/session/preferences models
do not exist. Lower crates do not depend on the app/UI layer; the renderer
does not read files; format code does not own viewer state.
No product crate depends on `mirante4d-storage`; WP-10C owns that future hard
cutover.

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

## Persistence And Settings

Unverified sources are unbound workspaces. Project attach/open/save rejects at
the typed identity gate because current sources do not expose a verified
scientific-content ID. The private project-v15 actor exists only to exercise
the future boundary; it is deleted by WP-10B.

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
The concise live owner/deletion ledger is
[`architecture/current-state-field-ledger.json`](../architecture/current-state-field-ledger.json).
Later target ownership is defined by the
[workspace architecture brief](plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md).
