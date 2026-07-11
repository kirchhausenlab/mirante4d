# Foundation Refactor — Technical Cutover Work-Package Catalog

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-10
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: WP-07A through WP-12 and WP-14/WP-15 package contracts; package definitions occur only here

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

## Package Definitions

Each package inherits its `PRG-*`, `UB-*`, and `INV-*` set from the parent
handoff's Promotion-Time Package Contract Index. An entry-stamped brief expands
those IDs into exact current paths, commands, checkpoints, datasets, thresholds,
and evidence artifacts without changing the contract.

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
- resource keys, immutable payload views, and lease semantics;
- runtime-reserved decode-buffer/sink semantics and the rule that only dataset
  runtime issues/accounts lease lifetimes while storage owns codecs;
- byte-ledger categories and the sole owner of every large CPU/GPU allocation;
- request priorities, cancellation generations, deduplication, and shutdown;
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

- Tiny, medium, and large fixtures take the same product path.
- First-current-partial and settled-frame behavior is automated-verified and
  product-open validated.
- CPU/VRAM/staging budgets and resource lifetime gates pass.
- Dense product startup/rendering, full-residency guards, old display
  orchestration/identity, and implicit CPU/alternate-GPU fallbacks are deleted.
- The current `mirante4d-renderer` crate/path is deleted without a re-export
  facade after `render-api`, `render-wgpu`, and dev-only `render-reference`
  assume their approved responsibilities.

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

- UI cannot directly read storage, mutate residency, join workers, submit
  unbounded GPU work, or execute project/import/analysis transactions.
- The spread `MiranteWorkbenchApp` orchestration and remaining god-state runtime
  fields plus the application-to-current-egui-shell bridge are deleted.
- Real product validation covers responsive open/save/switch/import/analysis
  and continuous interaction.

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

- Representative new-format small and large packages pass automated and
  product-open workflows.
- Old manifest/index/validator/writer/reader adapters, storage-source bridges,
  current importer, and product branches are deleted only after the replacement
  importer, project store, and analysis consumers pass their predecessor gates.
- `mirante4d-format`, the residual current-source responsibilities in
  `mirante4d-data`, and `mirante4d-import` are deleted rather than retained as
  re-export facades; their approved target owners are the only remaining path.
- Metadata open, memory, corruption, and bounded-validation gates pass.
- Target-format independent conformance passes; writer/reader round-trip alone
  cannot close the cutover.
- Every WP-06 bootstrap/current-format fixture and its apparent authority is
  deleted at its declared expiry; only the WP-10A target-profile registry may
  remain as candidate-format evidence.

### WP-11 — Import Pipeline Rebuild

Goal: turn import into a bounded, resumable, reproducible producer for the new
format and public-data workflow before that format becomes the sole product
source.

Required work:

- Define explicit inspect, decode, transform, statistics, hash, multiscale,
  write, verify, and commit stages.
- Fuse redundant source passes where correctness permits.
- Bound memory and I/O in bytes; avoid materializing complete large timepoints
  or all scale levels.
- Journal progress and define cancellation/restart semantics.
- Calculate checksums/statistics during production and avoid duplicate full
  validation passes.
- Produce machine-readable provenance sufficient to reproduce public derived
  packages.
- Preserve source immutability and safe staged/backup commit behavior.
- Promote independently governed source TIFF/OME-TIFF bytes and scientific/
  calibration facts before importer acceptance. Independently read back the
  produced target package and compare those facts; report actual package-wide
  shard/object/directory counts and one-brick storage/decode amplification, not
  only writer-to-reader agreement.
- Complete the replacement package producer off-product. The current importer
  remains the sole reachable product importer until WP-10C atomically activates
  the replacement and deletes the predecessor; no temporary old-format output,
  alternate UI entry, or selectable dual producer is permitted.

Exit proof:

- Peak RSS, throughput, cancellation latency, restart, disk-space, corruption,
  and transaction tests pass for named fixtures/datasets.
- A clean environment can reproduce the approved small public native fixture
  from its declared source and pipeline inputs. Representative private T5
  imports validate the same bounded pipeline with sanitized evidence; the
  follow-on WP-13A later assigns and freezes full release-candidate digests.
- The replacement contains none of the old full-decode review, redundant
  execute/inspection scan, duplicate validation route, or unbounded timepoint/
  scale materialization paths. Deletion of the still-reachable current importer
  belongs exclusively to WP-10C.

### WP-12 — Analysis Runtime Rebuild

Goal: keep analysis typed and reproducible while moving execution and data
access onto the shared runtime foundation.

Required work:

- Separate pure analysis definitions from execution, storage, UI, and scene
  presentation.
- Route all dataset reads through scheduler-controlled priority and cache
  admission.
- Define exact, approximate, and preview semantics per operation.
- For every retained scientific operation, freeze accumulation precision/order,
  mask and validity semantics, variance/percentile definitions, exact versus
  approximate behavior, and tolerance rules against hand-derived rational or
  dependency-independent high-precision facts; shared production math or a
  vague “NumPy-style” comparison is not an oracle.
- Promote those facts, oracle/tolerance manifests, and provenance assertions
  before the corresponding operation becomes an authoritative product route.
- Implement bounded progress, cancellation, provenance, and derived-artifact
  commit contracts through WP-10B's transactional project store; analysis must
  not create a second artifact-persistence authority.
- Preserve ROI, track, measurement, table, plot, annotation, and intensity
  capabilities only where their product contracts are approved.
- Do not reintroduce label/segmentation models through a generic artifact path.
- Switch the sole reachable product analysis route to `analysis-runtime` and
  delete the app-local predecessor in the WP-12 cutover; WP-10C later changes
  only the dataset-source binding beneath this contract.

Exit proof:

- Analysis cannot starve or bypass interactive data policy.
- Cancelled/failed work cannot produce an artifact that appears complete.
- Independently governed scientific goldens, reviewed tolerances, and
  provenance round trips pass; the candidate implementation cannot update and
  bless its oracle in the same evidence run.
- Failure injection proves derived artifacts become visible atomically through
  the project store or not at all.
- Large-data analysis stays within approved resources.
- App-local analysis job channels, direct `DatasetHandle` scans, stringified
  operation plumbing, and the temporary WP-08B analysis adapter are deleted.
- The current `mirante4d-analysis` crate/path is deleted without a compatibility
  facade after `analysis-core` and `analysis-runtime` own the retained scope.

### WP-14 — Verification, Release, And Contributor Hardening

Goal: make the rebuilt foundation continuously trustworthy and maintainable by
people other than its original author.

Required work:

- Complete all technical-foundation verification lanes and explicit evidence-set
  flow. Full-dataset reproducibility/candidate lanes are excluded by resolved
  D-021 and run later under WP-13V; this handoff still proves the data-ready
  contracts and small approved public fixtures. Native CI/process conclusions
  are authoritative; JSON reports add diagnostics and measurements and may
  never turn a failed/skipped job into passed evidence.
- Bind trusted scheduled outputs explicitly into a release evidence-set
  manifest rather than requiring an impossible single invocation or scanning
  storage. Produce a private exact manifest that may bind T5 identities and a
  public sanitized companion that contains exact digests only for approved
  public T1/T2 (and later T3/T4) artifacts, uses opaque T5 IDs, and states only
  `privately qualified; public reproducibility pending` where applicable. Both
  identify the exact commit/tree, build/package digest, eligible dataset
  release/candidate identity, lane run/job/attempt IDs, freshness, artifacts,
  and approved waivers without leaking private paths or digests.
- Establish a named performance metric registry and regenerate baselines on
  fixed declared hardware using D-023's sampling/confidence/freshness rules.
- Add seeded fuzz corpora, crash minimization/promotion, and rotating mutation
  shards. Before activating the code-coverage ratchet during WP-14, freeze its
  tool/version/command, registry-derived crate/target scope, reviewed generated/
  vendor/platform exclusions, clean baseline, per-crate and changed-code
  metrics, allowed noise, and no-regression rule; code coverage remains distinct
  from requirement/assertion coverage.
- Exercise a black-box packaged process through OS-level keyboard/mouse/window
  events, OS-observed mapped-window output, save/terminate/relaunch, and
  resulting durable state. Internal command injection, renderer readback, and
  offscreen capture remain integration evidence, not product E2E.
- Freeze the external E4 harness/assertions before the candidate; require at
  least one authoritative T1-backed workflow, externally observed physical
  client/render pixels, the 720p gate plus 1080p exercise, and observable
  pixel/landmark/durable-state facts rather than mapped/nonblank output alone.
- Keep persistent public-repository self-hosted runners absent. Trusted local
  E4/GPU/performance/T5 execution binds immutable commits and uses a separate
  post-run evidence assembly step that never exposes an upload credential to
  tested code.
- Make packages reproducible where practical and publish checksums, SBOM, build
  provenance, and release notes. Build and test with read-only credentials;
  publish only through a separate sanitized local maintainer operation or a
  credential-only no-checkout job that executes no candidate-controlled code.
- Validate contribution/bootstrap docs with external contributors or a clean
  room rehearsal.
- Define supported-platform, security response, deprecation, and maintenance
  policies.
- Enforce zero orphan ignored/special/GPU tests and a requirement/scenario/
  hardware matrix for the trusted GPU lane.
- Separate short-lived diagnostic artifacts from durable signed release
  evidence, SBOMs, provenance, fixture manifests, and public-data validation.
- Enforce D-022's initial cache-free/artifact-free default, seven-day repository
  retention, release-asset boundary, standard-runner-only policy, and
  organization `$0` stop-usage control. Any cache is a separately ratified,
  measured exception with the approved enforceable `2 GB`/two-day bounds; any
  hosted failure bundle is separately activated only after pre-upload and
  shared-headroom controls prove its `25/50/200 MiB` limits. Local unique-run
  diagnostics remain available without Actions storage.
- Define waiver schema: requirement/scope, reason, alternate proof, approver,
  issue/owner, expiry, and release applicability.
- Delete recursive gate composition, hard-coded test-name allowlists, arbitrary
  `target/` report scanning, completion-waiver/report-presence closure
  machinery, phase-numbered commands, and obsolete baselines after their
  approved replacements are active.

Exit proof:

- Public required checks are fast, non-duplicative, and green on the release
  candidate.
- The frozen code-coverage command reproduces its scoped baseline/ratchet, and
  the separate requirement-coverage audit derives closure only from passed
  registered assertions.
- Trusted GPU, performance, stress, real-data, and product-open gates pass.
- Final E4 evidence uses the frozen external harness and authoritative T1 facts;
  WP-02's transitional manual deletion-regression run cannot substitute for it.
- Performance results use an implementable registry containing measurement
  boundaries, clock/source, submitted-versus-presented meaning, unit/direction,
  scenario/viewport/dataset digest, build profile, warmups, repetitions,
  statistic/confidence rule, noise floor, absolute/relative thresholds,
  environment fingerprint, runner calibration/drift, promotion, and waiver
  policy. Any presentation proxy is named honestly.
- A new contributor can make and verify a scoped change without private tribal
  knowledge.
- Final settings/API and billing/storage readback after representative deep and
  release operations proves the selected-actions/token/fork-approval/retention
  policy still holds; no self-hosted/larger runner can target the repository;
  net Actions billing is zero; and caches/artifacts are absent or exactly within
  the separately approved bounded exception.

### WP-15 — Final Deletion Audit And Technical Foundation Milestone

Goal: audit that every prior hard cutover already deleted its old authority and
close the technical foundation without compatibility debris.

Required work:

- Re-run architecture/dependency ownership audits against the final graph.
- Confirm no old/new dual path remains without an explicit approved reason.
- Audit for dense product runtime, duplicate mirrors/pools, string-only workflow
  errors, obsolete report/audit machinery, phase commands, stale specs, aliases,
  and transitional adapters that prior packages were required to delete.
- If a major old authority remains, reopen its owning work package; WP-15 must
  not become a late cleanup bucket that carries two architectures to the end.
- Remove only incidental stray dead material whose deletion does not hide a
  missed architectural cutover.
- Update current-state, roadmap, release, format, testing, and contributor docs.
- Archive the final handoff and execution evidence after concise active
  contracts absorb the result.

Exit proof:

- Source and docs audits find no superseded product path or active historical
  authority.
- Every approved requirement has its designated passing evidence. Requirements
  classified for automation are automated-verified; human product, legal,
  institutional, data-rights, and owner-acceptance requirements use their
  explicitly assigned evidence/approval class.
- Required real application workflows and technical validation datasets are
  product-validated. Full public-release dataset workflows belong only to the
  separately approved WP-13A/V/B follow-on.
- The owner accepts any explicit remaining limitations for the foundation
  milestone.
