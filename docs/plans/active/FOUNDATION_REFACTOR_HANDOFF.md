# Mirante4D Foundation Refactor Handoff

Status: ACTIVE PUBLIC-EPOCH PROGRAM
Last reviewed: 2026-07-11
Current technical sequence: WP-06 through WP-15
Public-data publication: deferred to a separate future handoff
WP-06 predecessor: `foundation-wp-05-exit-1`
Completed through: WP-05 documentation and governance reset
Current checkpoint: WP-06A shadow machinery implemented in this revision;
WP-06 not yet accepted
Next checkpoint: integrate and accept WP-06A under Bootstrap, then calibrate

## Purpose

Mirante4D is an early-stage academic desktop viewer for large 4D microscopy
data. The current prototype proved useful product ideas, but its ownership,
state, storage, rendering, testing, documentation, and release foundations need
hard replacement before feature development resumes.

This program replaces those foundations from the top down. It does not preserve
an obsolete design because code or files already exist. Each cutover installs
one authority, proves it, deletes its predecessor, and leaves the protected
branch usable for the next package.

## Durable Product Direction

- Native Linux x86_64 desktop application with Vulkan through `wgpu`.
- Interactive viewing requires a qualifying GPU; CPU rendering remains a
  reference/test/diagnostic/export tool, not a silent product fallback.
- Large datasets remain bounded in RAM, VRAM, queues, open objects, and physical
  filesystem objects.
- The application format is a strict profile layered on released OME-NGFF 0.5
  and Zarr v3, with sharded storage. It must never create one physical file per
  brick or a comparable sidecar explosion.
- Dataset scientific identity, package identity, recipes, derivations, and
  artifacts use versioned SHA-256 contracts. Scientific content identity is
  independent of storage layout.
- Project state uses immutable content-addressed objects, complete generation
  snapshots, atomic head/recovery refs, revision-aware autosave/recovery,
  leases, and conservative garbage collection.
- Rendering is progressive and current-generation-only. It must show useful
  current partial results without a hidden dense or legacy product path.
- Source contribution remains MIT-licensed, maintainer-led, and lightweight.
  Full dataset publication and external dataset contribution are separate
  rights/hosting decisions.

Persisted formats remain experimental until their owning cutovers and release
gates accept them. There is no backward-compatibility requirement during this
greenfield phase.

## Non-Negotiable Invariants

1. One authority per model, state field, resource, operation, and persisted
   identity. No dual live model or shadow runtime survives a cutover.
2. No compatibility reader, migration shim, fallback renderer, alternate data
   path, or commented-out predecessor unless the owner explicitly requests it.
3. All large work is bounded and cancellable. Cancellation and stale-result
   suppression are observable behavior, not best-effort comments.
4. Source input is never mutated by import, validation, failure recovery, or
   project maintenance.
5. Dataset storage is sharded and proves object count, fan-out, amplification,
   and filesystem behavior. Trillions of tiny files are forbidden.
6. Scientific claims require independent expected facts or an independent
   reader. Writer/reader self-agreement alone is not conformance evidence.
7. GPU/resource accounting uses explicit CPU and GPU byte ledgers. Capacity
   failure is typed and visible; it cannot trigger a hidden dense/fallback path.
8. Product errors are typed, actionable, and safe to expose. Logs and evidence
   redact private paths, dataset identities, and credentials.
9. Segmentation remains absent throughout the foundation program. Any return
   requires a separately approved post-foundation capability plan.
10. Foundation display qualification covers 1280x720 and 1920x1080. 4K and
    spanning-display work are out of scope.
11. Public hosted verification costs `$0`: standard public runners only,
    bounded timeouts, no paid/larger runners, no public self-hosted workstation,
    and no cache/artifact storage by default.
12. Rendering, loading, GPU, interaction, and large-data packages require the
    real viewer to be opened and exercised on the relevant data. Automated
    tests and internal smoke scripts are supporting evidence, not substitutes.

## Work-Package Sequence

The order is dependency authority, not a menu:

1. **WP-05 — Documentation and governance reset.** Install one concise
   authority tree and remove contradictory current/historical navigation.
2. **WP-06 — Verification bootstrap.** Replace recursive, duplicated, slow
   gates with requirement-owned leaves, independent fixtures, bounded public
   checks, and trusted local GPU/product/performance evidence.
3. **WP-07A — Canonical model contract.** Define the authoritative domain
   models, typed errors, identity types, and dependency boundary.
4. **WP-07B — Application API and durable-state cutover.** Move the product to
   the canonical models and delete predecessor application/durable state.
5. **WP-08A — Subsystem contract and ownership freeze.** Freeze the ownership
   graph, resource contracts, metrics, and permitted migration bridges.
6. **WP-08B — Unified dataset resource runtime.** Implement bounded metadata,
   schedulers, leases, caches, currentness, cancellation, and byte ledgers.
7. **WP-09A — Progressive render runtime on fixture leases.** Build the new
   renderer off-product against the accepted resource/runtime interfaces.
8. **WP-10A — Dataset schema, storage, index, and identity.** Implement the
   strict OME-NGFF/Zarr profile, sharding, indexes, provenance, identities,
   independent corpus, and negative cases.
9. **WP-10B — Transactional project store.** Implement immutable generations,
   atomic refs, autosave/recovery, leases, failpoints, and durability tests.
10. **WP-11 — Import pipeline rebuild.** Implement bounded, reproducible,
    cancellable import with independent readback and atomic publication.
11. **WP-12 — Analysis runtime rebuild.** Implement typed, out-of-core,
    scientifically checked analysis and atomic artifacts without segmentation.
12. **WP-10C — Storage/runtime product hard cutover.** Switch the application
    to the new dataset/project/import authorities and delete the old paths.
13. **WP-09B — Product render hard cutover.** Switch the product to the new
    renderer and delete every dense/legacy render route.
14. **WP-09C — UI and composition-root cutover.** Make the UI depend only on
    the accepted application/subsystem APIs and delete the old composition
    root.
15. **WP-14 — Verification, release, and contributor hardening.** Qualify one
    evidence set across tests, GPU, packaged E2E, performance, scientific
    checks, clean-clone contribution, release, and zero-cost settings.
16. **WP-15 — Final deletion audit and technical foundation milestone.** Prove
    that no predecessor, duplicate authority, compatibility path, stale active
    document, or unowned requirement remains.

After WP-08A, WP-08B, WP-09A, and WP-10A may be developed against the frozen
interfaces, but protected-branch integration remains deterministic: WP-08B,
then WP-10A, then WP-09A. WP-10B and WP-11 preparation may overlap only where
their accepted interfaces permit it. Product cutovers remain serial.

The package definitions are in
[TECHNICAL_CUTOVER_WORK_PACKAGES.md](foundation-refactor/TECHNICAL_CUTOVER_WORK_PACKAGES.md).
WP-05 and WP-06 are summarized in
[FOUNDATION_ENTRY_WORK_PACKAGES.md](foundation-refactor/FOUNDATION_ENTRY_WORK_PACKAGES.md).

## Owning Technical Briefs

- [Workspace architecture](foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md)
- [Dataset and hardware envelope](foundation-refactor/DATASET_HARDWARE_ENVELOPE_BRIEF.md)
- [Data format and identity](foundation-refactor/DATA_FORMAT_IDENTITY_BRIEF.md)
- [Project-store durability](foundation-refactor/PROJECT_STORE_DURABILITY_BRIEF.md)
- [Verification evidence](foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md)

These documents define target contracts. `docs/CURRENT_STATE.md` alone states
what is implemented now. A package updates current-state documentation in the
same checkpoint that changes the behavior.

## Verification Topology

The WP-06A checkpoint exposes six nonrecursive leaves: policy, lint, unit,
contract, UI, and doctest. Candidate `PR / policy` and `PR / rust` checks and
matching Main results are shadow and non-required; `Bootstrap / required`
remains the sole required context until the fixed calibration window is
accepted and the branch-rule replacement is read back. Only the
unit/contract/UI test-binary build may be shared. The uncached public target is
p95 below ten minutes with a fifteen-minute ceiling over the fixed calibration
window.

The checkpoint discovers 879 live tests: 839 normal tests owned by the public
CPU leaves and 40 ignored tests owned by the trusted GPU lane. Package
capability remains pending. WP-06A makes no target-format T1 claim.

Trusted local lanes own GPU, package, E0-E4, performance, stress, private T5,
and scientific evidence. E4 means the packaged application, OS-level input,
external window/pixel/state observation, a real mapped display, and a relevant
dataset. Internal automation is useful support but cannot close E4.

Evidence is bound to one immutable commit/tree, executable or package digest,
fixture/dataset identity, toolchain, hardware/display facts, commands, and
results. Zero retry is the default. A failure remains part of the evidence set;
quarantine is remediation-only, time-bounded, and never a silent pass.

WP-06 remains incomplete until protected integration and acceptance, twenty
cache-free calibration attempts, the required-context flip, separate bootstrap
cleanup, product-open validation, and the create-once exit tag are accepted.

## Dataset And Fixture Boundary

The public foundation uses three evidence tiers:

- T1: small, immutable, pairwise-independent conformance vectors;
- T2: generated support fixtures that are not conformance authority; and
- T5: private qualification data exposed publicly only by opaque IDs.

Target-format T1 authority begins only after WP-10A freezes the schema. Full
microscopy-dataset selection, rights review, hosting, DOI, uploads, and public
release are excluded from this handoff. They require a separately approved
open-data handoff after the technical foundation exists.

## Package Entry And Exit

Before editing an eligible high-risk package, create a short entry brief bound
to the accepted predecessor. It must name:

- exact outcome and inherited requirements;
- current commit/tree/tag and clean-worktree proof;
- allowed paths, APIs, deletions, and forbidden shortcuts;
- exact tools, commands, fixtures, hardware, thresholds, and evidence root;
- automated and product-open acceptance;
- stop conditions, checkpoint size, rollback unit, and authority flip.

An entry brief may specialize current paths and measurements; it cannot change
this program's scope, architecture, order, or proof class. A discovered gap
that needs such a change reopens this handoff for owner review.

Each package exits only from a clean committed protected-branch revision with
its required evidence and a create-once annotated `foundation-wp-*-exit-1`
attempt tag. Tests or reports existing in the tree do not prove their own
coverage. Every cutover deletes its predecessor in the same accepted package.

## Branch And Rollback Policy

Use one protected `main`, short reviewable branches, squash checkpoints, and
serial authority flips. No force push, history rewrite, moving attempt tag, or
bidirectional cherry-pick is part of the implementation strategy.

Rollback means reverting to an earlier accepted revision, deployment, or
package generation outside the product runtime. It never justifies keeping an
alternate reader, renderer, model, or hidden fallback after cutover.

Unrelated feature work remains frozen through WP-15. Only credible security,
data-loss/corruption, or user-safety work may interrupt through an explicit
handoff update.

## Completion

The technical foundation is complete only when:

- every invariant is implemented and directly enforced;
- every superseded product path and active historical authority is absent;
- one exact evidence set passes the required automated, GPU, packaged E2E,
  performance, stress, scientific, and product-open gates;
- format/project lifecycle claims match the code;
- documentation is concise, current, navigable, and sufficient for a new
  contributor; and
- the owner accepts the remaining explicit limitations.

This milestone does not claim that full microscopy datasets have been
published. Public-data release remains a separately tracked future outcome.
