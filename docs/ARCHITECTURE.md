# Architecture

Last updated: 2026-07-11

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict `.m4d` packages; source microscopy data enters through explicit
import/preprocessing workflows.

## Workspace Boundaries

The workspace has fourteen crates:

- `mirante4d-domain`: validated framework-neutral geometry, view, transfer,
  render-intent, and tool values.
- `mirante4d-identity`: strict typed scientific/package/artifact identities;
  no hashing or I/O.
- `mirante4d-project-model`: canonical durable project/view state and
  persistence-neutral generation projections.
- `mirante4d-application`: the sole command reducer, revision/history owner,
  transient semantic state, operations, events, snapshots, and typed faults.
- `mirante4d-settings`: closed settings document and bounded background I/O.
- `mirante4d-dataset`: immutable logical-layer catalog and explicit
  verified/unverified identity status.
- `mirante4d-render-api`: framework-neutral presentation and camera math.
- `mirante4d-data`, `mirante4d-format`, `mirante4d-import`,
  `mirante4d-analysis`, and `mirante4d-renderer`: current storage/runtime,
  format, import, analysis, and rendering implementations.
- `mirante4d-app`: native composition and egui shell.
- `xtask`: developer and verification tooling, never a product mode.

`mirante4d-core` and the predecessor application/session/preferences models
do not exist. Lower crates do not depend on the app/UI layer; the renderer
does not read files; format code does not own viewer state.

## Application Composition

`MiranteWorkbenchApp` holds `ApplicationState`, process diagnostics, seven
separate temporary runtime owners, and narrow persistence/settings/source-open
handles. It is a composition root, not a second model.

The temporary owners and deletion gates are:

| Owner | Scope | Gate |
|---|---|---|
| `CurrentDatasetRuntime` | workers, tickets, payloads, current data runtime | WP-08B |
| `CurrentRenderRuntime` | render status, frames, GPU and presentation resources | WP-09B |
| `CurrentUiRuntime` | egui-local drafts and interaction facts | WP-09C |
| `CurrentProjectRuntime` | current project package path only | WP-10B |
| `CurrentImportRuntime` | current import execution | WP-10C |
| `CurrentAnalysisRuntime` | current analysis execution and payloads | WP-12 |
| `CurrentValidationRuntime` | product-validation harness only | WP-14 |

The private egui bridge translates UI input to `ApplicationCommand` and reads
snapshots/events. The private project-v15 bridge is the sole temporary project
I/O route and has no compatibility reader. Both are mandatory-deletion
bridges, not permanent public APIs.

## Runtime Flow

```text
native package
  -> strict format validation and unverified logical catalog
  -> canonical application snapshot
  -> bounded shard/brick scheduling, cancellation, and leases
  -> resident CPU/GPU resources
  -> per-channel render intent and render cohorts
  -> renderer-owned GPU target
  -> egui-wgpu presentation, overlays, picking, and diagnostics
```

Small fixtures and large datasets use the same path. Whole-volume residency
for a tiny fixture is an optimization inside that path, not a second product
architecture. Missing occupied data is loading/incomplete, never empty.

## Persistence And Settings

Unverified sources are unbound workspaces. Project attach/open/save rejects at
the typed identity gate until WP-08 supplies verified scientific identity.
The private project-v15 actor exists only to exercise the future boundary; it
is deleted by WP-10B.

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

The exact enforced cutover contract is
[`architecture/wp07b-boundary-contract.json`](../architecture/wp07b-boundary-contract.json).
Later target ownership is defined by the
[workspace architecture brief](plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md).
