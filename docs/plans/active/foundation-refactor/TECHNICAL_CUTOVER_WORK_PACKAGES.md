# Foundation Refactor — Technical Cutover Work-Package Catalog

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-11
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: WP-07A through WP-12 and WP-14/WP-15 package contracts; package definitions occur only here

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

## Package Definitions

Before implementation, each package receives a short entry record that freezes
its exact current paths, commands, checkpoints, datasets, thresholds, and
evidence artifacts. The entry specializes those details but cannot weaken,
narrow, expand, or replace the required work or exit proof below.

### WP-07A — Canonical Model Contract

Goal: approve the canonical domain/project/view vocabulary before migrating
product authority.

Required work:

- Introduce real `domain`, `identity`, and `project-model` code only as their
  contracts/tests are defined; do not create empty target crates.
- Define canonical dataset, project, layer, channel, view, tool, artifact, and
  task/operation concepts.
- Define which facts are durable, transient UI state, runtime snapshots, or
  derived diagnostics.
- Specify active selection by ID and central command/reducer invariants.
- Specify project serialization DTO boundaries without renderer/runtime types.
- Prove the model with pure contract/property tests; do not add a second live
  model to the product.

Exit proof:

- The contract and ownership ADRs are approved.
- Exact API schemas, public dependency/side-effect allowlists, and the complete
  current-field-to-owner/deletion ledger are frozen for WP-07B.
- Every current `AppState` field has one target owner or an explicit deletion
  disposition.
- No downstream API-changing state decision is unresolved.

### WP-07B — Application API And Durable-State Cutover

Goal: make the WP-07A model the sole durable authority and establish the typed
application command/event/snapshot boundary.

Required work:

- Implement typed commands, reducer/state transitions, events, runtime
  `ApplicationSnapshot`s, operation IDs, and fault envelopes; durable
  `ProjectState`/generation projections remain project-model concepts.
- Remove active-layer mirrors and duplicated durable facts.
- Move workers, decoded payloads, residency, GPU resources, frames,
  diagnostics, and transient errors out of durable state. The WP-07B execution
  brief names their temporary non-durable owner until WP-08/WP-09 and its
  mandatory deletion/transfer gate; no new general runtime god-state is allowed.
- Cut project/session serialization to canonical durable state only.
- Move current core types into their approved owners, update every consumer,
  and delete `mirante4d-core` rather than retaining a re-export facade.
- Introduce `mirante4d-settings`, hard-cut the experimental preferences schema,
  move settings I/O off the UI thread, and route validated resource-policy
  reconfiguration through application commands. The entry brief freezes its
  exact identity/path/atomic-write/rejection contract and never deletes or
  silently migrates an old user file.
- Adapt retained UI actions to the application API without claiming that the
  later direct-I/O/render/runtime UI removal is complete.
- Keep exactly one private application-to-current-project-persistence bridge
  until WP-10B and one application-to-current-egui-shell bridge until WP-09C;
  neither may mirror authority or expose an alternate route. The entry brief
  freezes each exact module path, dependency direction, visibility, reachability
  test, owner, and expiry.
- Delete the old god-state mutation/synchronization authority in the same
  cutover; no mirrored legacy `AppState` may remain as a second authority.

Exit proof:

- Durable state has one source of truth for each fact.
- Project round trips cannot serialize runtime resources or active mirrors.
- Invalid transitions are rejected centrally.
- Temporary WP-06 adapters are deleted.
- String-only cross-boundary workflow/runtime faults and duplicated ad hoc error
  fields are deleted or confined behind the approved typed fault boundary.
- The entry-stamped named product-open scenario proves project/state/UI behavior
  on the exact package and project identities, and logs show no fallback,
  mutation of rejected old inputs, panic, or repeated retry loop.
- Preparatory types may merge only while unreachable; the live model/serialization
  activation and predecessor deletion occur in one atomic checkpoint with no
  dual live authority or re-export facade.

### WP-08A — Subsystem Contract And Ownership Freeze

Goal: freeze implementation-independent seams before storage, runtime, and
render work proceeds in parallel.

Required contracts:

- exact D-017 crate dependency and external side-effect ownership matrices;
- resource keys, immutable value-plus-validity payload views, and lease
  semantics;
- runtime-reserved decode-buffer/sink semantics and the rule that only dataset
  runtime issues/accounts lease lifetimes while storage owns codecs;
- byte-ledger categories and the sole owner of every large CPU/GPU allocation;
- request priorities, scope-isolated cancellation generations, runtime-owned
  request IDs, deduplication, and shutdown;
- bounded runtime configuration, immutable diagnostics, and per-request
  progress;
- render intent, resource requirements, frame identity, and progressive
  coverage/completeness semantics;
- opaque presentation-token registration/update/retirement across render-wgpu,
  application snapshots, composition, and UI without backend/UI type leakage;
- storage/catalog interfaces and stable content-identity boundaries;
- typed operation/fault envelopes;
- forbidden dependencies among domain, storage, runtime, renderer, project,
  analysis, UI, and the composition root.

Exit proof:

- Contract tests run against in-memory/fixture sources.
- Contracts expose neither the current `NativeManifest`, concrete Zarr objects,
  owning `VolumeBrick*` values, current pools, `egui`, nor `wgpu`.
- Every large buffer/resource class has one owner and one ledger category.
- Mechanical dependency checks enforce the approved graph.
- Public-API checks prevent storage/backend/UI/owning-payload leakage across
  the frozen contracts.
- Changing a frozen contract reopens WP-08A and every affected downstream exit.

### WP-08B — Unified Dataset Resource Runtime

Goal: create one scheduler and one byte-accounted CPU resource authority for
interactive data demand.

Required work:

- Introduce shared immutable payloads and resource leases.
- Move renderer-facing semantic keys, regions, payload views, and test fixtures
  into the frozen `dataset` contract; refactor the still-live renderer bridge
  to borrow them without depending on current data/format crates.
- Share immutable dataset metadata rather than cloning manifests per worker.
- Implement byte-weighted bounded ingress, in-flight, completion, prefetch, and
  residency accounting.
- Deduplicate demand by resource key and define priority classes for active
  view, linked views, playback, and warm work.
- Implement cancellation generations and explicit overload/capacity results.
- Move 3D, cross-section, playback, histogram, and retained interactive readout
  demand to the same scheduler. Analysis remains an explicitly blocked legacy
  consumer until WP-12; it may use only a narrow temporary adapter with a WP-12
  deletion gate.
- Bind the product runtime through the one current-storage-source bridge until
  WP-10C and the one lease-to-current-renderer bridge until WP-09B. Neither
  permits a second source/renderer choice.
- Provide the reservation-bound source stream and progress/cancellation/cache
  hooks needed for the D-009 current-source verification operation before
  WP-10B; the normative algorithm lands after WP-10A identity rules exist.
- Instrument work directly per operation/request.

Exit proof and deletion gate:

- Concurrent identical demand decodes once and fans out leases.
- Stress tests prove total accounted CPU bytes stay within approved tolerance.
- Result delivery is bounded; stalled UI consumption cannot accumulate
  unbounded payloads.
- Priority, cancellation, deduplication, and stale-result tests pass.
- Old brick/cross-section pools, unbounded result channels, active resident
  mirrors, count-only prefetch ownership, and app-owned interactive payload maps
  are deleted when the interactive cutover closes.

### WP-09A — Progressive Render Runtime On Fixture Leases

Goal: implement one GPU owner and progressive rendering against WP-08A fixture
resource leases without creating a second product path.

Required work:

- Implement backend-neutral render intent, resource lease, frame identity, and
  completion contracts approved by WP-08A.
- Give one runtime ownership of device, pipelines, staging, VRAM residency,
  uploads, display targets, frame generations, and timing queries.
- Render honest partial/current fixture frames without full residency.
- Apply per-frame upload and render budgets.
- Make timestamps/readback asynchronous and validation/export-only.
- Keep the implementation unreachable from the product until WP-09B.

Exit proof:

- Fixture bricks appear incrementally with typed coverage/freshness.
- GPU lifetime, eviction, replacement, partial coverage, and capacity tests
  pass.
- No UI, project, current data-engine, or product fallback type enters the
  render contract.

### WP-09B — Product Render Hard Cutover

Goal: make WP-09A plus WP-08B/WP-10C the only product rendering route for every
dataset size.

Required work:

- Connect product render intent to the unified resource/runtime contracts.
- Migrate small datasets to the same progressive bricked path as large data.
- Retain dense CPU rendering only as explicit oracle/export/test tooling.
- Remove complete-residency gating and present truthful partial/current frames.
- Remove every hidden product fallback. An unsupported or insufficient adapter
  fails through the resolved D-006 pre-viewer diagnostic; CPU rendering remains
  reference/export/test/diagnostic tooling, never a degraded product route.
- Keep bounded GPU submission inside the render runtime/render callback, never
  in an egui interaction handler merely because it is frame-budgeted.
- Delete the lease-to-current-renderer bridge and the entire predecessor
  product renderer in this same cutover.

Exit proof and deletion gate:

- Focused contract tests cover intent/lease delivery, a useful current partial
  frame followed by a settled frame, stale-frame rejection, and a typed
  capacity or unsupported-GPU diagnostic with no CPU fallback.
- The existing small target-package product scenario exercises MIP, DVR, ISO,
  and a linked cross-section at 1280x720, then confirms a current nonblank frame
  after resizing to 1920x1080.
- Dense product startup/rendering, full-residency guards, old display
  orchestration/identity, and implicit CPU/alternate-GPU fallbacks are deleted.
- The current `mirante4d-renderer` crate/path is deleted without a re-export
  facade after `render-api`, `render-wgpu`, and dev-only `render-reference`
  assume their approved responsibilities.
- WP-09A's GPU correctness, dtype, validity, budget, eviction, and lifetime
  evidence is inherited. WP-09B makes no performance claim and does not repeat
  that matrix or add large-data simulation, 4K, or new evidence machinery.

### WP-09C — UI And Composition-Root Cutover

Goal: finish the command/snapshot boundary after the replacement runtime,
render, persistence, import, and analysis services exist.

Required work:

- Make egui code enqueue typed commands and present `ApplicationSnapshot`s plus
  framework-neutral presentation tokens only; the composition bridge resolves
  tokens without exposing backend resources.
- Move dataset/project open/save, import, analysis, worker shutdown, render
  planning, and resource mutation out of UI execution.
- Extract `mirante4d-ui-egui` and make `mirante4d-app` composition/process-
  lifecycle only.
- Deliver typed operation errors/progress through the application boundary.

Exit proof and deletion gate:

- Focused contract tests prove that UI output is limited to typed commands and
  paint requests, while backend storage/runtime/WGPU resources remain outside
  the UI crate.
- Focused integration tests cover import cancellation/progress, presentation
  token resolution, and orderly shutdown through the application boundary.
- The spread `MiranteWorkbenchApp` orchestration, temporary UI/import/render
  owners, and application-to-current-egui-shell bridge are deleted.
- One bounded native small-fixture scenario covers open/save/switch,
  import/analysis, and useful interaction during background work at 1280x720,
  with a short 1920x1080 presentation check.
- WP-10B durability, WP-11 import, WP-12 science, WP-08B runtime, and WP-09A/B
  GPU evidence is inherited. This package does not repeat their exhaustive
  matrices, add performance claims, introduce 4K or huge-data simulations, or
  create new evidence machinery.

### WP-10A — Dataset Schema, Storage, Index, And Identity

Goal: implement a scalable dataset persisted contract suitable for public data
without yet adding a second reader to the product.

Required work:

- Implement the approved strict M4D-over-released-OME-NGFF profile rather than
  reopening generic-format support.
- Separate logical scientific schema, physical storage profile, large indexes,
  acceleration data, provenance, and display defaults.
- Replace monolithic large JSON indexes with compact lazy structures.
- Enforce OD-018 with mandatory indexed sharding for production pixel,
  validity, and large-index arrays; import preflight and acceptance record
  logical-brick, shard, physical-object, and maximum-directory-fan-out counts.
- Before any production/profile candidate is written or WP-10C begins, freeze
  numeric per-profile package-wide ceilings for pixel/validity/index shards,
  metadata/bootstrap/manifest/provenance objects, directories, depth/fan-out,
  logical and encoded shard size, and one-brick read/decode amplification. No
  per-logical-brick file, sidecar, or manifest record is permitted; bounded
  per-shard descriptors remain required.
- Implement the final owner-approved D-009 scientific/object-package/recipe-
  derivation/release/artifact identities and the approved experimental-to-
  stable lifecycle.
- Define any converter as a separate explicitly approved tool, never a legacy
  branch in the core app.
- Remove absolute local source paths from portable identity/public metadata.
- Make validation bounded, indexed, and proportional to selected mode.
- Provide a deterministic new-format fixture/package producer before product
  cutover.
- Freeze pinned normative schema artifacts by digest and promote the D-023
  target-profile T1 archive, expected facts, identity vectors, and tolerances
  through their separate reviewed authority manifests. Use pairwise-independent
  fixture producer, scientific fact oracle, and reader implementation lineages
  plus hand-built critical vectors; the new Mirante writer cannot create or
  bless its own conformance authority.
- Promote the complete D-009 canonical byte/SHA-256 and metamorphic vector set,
  including equal-science recompression/resharding/channel-order/validity pairs
  and one-bit value/validity/transform identity changes, before candidate
  identity claims.

Exit proof:

- Representative large metadata meets approved open-time/RSS bounds.
- No validity/index validation path is accidentally quadratic.
- Harmless provenance/display changes do not alter content identity.
- New packages are producible and contract-tested without changing the product
  reader yet.
- Target-format independent conformance covers the supported schema/storage/
  identity matrix, and the distinct external reader actually reads the chosen
  Zarr-v3 sharding/codec subset, before WP-10C may start. If it cannot, the
  interoperability claim is narrowed before candidate freeze.

### WP-10B — Transactional Project Store

Goal: persist only WP-07B durable state through atomic project generations.

Required work:

- Implement the final owner-approved D-010 directory-backed project store and
  hard-cut project identity.
- Freeze independently produced canonical envelope/ref/generation/object bytes
  and digests, valid/corrupt project graphs, recovery candidate sets, rebinding
  facts, and a dependency-isolated read-only validator before project-store
  candidate acceptance.
- Bind datasets through verified D-009 scientific identity, with package,
  release, and locator facts kept in their approved roles.
- Complete the current-source identity operation/cache rules above and keep
  project attach/restore/save blocked until the opened current package has a
  verified D-009 identity; a slug or manifest fingerprint never satisfies this
  prerequisite.
- Write and sync immutable/content-addressed objects and a complete generation
  before atomically replacing the tiny authoritative head.
- Use a background store actor, OS writer lease, expected-parent conflict
  check, and file plus directory durability protocol.
- Use exact project-revision dirty tracking rather than deep state snapshots.
- Use the same generation model for explicit revision-aware autosave/recovery
  and conservative root-based compaction.
- Enforce that project object/generation growth follows retained revisions and
  saved artifacts plus total encoded artifact bytes divided by a bounded page/
  object size, never one object per semantic voxel, logical brick, table row,
  or timepoint; measure encoded bytes, page bounds, directory fan-out, autosave
  coalescing, retention/GC, and interrupted GC under stress.
- Keep renderer/runtime types and arbitrary internal paths out of project DTOs.
- Switch the sole project save/open path and delete the WP-07B-to-current-
  persistence bridge in the same WP-10B product cutover; WP-10C does not recut
  project persistence.
- Treat public-main merge as the rollback no-return point for pre-WP-10B
  executables: preserve/verify project backups and a deployable new-store-
  capable revision, then use fix-forward rather than stranding external work.

Exit proof and deletion gate:

- Failure injection at every save stage leaves the prior generation readable.
- Process-kill, corruption, concurrency, autosave, GC, relocation, Save As,
  resource, and real-filesystem durability evidence meets the approved D-010
  matrix.
- New project save/open passes with no legacy migration reader.
- Old artifact replacement, segmentation fields, renderer DTO coupling, and
  deep-snapshot dirty paths are deleted.

### WP-10C — Storage/Runtime Product Hard Cutover

Goal: connect WP-10A storage to WP-08B and make it the sole product dataset
source only after every persisted-data consumer has a replacement path.

Required work:

- Confirm WP-10B project references, WP-11 import output, and WP-12 analysis
  access use the approved identity, storage, and resource contracts before
  removing any old source.
- Mechanically prove that no still-live normal/dev/build/test/benchmark/tool
  target depends on `mirante4d-data` or `mirante4d-format`; in particular the
  predecessor renderer and its tests must already use semantic dataset leases
  and target fixtures.
- Open, validate, schedule, render, and inspect representative new-format
  packages through the real product.
- Switch the sole product dataset source and importer entry point to the target
  storage/identity contracts in one product-facing cutover. WP-10B already owns
  project save/open, and WP-12 already owns analysis execution; WP-10C verifies
  their new dataset bindings rather than recutting those subsystems.
- Cut dataset fixtures and format/storage tooling to the approved profile and
  identity schemes.
- Reject unsupported identities explicitly without an old reader or fallback.

Exit proof and deletion gate:

- Focused source-adapter tests use the existing bounded target archives and
  cover their useful dtype, validity, multiscale, and cross-brick behavior.
- One integration covers target import, product open, background verification,
  analysis, project save, and reopen. One focused corruption or source-change
  case proves fail-closed behavior.
- A short real-product exercise covers 1280x720 and 1920x1080. No huge-data,
  KVM, power-cut, broad performance, or 4K matrix applies.
- Old manifest/index/validator/writer/reader adapters, storage-source bridges,
  current importer, and product branches are deleted only after the replacement
  importer, project store, and analysis consumers pass their predecessor gates.
- `mirante4d-format`, the residual current-source responsibilities in
  `mirante4d-data`, and `mirante4d-import` are deleted rather than retained as
  re-export facades; their approved target owners are the only remaining path.
- Metadata, memory accounting, cancellation, and bounded validation are checked
  where the new product adapter joins the already accepted storage/runtime
  contracts. WP-10A conformance and WP-10B/WP-11/WP-12 evidence are inherited.
- Every WP-06 bootstrap/current-format fixture and its apparent authority is
  deleted at its declared expiry; only the WP-10A target-profile registry may
  remain as candidate-format evidence.

### WP-11 — Import Pipeline Rebuild

Goal: build one bounded, resumable, reproducible off-product importer for the
accepted target format. Product activation remains WP-10C work; public-data
release remains a later open-data workflow.

Required work:

- Implement a clear bounded flow from source inspection through streamed
  decoding, transformation, statistics and scientific hashing, multiscale
  production, validation, and commit. Combine passes where safe.
- Bound memory, queues, and I/O in bytes. Never materialize a complete large
  timepoint or all scale levels.
- Persist only the minimal bounded checkpoint state needed to validate and
  resume completed work units; otherwise clean the owned stage and restart.
- Calculate checksums and statistics while data is already flowing and avoid a
  duplicate full validation pass.
- Record canonical source, recipe, and derivation facts sufficient to reproduce
  the import. Rights, citation, release, DOI, and publication records remain
  deferred.
- Never modify source input. Write into an owned sibling stage, validate it,
  and atomically publish only to a previously absent destination. Collision,
  cancellation, or precommit failure leaves source and existing destinations
  unchanged.
- Reuse the accepted source-TIFF fixture and expected facts. Read importer
  output with the existing independent target reader and compare scientific
  values, validity, axes, calibration, and transforms. Verify actual shard,
  object, and fan-out bounds on the small output plus one focused one-brick
  amplification case.
- Complete the replacement package producer off-product. The current importer
  remains the sole reachable product importer until WP-10C atomically activates
  the replacement and deletes the predecessor; no temporary old-format output,
  alternate UI entry, or selectable dual producer is permitted.

Exit proof:

- Focused tests cover source grouping and rejection, bounded buffers and
  queues, deterministic two-run output, cancellation cleanup, one interrupted
  restart, free-space refusal, corrupt checkpoint/output rejection,
  destination collision, and atomic publication.
- One clean end-to-end run imports the accepted small source corpus, proves
  source bytes unchanged, and passes independent target readback. A practical
  local real-data import may be diagnostic but is not an exit requirement.
- The replacement contains no full-timepoint or all-scale materialization and
  no duplicate product route. Deletion of the still-reachable current importer
  belongs exclusively to WP-10C.

### WP-12 — Analysis Runtime Rebuild

Goal: keep analysis typed and reproducible while moving execution and data
access onto the shared runtime foundation.

Required work:

- Separate pure analysis definitions from execution, storage, UI, and scene
  presentation.
- Route all dataset reads through scheduler-controlled priority and cache
  admission.
- Retain only exact full-intensity summaries/time traces and exact axis-aligned
  box-ROI intensity statistics. Reject approximate and preview execution.
- Freeze deterministic traversal and accumulation, validity-mask behavior, and
  population-variance semantics for `uint8`, `uint16`, and finite `float32`
  against small checked-in hand-computed facts. Do not add an oracle manifest
  or separate evidence workflow.
- Implement bounded progress, cancellation, provenance, and derived-artifact
  commit contracts through WP-10B's transactional project store; analysis must
  not create a second artifact-persistence authority.
- Preserve existing ROI, track, measurement, table, plot, and annotation value
  types where needed, but add no new tracking or segmentation algorithms.
- Do not reintroduce label/segmentation models through a generic artifact path.
- Switch the sole reachable product analysis route to `analysis-runtime` and
  delete the app-local predecessor in the WP-12 cutover; WP-10C later changes
  only the dataset-source binding beneath this contract.

Exit proof:

- Focused scheduling tests prove analysis uses analysis priority, remains
  bounded, and cannot prevent a current-view request from completing first.
- Cancelled, failed, or stale work cannot produce an artifact that appears
  complete, and all accounted resources are released.
- Small hand-computed scientific cases and provenance round trips pass for the
  two retained operations without sharing production math as the expected
  result.
- One table/plot bundle becomes visible in a single project generation and
  survives reopen; focused failure injection before publication exposes
  neither artifact. Accepted WP-10B durability evidence is inherited rather
  than rerun.
- One configured-memory pressure case proves bounded streaming without a huge
  dataset simulation or a performance claim.
- App-local analysis job channels, direct `DatasetHandle` scans, stringified
  operation plumbing, and the temporary WP-08B analysis adapter are deleted.
- The current `mirante4d-analysis` crate/path is deleted without a compatibility
  facade after `analysis-core` and `analysis-runtime` own the retained scope.
- One small product exercise covers cancel, complete, save, and reopen at
  1280×720, with a short 1920×1080 check. No 4K, private-data, performance,
  or broad operation matrix applies to WP-12.

### WP-14 — Verification, Release, And Contributor Hardening

Goal: make the rebuilt foundation continuously trustworthy and maintainable by
people other than its original author.

Required work:

- Keep the existing six public verification leaves and two required GitHub
  Actions jobs unchanged and at `$0`; do not add a release, deep, private-data,
  performance, fuzz, mutation, or coverage workflow.
- Make the existing Linux release-directory, tarball, and AppImage command
  produce self-consistent artifacts bound to a clean full commit and tree, with
  checksums and the notices already shipped by the project.
- Reuse the existing small promoted fixture and render-mode scenario against
  the packaged executable. Exercise 1280x720 and briefly 1920x1080 on the real
  display; do not add 4K, private microscopy data, or a broad product matrix.
- Rehearse the documented contributor setup and ordinary PR checks from one
  clean clone. Correct only concrete setup or release-documentation gaps found
  by that rehearsal.
- Delete the final temporary validation runtime wrapper and other clearly
  obsolete verification helpers. Retain useful focused tests and opt-in
  subsystem checks for future changes to their own boundaries.
- Inherit the accepted storage, durability, import, analysis, GPU, and science
  results. Do not rerun exhaustive matrices whose implementation did not
  change.
- Keep release support honest: WP-14 qualifies a local Linux pre-alpha release
  candidate, not a supported public release or public-data publication.

Exit proof:

- The ordinary public checks pass on the candidate without new paid or
  self-hosted CI.
- One clean clone can build and run the documented PR checks.
- The local Linux package command produces and smoke-checks its three artifact
  forms, and its report identifies the exact clean source and checksums.
- The packaged viewer opens the small fixture, exercises MIP/DVR/ISO and linked
  panels at 1280x720, and remains usable during the short 1920x1080 check.
- No temporary validation owner or obviously obsolete WP-14 verification
  machinery remains.

### WP-15 — Final Deletion Audit And Technical Foundation Milestone

Goal: remove demonstrated foundation leftovers and leave one small, current
product and contributor surface.

Required work:

- Delete accepted-work-package replay validators, stale commands, aliases, and
  adapters that have no current product or contributor purpose. Retain direct
  checks of the current crate graph, dependency direction, filesystem safety,
  renderer boundary, and sharded-storage rules.
- Reduce product validation to the four retained small public scenarios:
  camera/render modes, source verification, and project persistence. Remove
  private T5, free-form script, performance, RSS, and speculative timing
  machinery without changing those retained scenarios.
- Remove demonstrated dead parameters and transitional namespaces, and expose
  only the accepted UI composition entry outside the UI crate.
- Make current-state, contributor, testing, format, release, backlog, and
  deferred-feature docs describe the resulting repository. Delete completed
  active-plan prose; Git history is its archive. Keep schemas used by live
  fixture validators.
- Do not add a replacement evidence system or rerun accepted storage,
  durability, import, GPU, science, KVM, or power-cut matrices.

Exit proof:

- A focused source/docs audit finds no live predecessor route, duplicate
  authority, expired adapter, or stale foundation command.
- Focused tests, the ordinary `$0` PR gate, and the retained small-fixture
  render-mode scenario pass at 1280x720 with its short 1920x1080 check.
- Source microscopy remains byte-identical, target storage remains sharded,
  and the current project/format lifecycle contracts remain unchanged.
- Current docs state the remaining pre-alpha limitations and defer public-data
  publication and segmentation without creating a new gate.
