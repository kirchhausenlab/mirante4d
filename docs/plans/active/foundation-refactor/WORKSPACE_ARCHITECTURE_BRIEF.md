# Foundation Refactor — Workspace Architecture Brief

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-11
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: D-017 workspace graph, ownership, bridges, migration/deletion, enforcement, and operation classification

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

## Target Logical Architecture

```text
desktop-app (composition and native process lifecycle only)
├── ui-egui
│   └── typed commands out; immutable snapshots/events in
├── application-runtime
│   ├── project-model
│   ├── settings
│   ├── task/operation registry
│   └── workflow orchestration
├── project-store
│   └── transactional generations and durable identity
├── dataset-runtime
│   ├── demand scheduler and decoded-buffer admission
│   ├── CPU residency
│   └── byte-accounted resource leases
├── storage
│   ├── bootstrap metadata
│   ├── lazy indexes
│   └── chunk/shard access, codec decode, and writers
├── render-api
│   └── backend-neutral frame and resource contracts
├── render-wgpu
│   └── GPU residency, uploads, progressive frame execution, presentation
├── analysis-core
├── analysis-runtime
│   └── scheduler-controlled operations and derived artifacts
└── import-pipeline
    └── bounded inspect/decode/hash/statistics/write/validate/commit stages
```

These names describe ownership. The owner-approved D-017/D-018 crate and
cutover boundary below maps them to candidate package names, dependencies, and
deletion gates. It remains an implementation target rather than current source
fact until the approved work packages execute.

## Approved Workspace And Cutover Boundary

Status: OWNER-APPROVED TARGET POLICY under OD-021. D-002, D-017, and D-018 are
resolved. This section fixes the target architecture, publication timing, work-
package sequencing, and deletion ownership. It did not independently create a
branch, change repository visibility, modify production source, or authorize
implementation. WP-04 later completed the approved public-root cutover.

### D-017 Architecture Alternatives

| Alternative | Benefit | Foundation failure/risk | Recommendation |
| --- | --- | --- | --- |
| Reorganize only inside the existing eight crates | Least manifest churn | Cargo cannot prevent the same format/app/GPU/payload ownership leakage; app, renderer, and xtask remain broad authorities | Reject |
| Merge the rewrite into one new foundation crate | Easy initial moves | Renames the god object and creates an even wider public dependency surface | Reject |
| Dynamic plugins, services, or a generic event bus | Maximum theoretical substitution | Adds deployment, serialization, lifecycle, and debugging complexity to one desktop process without a product need | Reject |
| Big-bang replacement workspace | No temporary source bridges | Long test desert, unreviewable integration, and two architectures until the final switch | Reject |
| Ownership crates with a small explicit DAG | Cargo-enforced authority, independent tests, bounded adapters, and staged deletion | More packages and deliberate migration work | Select |

Crates are justified only where authority, dependency direction, persistence
lifecycle, side effects, or resource ownership differs. This is not a target
file count, and no empty package may be created merely to reserve a name.

### Proposed Final Production Graph

Each row lists the crate's sole authority and its permitted normal Mirante4D
dependencies. External dependencies receive equivalent owner restrictions.

| Crate | Sole authority | Permitted Mirante4D dependencies |
| --- | --- | --- |
| `mirante4d-domain` | Axes, dtype, shapes, transforms, logical keys, and small framework-neutral product-domain values | None |
| `mirante4d-identity` | D-009 typed identities, raw-object descriptors, canonical encodings, and Merkle primitives | `domain` |
| `mirante4d-dataset` | Immutable logical catalog, semantic resource keys, opaque payload-view/lease contracts, and dataset-source contract; it cannot issue/account leases | `domain`, `identity` |
| `mirante4d-project-model` | Canonical durable `ProjectState`, `ProjectRevisionId` and revision invariants, dataset/artifact references, and persistence-neutral generation projections | `domain`, `identity` |
| `mirante4d-settings` | Validated user preferences, versioned settings DTO/store, background atomic writes, and resource-reconfiguration intent | `domain` |
| `mirante4d-storage` | Strict target M4D profile, bounded validation, lazy indexes, Zarr shards, and dataset readers/writers | `domain`, `identity`, `dataset` |
| `mirante4d-project-store` | D-010 objects, generations, refs, leases, autosave, recovery, and GC | `domain`, `project-model`, `identity` |
| `mirante4d-dataset-runtime` | One demand scheduler, decoded-buffer admission, CPU ledger, caches, deduplication, cancellation, and sole lease issuance/lifetime | `dataset` |
| `mirante4d-render-api` | Backend-neutral render intent, requirements, frame identity, coverage, and presentation contracts | `domain`, `dataset` |
| `mirante4d-render-wgpu` | Sole production GPU owner: device, pipelines, VRAM, staging, uploads, execution, and presentation | `render-api`, `dataset` |
| `mirante4d-analysis-core` | Pure typed operations, algorithms, tables, plots, recipes, and artifact payloads | `domain`, `dataset`, `identity` |
| `mirante4d-analysis-runtime` | Bounded analysis execution over shared leases, producing pending artifacts | `analysis-core`, `dataset-runtime`, `project-model` |
| `mirante4d-import-pipeline` | Bounded inspect/decode/hash/multiscale/write/verify/commit stages | `domain`, `identity`, `dataset`, `storage` |
| `mirante4d-application` | Sole reducer, live revision/history pointer and transitions, operation registry, orchestration, commands, application snapshots, and events | `domain`, `identity`, `dataset`, `analysis-core`, `project-model`, `settings`, `project-store`, `dataset-runtime`, `render-api`, `analysis-runtime`, `import-pipeline` |
| `mirante4d-ui-egui` | Widgets, layout, framework-input translation, dialogs, and snapshot/texture presentation | `application` |
| `mirante4d-app` | Process lifecycle, capability diagnosis, dependency construction, presentation bridging, window/log paths, launch, and shutdown only | `application`, `ui-egui`, `render-api`, `settings`, `storage`, `project-store`, `dataset-runtime`, `render-wgpu`, `analysis-runtime`, `import-pipeline` solely for construction/bridging |

An unpublished workspace-only `mirante4d-render-reference` crate owns the
deterministic CPU oracle/export/test implementation and may depend only on
`domain`, `dataset`, and `render-api`. The normal product application must not
depend on it, mechanically preventing a silent CPU viewer fallback.

`xtask` remains repository orchestration, not a product library or headless
viewer. Final benchmark/oracle/fixture logic lives with the owning crate or the
CPU reference target; `xtask` invokes those commands and audits their reports
instead of normally importing the entire production graph. No speculative
generic `common`, `utils`, `prelude`, or test-support crate is approved.

### Boundary Invariants

- UI creates framework-neutral application commands and reads immutable
  snapshots/events only. Egui/winit coordinate types stop at `ui-egui`.
- `mirante4d-app` contains no product state, algorithms, synchronous dataset or
  project work, render planning, cache mutation, or durable DTOs.
- Storage implements codec interpretation/decompression and the dataset-source
  contract. Dataset runtime owns scheduling, decoded-buffer reservation/
  admission, byte accounting, caching, and lease lifetime but never imports
  Zarr, package paths, OME/M4D DTOs, physical chunk/shard records, or codecs.
  The source contract decodes into a runtime-reserved buffer or equivalent
  reservation-bound sink, so storage cannot allocate a large decoded payload
  outside the ledger.
- Semantic resource keys cross boundaries. `NativeManifest`, physical brick/
  shard indexes, and storage object names never enter renderer, analysis,
  application, project-model, or UI APIs.
- Every large decoded payload has one immutable `Arc`-backed owner and an
  accounted lease. Cache hits, scheduler fan-out, renderer, and analysis do not
  deep-copy pixel buffers.
- Dataset runtime owns all CPU demand, priorities, cancellation, deduplication,
  queues, decoded-buffer admission, lease issuance/lifetime, and byte ledgers
  for 3D, cross-section, playback, analysis, and prefetch. The `dataset` crate
  defines opaque lease/view contracts and the dependency-inverted CPU ledger
  admission interface but cannot issue or account them. Storage and import
  receive that interface by injection; analysis obtains an opaque accounted
  byte lease through dataset runtime. Only dataset runtime may implement the
  ledger in production.
- `render-wgpu` alone may create pipelines/resources, mutate VRAM residency,
  upload, submit GPU work, or own presentation targets. It consumes semantic
  leases rather than storage-shaped volumes.
- `render-api` owns a framework-neutral opaque presentation token and lifecycle.
  `render-wgpu` retains the texture/resource; `ApplicationSnapshot` carries only
  the token; `ui-egui` emits a paint request; and the composition root installs
  the sole egui-wgpu bridge that resolves the token without transferring GPU
  ownership or exposing `wgpu`/egui types through application contracts.
- Analysis cannot scan a concrete dataset handle or commit project files. It
  returns a pending typed result; the application verifies revision/source
  identity and the project store publishes it transactionally.
- Dataset storage and project storage remain distinct authorities and
  persisted lifecycles.
- `project-model` owns semantic durable `ProjectState` and generation
  projections plus `ProjectRevisionId` and revision invariants, but no on-disk
  schema, paths, locks, or backend/runtime handles; `project-store` alone maps
  them to versioned project DTO bytes. Application exclusively owns the live
  revision/history pointer, undo/redo movement, command transitions, and
  transient/UI-facing `ApplicationSnapshot`/events.
- `settings` is the separate preferences lifecycle and filesystem authority.
  Application applies validated reconfiguration commands; UI and composition
  code do not write settings or mutate resource budgets directly.
- Import cannot depend on application, UI, runtime, renderer, or project store;
  source formats never become product dataset readers.
- No compatibility facade crate, indefinite re-export alias, second product
  route, or backend/format fallback survives a cutover.

### Current-To-Target Migration And Deletion Map

| Current authority | Target owner(s) | Cutover/deletion owner |
| --- | --- | --- |
| `mirante4d-core` | `domain`, `identity`, `project-model` | Move rather than copy and delete current core at WP-07B |
| App durable state, commands, project model | `project-model`, `application` | WP-07B makes the new reducer/model sole authority |
| App project/session store | `project-store` | WP-10B switches save/open and deletes old persistence |
| App preferences/configuration persistence | `settings` plus application reconfiguration | WP-07B hard-cuts the experimental preferences path and deletes app-local settings DTO/I/O |
| `mirante4d-format` | `storage`, `identity`, `dataset` | Target remains off-product through WP-10A; delete current format at WP-10C |
| `mirante4d-data` | `dataset`, `dataset-runtime`, current-source bridge | WP-08B moves runtime ownership; WP-10C deletes the current-source bridge and old crate |
| `mirante4d-renderer` | `render-api`, `render-wgpu`, dev-only `render-reference` | WP-09A builds off-product; WP-09B switches the sole renderer and deletes the old crate/path |
| `mirante4d-analysis` and app analysis jobs | `analysis-core`, `analysis-runtime` | WP-12 switches the sole analysis path and deletes predecessors |
| `mirante4d-import` and app import execution | `import-pipeline` | WP-11 builds replacement off-product; WP-10C activates it and deletes the old importer |
| App UI/workbench orchestration | `ui-egui`, `application`, composition-only `mirante4d-app` | WP-09C cuts the shell and deletes remaining god-state/orchestration modules |
| Phase/report/product helper logic in `xtask` and app | Owning test/bench targets plus slim `xtask` orchestration | WP-06 inventories/splits; WP-14 deletes superseded tooling |

### Bounded Transitional Source Bridges

Staged replacement requires a runnable single-route product between cutovers.
The following private, statically wired, one-way bridges are approved target
policy under resolved D-017/D-018:

| Bridge | Predecessor/composition placement and only temporary edge | Authority/buffer rule | Exists after | Mandatory deletion |
| --- | --- | --- | --- | --- |
| Canonical state to current project persistence | Private module in current `mirante4d-app`; old app may depend on `application`/`project-model` | Application owns revision/state; predecessor owns only current-format I/O and maps typed faults | WP-07B | WP-10B |
| Unified runtime to current dataset source and identity verification | `mirante4d-data::current_source_bridge`; old data may depend on target `dataset`/`identity` while retaining its old-format edge | Dataset runtime reserves buffers/issues leases; bridge owns current codec/source translation and the bounded D-009 verification cache below | WP-08B | WP-10C |
| Unified lease views to current renderer | Private module in current `mirante4d-renderer`; old renderer may depend on target `dataset`/`render-api` | Dataset runtime retains/account buffers; predecessor renderer borrows semantic views and may not clone them | WP-08B | WP-09B |
| Canonical application boundary to current egui shell | Private module in current `mirante4d-app`; old shell may depend on `application` | Application owns state/tasks; shell translates framework input and renders snapshots only | WP-07B | WP-09C |
| Current analysis implementation to unified scheduling | Private module in current `mirante4d-app`; old app may depend on target `dataset-runtime` while invoking current analysis | Dataset runtime owns demand/buffers/cancellation; bridge maps pending typed results/faults, never commits artifacts itself | WP-08B | WP-12 |
| Current importer route while replacement is off-product | Composition routing in current `mirante4d-app`; no old-to-new Cargo edge or alternate UI route | Current importer remains sole reachable producer; target pipeline is test-only/unreachable | WP-11 | WP-10C switch/deletion |

These are internal migration seams, not persisted compatibility readers or
user-selectable old/new implementations. Each has one statically reachable
product route, no feature flag, fallback, hidden UI, alternate reader/writer,
or mirrored durable authority. A bridge cannot cross its named deletion gate;
failure to remove it reopens the owning work package.

Target foundation crates never import a predecessor crate. WP-08A generates an
exact transient normal/dev/build dependency allowlist for every checkpoint;
only the old-to-new edges in the bridge and predecessor tables are permitted,
and each rule carries its deletion gate. A cutover cannot delete a crate while
any still-live normal, dev, build, test, benchmark, or tool target depends on
it.

The WP-07B move of current core types also creates these explicit predecessor-
to-owner edges; they are type relocation, not additional product-route bridges:

| Still-live predecessor | Permitted transient Mirante4D dependencies | Expiry/condition |
| --- | --- | --- |
| `mirante4d-format` | `domain` | Delete at WP-10C |
| `mirante4d-data` | `domain`, `dataset`, `identity`, current `format` | Delete at WP-10C |
| `mirante4d-renderer` | `domain`, `dataset`, `render-api`; current `data`/`format` only until the WP-10C prerequisite below | Delete at WP-09B |
| `mirante4d-analysis` | `domain`, current `data`, and target `dataset` only where the WP-08B bridge needs semantic views | Delete at WP-12 |
| `mirante4d-import` | `domain`, current `format` | Delete at WP-10C |
| current `mirante4d-app` shell | Eligible target boundary/service crates plus still-live predecessors solely through the six bridge/composition roles | All bridge edges expire at their table gates; shrink to final app graph at WP-09C |
| current `xtask` | Only still-live owners needed by an inventoried command | Owning WP-06/WP-14 replacement removes each edge before its target is deleted; final normal product-crate edge set is empty |

Any dependency not in the final graph, bridge table, or this predecessor table
is forbidden. The per-checkpoint generated matrix narrows these maxima to the
edges actually needed; it cannot use this table as a blanket exception.

The current renderer bridge is zero-copy: WP-08B/WP-09A first move every
renderer-facing key/view/region type and fixture into `mirante4d-dataset`, then
the predecessor renderer borrows the immutable lease payload. It has no normal,
dev, build, or test dependency on `mirante4d-data`/`mirante4d-format` by WP-10C.
An owning-copy exemption is not implied; if zero-copy proves impossible, the
resource contract and D-017 must be reopened rather than hiding a temporary
unaccounted copy.

#### Current-Source Identity Bridge Before WP-10C

WP-10B cannot bind current packages to D-009 identity merely from their
filename or serialized manifest. Before WP-10B activates project save/open, the
WP-08B current-source bridge provides a background, cancellable, resumable,
byte-bounded scientific-identity operation. It streams canonical base-scale
samples/validity through runtime-reserved buffers, validates the current
encoded-object checksum inventory while reading, reports exact progress, and
produces a verified `ScientificContentId`.

A machine-local, non-portable cache may reuse that result only after the full
current manifest/checksum inventory, normalized object set, sizes, and encoded-
object digests are reverified. A missing checksum, object/stat change, checksum
mismatch, source mutation, or unverifiable cache record invalidates it and
requires recomputation. The cache is acceleration, never package/release
authority, and is deleted with the bridge at WP-10C.

The viewer may remain open with truthful `identity verification required/in
progress` status, but project attachment, restore, and save remain unavailable
until verification succeeds for the currently opened source generation.
Cancellation/failure leaves the project dirty/unbound rather than accepting a
self-declared ID. WP-10C replaces this bridge with the approved target package
and identity verification path.

### Architecture Enforcement Cutover

WP-06/WP-08A replace the line-count-centric gate with:

- a `cargo metadata`-derived exact normal-dependency matrix for every target
  crate, with normal/dev/build edges classified separately;
- direct external-dependency ownership: Zarr and dataset-package filesystem
  access only in storage; project-package filesystem/locking only in project-
  store; TIFF/XML/source reads and import staging only in import; preferences
  paths/I/O only in settings; log/process/config paths only in app composition;
  file dialogs and egui/eframe only in UI/process shell; and production WGPU
  implementation APIs only in `render-wgpu`;
- public-API checks forbidding storage DTOs/paths, owning pixel `Vec`s, worker
  handles, renderer frames, `wgpu`, or egui types across frozen contracts;
- compile/contract tests proving source substitution and crate independence;
- one owner and resource-ledger category for every large allocation, queue,
  worker, GPU resource, and persisted object; and
- a complete old-type/module/crate deletion ledger plus product-open evidence
  after every product-facing cutover.

Source/file size remains diagnostic evidence for cohesion review, not a proxy
for architecture correctness or a blind hard failure.

### D-018 Alternatives Rejected

| Alternative | Rejection reason |
| --- | --- |
| One big rewrite branch | Longest divergence, weakest review, hidden integration, delayed deletion |
| Long-lived `develop`/milestone branch | Creates a second authority and keeps `main` deceptively stale |
| Runtime strangler flags | Retains two product architectures and violates hard cutover |
| One branch per entire work package | WP-06/07B/08B/09/10/11/12/14 are too large for one reviewable lifetime |
| Direct trunk microcommits | Places partially migrated authorities on canonical `main` |
| Feature branches stacked on feature branches | Obscures eligibility and makes rollback/rebase evidence ambiguous |
| Begin main refactor before the public-root transition | Creates twin canonical histories across the clean-repository boundary |

### Workspace And Cutover Resolution

The owner approved D-017 and D-018 together on 2026-07-09 through OD-021:

1. adopt the sixteen-crate ownership graph, dev-only CPU reference, exact
   dependency direction, resource/API boundaries, and current-to-target
   deletion map;
2. permit only the six named one-way migration bridges with one reachable
   product route and mandatory deletion gates;
3. replace line-count architecture enforcement with dependency, public-API,
   side-effect, resource-owner, and deletion-ledger checks;
4. adopt the two-epoch strategy, making the clean public repository canonical
   before WP-05 and publishing immediately after WP-02/WP-03 readiness subject
   to the existing final visibility approval; and
5. adopt short-lived squash-checkpoint branches on one protected `main`, the
   corrected serial integration order, annotated exit tags, atomic
   product-cutover revisions, and revision/deployment rollback without a
   product fallback.

WP-03 completed public-source readiness, WP-04 completed the clean public-root
transition, and WP-06 installed the accepted verification topology. The WP-07A
candidate freezes the first pure model boundary; after its protected-main
acceptance, the remaining technical sequence begins with WP-07B.

This approval fixes the architecture and execution topology, not every internal
module name or checkpoint size. The WP-07A candidate freezes the canonical
model API, dependency/side-effect allowlists, and current-field ledger. WP-08A
must still freeze the subsystem API, resource ownership, and checkpoint
decomposition before its source movement.

## Operation Classification

| Operation class | UI/input handler | Once per frame | Background/runtime | Forbidden during interaction |
| --- | --- | --- | --- | --- |
| Typed command creation and validation | Allowed when bounded | Allowed | Allowed | No |
| Immutable snapshot rendering | Allowed when bounded | Allowed | N/A | No |
| Dataset/project metadata open | Enqueue only | Poll completion only | Required | Synchronous execution |
| Chunk/shard I/O and decode | Enqueue demand only | Drain bounded completions | Required | Synchronous execution |
| Resource planning | Small view-local math only | Allowed under measured budget | Heavy/global planning | Unbounded scans/sorts |
| GPU upload/render submission | Enqueue intent only | Bounded submission by render runtime/callback, not egui interaction code | Preparation allowed | Unbounded upload/readback waits |
| Project save/autosave | Enqueue only | Poll completion only | Required | File transaction execution |
| Import/preprocessing | Enqueue/cancel only | Progress snapshot only | Required | Source scan/decode/write |
| Analysis | Enqueue/cancel only | Progress/result snapshot only | Required | Dataset-scale execution |
| Worker shutdown/replacement | Request only | Poll completion only | Required | Blocking joins |
| Product capture/readback | Explicit validation/export only | Never implicit | Controlled task | Normal interaction path |

The final handoff must refine budgets and the permitted view-local operations
using measurements on named hardware.
