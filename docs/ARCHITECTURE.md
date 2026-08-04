# Architecture

Last updated: 2026-08-04

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict local packages and imports TIFF sources through a bounded preprocessing
pipeline. The product has one path for small and large data.

## Workspace Ownership

The workspace has seventeen `mirante4d-*` crates and `xtask`.

| Package | Sole responsibility |
| --- | --- |
| `mirante4d-domain` | Validated framework-neutral geometry, view, transfer, render-intent, and tool values. |
| `mirante4d-identity` | Typed identities, normalization, and pure hash/tree primitives. It performs no filesystem I/O. |
| `mirante4d-project-model` | Canonical durable project and workspace state. |
| `mirante4d-project-store` | Immutable project objects, generations, references, leases, and recovery. |
| `mirante4d-application` | Commands, reducer state, operations, events, history, and typed application faults. |
| `mirante4d-settings` | The closed settings document and bounded settings I/O. |
| `mirante4d-dataset` | Dataset catalog, semantic resource keys, payload views, leases, and source/decode contracts. |
| `mirante4d-dataset-runtime` | The production request scheduler, CPU byte ledger, workers, cancellation, progress, and decoded leases. |
| `mirante4d-analysis-core` | Pure exact intensity operations and canonical artifact payloads. |
| `mirante4d-analysis-runtime` | Bounded analysis execution over dataset-runtime requests. |
| `mirante4d-render-api` | Backend-neutral render intent, requirements, camera math, numerical admission, presentation, and Pick contracts. |
| `mirante4d-render-reference` | A bounded test-only CPU correctness oracle. |
| `mirante4d-render-wgpu` | The only product renderer and GPU residency owner. |
| `mirante4d-storage` | Package profile, admission, bounded reads, audits, identities, and create-only publication. |
| `mirante4d-import-pipeline` | Bounded, cancellable, restartable TIFF and OME-TIFF production. |
| `mirante4d-ui-egui` | Shared egui visuals, message projection, and transient widget state. |
| `mirante4d-app` | Native process, service composition, product automation, and presentation-token resolution. |
| `xtask` | Development, verification, fixture, and package tooling. It is not a product mode. |

Lower crates do not depend on the native app or UI. The renderer does not
read files. Storage does not own viewer state. Egui does not own product
threads, source paths, durable state, or resource scheduling.

## Native Composition

`MiranteApplicationShell` is the process composition root. It owns selected
adapter facts, settings, the process CPU broker, preprocessing, package-open
transactions, and an optional dataset workbench.

The shell starts in a dataset-independent Welcome state. Cancelling a chooser
keeps the window open. A successful package open creates the dataset catalog,
runtime, renderer session, application state, and workbench. A failure returns
to the dataset-independent shell. There is no dummy catalog or fallback
session.

`MiranteWorkbenchApp` composes application state, demand state, renderer
state, project services, diagnostics, and the egui projection. It is a
composition root, not a second model. The application reducer owns durable
commands and product revisions. Egui owns only transient interaction drafts.

The normal native process and product automation use the same product route.
Test builds can provide bounded control and capture inputs. They cannot select
a different renderer, data path, or state model.

## Import And Publication Flow

```text
explicit source manifest
  -> bounded metadata inspection
  -> guarded TIFF decode
  -> current unit cache
  -> scientific/base/pyramid production
  -> bounded encoded spool
  -> resumable final-layout stage
  -> validation
  -> create-only publication
  -> ordinary package-open boundary
```

Each channel row names one source kind and its complete ordered file set.
Inspection reads metadata only. It is cancellable and generation checked. It
does not infer hierarchy from filenames or decode the full payload.

The process CPU broker owns managed-byte admission. Dataset work installs a
foreground reserve. Import retains one non-stealable progress lane and borrows
only surplus capacity for other work. Purpose labels report use but do not
create fixed percentage partitions.

One canonical import coordinator owns the current `(timepoint, channel)` unit,
scientific hashing, spool, outer shards, journals, and publication order. It
can admit one future decode cache when CPU, descriptor, and disk headroom
remain after current progress is protected. The future worker consumes one
slot from the same process parallelism limit. It cannot write a spool, stage,
hash, journal, or package.

Automatic no-data resolution for channel zero and timepoint zero completes
before future decode starts. The resolved typed value, spatial mask, constant
planes, plan binding, and source generation are durable checkpoint facts.

The current unit produces base and pyramid values in deterministic order.
Eligible internal work can use byte-admitted workers. One owner commits
results. The active cache, one optional future cache, one bounded spool, and
compact control records are the checkpoint authority. Work does not grow with
the total timepoint count.

The final-layout stage writes package-relative outer shards directly. It keeps
a durable compact prefix and removes an incomplete suffix on recovery. A
capacity failure keeps valid staged work. Corruption or a source/plan mismatch
requires confirmed reset.

Publication validates the staged package, completes its durability order, and
renames it only to an absent destination. A successful same-directory
capability can transfer validated facts directly to product admission. A
later external open remains an independent ordinary open.

Import never writes source microscopy data.

## Package Admission And Reads

`mirante4d-storage` owns the strict package catalog and local source. Package
open validates paths, object types, lengths, schemas, profiles, manifests, and
bounded indexes. It does not decode every payload before the workbench opens.

The source retains one package-root descriptor. Linux `openat2` constraints
reject symlink and magic-link traversal. A bounded authenticated object-handle
cache reuses safe readers and decoded shard indexes.

Every consumed object receives exact encoded-integrity and pre/post-use
currentness checks. Packed facts from an external package are acceleration
hints until the related payload has passed its first required check. A
successful import capability can transfer facts that the same publication
already checked.

Physical chunks can serve several semantic regions. A bounded physical cache
coalesces concurrent consumers. Full aligned bricks can decode directly into
runtime-owned output spans. Codec workspace ends before a decoded resource
lease survives.

The explicit full package audit walks encoded objects, base content, and
packed facts with bounded progress. It reports self-consistency. It does not
authenticate authorship or change source and presentation authority.

## Dataset Runtime

The application creates semantic demand for 3D, linked 2D, playback,
histogram, readout, and analysis scopes. `mirante4d-dataset-runtime` merges the
deduplicated union and admits its exact managed bytes once.

The runtime uses one bounded lazy priority scheduler. Each logical resource
has one job identity and can have several waiters. Cancellation removes only
the cancelled scope. Decoded payloads remain byte-charged while leases exist.

Foreground demand can interrupt one lower-priority decode at a source
checkpoint. The interrupted logical job, waiters, and deduplication identity
remain. Capacity release wakes eligible work through generation predicates;
the scheduler does not depend on timeout polling.

Results bind their request generation and immutable body. The application
rejects stale results and preserves useful installed work. A failure names its
resource, scope, and phase. Capacity refusal cannot silently change product
semantics.

## Viewer Demand And LOD

Application demand is split into 3D, linked-plane, playback, and analysis
scopes. Each visible layer uses its affine grid and physical view to select an
ideal scale. A global selector then admits one feasible per-layer map against
the exact CPU and GPU union.

Static 3D demand retains a complete terminal-to-fine ladder. The terminal
shape has a maximum dimension of 64 or less and at most 262,144 voxels. The
selector adds finer coherent full-volume maps in deterministic max-min layer
order while the global resource limit permits them.

During a gesture, the work controller selects the finest complete resident
body that is safe for native-resolution interaction. The selection is frozen
for that gesture. A complete coarse body can remain visible while hidden exact
work advances. No partial volume frame becomes current.

Linked planes have a complete coarsest full-volume floor and a selected plane
target. The renderer can sample already-resident intermediate scales, but
they are not load dependencies. Plane output can publish from complete floor
coverage and refine as selected bricks arrive. Volume output requires one
complete uniform body.

The render-intent mailbox owns latest transient camera and linked-plane input.
It allocates render frame identities and keeps independent 3D and linked
identities. Egui does not own a second camera or currentness model. Durable
commit adopts the final transient value without making an extra frame.

## GPU Residency And Rendering

The renderer has one private `ResidencyOwner`. It owns:

- the segmented storage-buffer arena;
- exact payload allocations and page records;
- one sparse logical-key directory;
- resident maps, pins, and LRUs;
- bounded staging and compaction scratch;
- eviction and reoffer events; and
- GPU-memory growth and placement facts.

The application holds opaque frame leases. It does not mirror GPU-resident
sets, allocation state, or page tables.

Arena growth preserves offsets and bytes. It remains within configured
logical and physical limits. Placement preflight distinguishes total free
bytes from the largest contiguous span. Stable in-segment compaction can run
once before a typed refusal returns to the generic scale selector.

Plane, MIP, DVR, ISO, Mixed, and Pick use dedicated pipelines over shared
accepted bindings, sampling, and volume traversal. Multichannel DVR can use
one joint medium for compatible layers. Mixed rendering dispatches explicit
per-layer modes without redefining homogeneous algorithms.

The render API validates binary32 affine condition, inverse error,
coordinates, work bounds, camera geometry, rays, and page traversal before
shaders consume them. Shader paths use the same direction, sample, page-exit,
coverage, validity, and Pick rules.

One private `FrameCoordinator` owns target fronts, texture revisions,
recording order, color submission, hidden candidates, completion, Pick, and
capture freshness. Changed active targets record before passive targets in one
encoder and one queue submission. A complete exact 3D replacement swaps only
after its coordinated submission completes.

Hidden exact rendering uses bounded row batches on a latest-only worker.
Partial rows do not publish a texture, capture, Pick target, or frame revision.
Cancellation occurs between submissions. A newer request waits behind at most
one submitted batch.

Initial color pipelines and Pick compile asynchronously. Capability state is
separate for color and Pick. Local Pick failure resolves pending picks with a
typed result and preserves color. Device-level failure uses the renderer's one
terminal first-cause latch.

## Composed Presentation And Playback

The application `ComposedPresentationScheduler` owns semantic coordinates for
time, 3D space, linked space, and retained quality. One transaction snapshots
the active target set, extents, and surface generations.

All affected semantic members must be prepared, reused, or terminal no-work
before publication. The renderer verifies private-front reuse, derives the
physical delta, and owns the atomic GPU submission and texture swap. A true
zero delta performs no GPU work.

Playback creates one immutable `PlaybackSession`. Admission selects one
full-volume per-layer scale map for the requested rate, layout, memory, and
runway. The session retains a bounded rotating slot ring. Time-axis length does
not change steady-state memory.

A successor timepoint publishes only when its complete fixed-scale bundle is
resident and renderable. A late successor holds the previous frame and
applies clock backpressure. It does not skip, blank, flash a coarse frame, or
publish a partial layout.

Pause and Stop release future playback demand but keep the last coherent
front. A stationary transaction replaces that front only when its complete
active layout is ready.

## Analysis, Projects, And Settings

`mirante4d-analysis-core` owns exact `uint8`, `uint16`, and finite `float32`
statistics. `mirante4d-analysis-runtime` uses the shared scheduler with a
bounded two-block window, lower priority, scoped cancellation, and stale
suppression.

The application commits a table and plot as one atomic artifact bundle. The
project model stores authenticated references. Project reopen restores only a
complete pair.

The project-store actor owns roots, sessions, immutable objects, generations,
refs, held leases, autosaves, and recovery. The application service is the
only product route. UI frames never perform project filesystem mutation.

The settings service owns one closed settings document and bounded background
I/O. Persisted settings are explicit user overrides. Selected-adapter memory
facts supply recommendations but do not silently replace stored settings.

## Shutdown And Failure Ownership

Native close, SIGINT, and SIGTERM converge on one monotonic close request.
`on_exit` owns cancellation and joins. Dirty window close first requires Save,
Discard, or Cancel. There is no second close loop, guardian, fallback kill, or
parallel shutdown owner.

Large work is bounded in memory, VRAM, queues, workers, handles, descriptors,
I/O, and physical objects. It is cancellable and suppresses stale results.

Recoverable resource and readiness outcomes preserve the last valid product
state and name the event that can make progress. Stable deterministic failure
is quiescent. Terminal device failure stops later unsafe GPU work. No fault
silently selects a legacy, dense, CPU, or alternate path.

## Guardrails

- Source microscopy data is read-only.
- Incomplete output never appears complete.
- One owner controls each durable or live identity.
- Storage remains sharded with bounded physical-object count.
- Scientific conformance uses independent facts or an independent reader.
- Product rendering uses one WGPU renderer and one residency authority.
- The CPU renderer remains test-only.
- Persisted formats have no compatibility promise during pre-alpha work.
- Segmentation remains absent.
- Private dataset facts and machine paths stay outside public records.
