# Development Simplification And Viewer Recovery Plan

Status: ACTIVE — RECOVERY AND PROCESS CUTOVER
Planning authorization: OWNER GRANTED 2026-07-26
Implementation authorization: OWNER GRANTED 2026-07-26
Last reviewed: 2026-07-26

## Outcome

Restore fast, product-directed development without weakening the boundaries
that protect scientific meaning, source data, persisted user data, security,
bounded resource use, or release integrity.

The program has two coupled outcomes:

1. preserve and selectively recover useful viewer-successor work without
   carrying forward its qualification apparatus; and
2. deliver the reported rendering improvements as independently useful,
   immediately product-validated slices.

This plan supersedes the former EP-00 through EP-07 viewer-performance plan.
Git history and the external recovery bundle retain that plan and its
unpublished research history.

## Recovery Record

The damaged linked worktree pointed at a missing commit object after the last
recoverable commit `3d967f0f9805536e63cdf1aaf8c1a36793b21a92`.

Before repository changes:

- the complete shared Git directory was copied and checksum-compared;
- the linked worktree was copied without its reproducible build target and
  checksum-compared;
- its seven-file, 1,711-line surviving working-tree delta was preserved as a
  binary patch;
- `main`, all foundation tags, and the 199 recoverable commits were written to
  a complete external Git bundle;
- the surviving delta was committed on a reconstructed branch as
  `3dfe9de4105fa5355d783656e07fc27b7472ed2a`; and
- an independent clone passed `git fsck` and matched every non-build worktree
  file byte-for-byte.

The external bundle SHA-256 is
`f7abe46b34ee4d9a90d89748bae4bacd683b7f34accae0d6094bc457fd2e9b7e`.
Private paths and unpublished evidence remain outside the repository.

The recovered branch is installed in the live repository. The five zero-byte
loose objects, pre-repair index, and damaged reflog tails are quarantined in
the recovery area. Strict full object/ref verification passes, and the linked
worktree is clean and byte-identical to the independent recovery clone.

## Invariants Retained

- Never mutate source microscopy data.
- Publish datasets and projects atomically and create-only where required.
- Keep valid zero, invalid/no-data, missing/loading, and complete data
  semantically distinct.
- Bound memory, VRAM, queues, I/O, descriptors, filesystem objects, and
  temporary storage; large work remains cancellable and stale-suppressing.
- Keep capacity, capability, corruption, and unsupported-state failures typed
  and visible.
- Keep dataset storage sharded with a bounded object count.
- Use independent expected facts or an independent reader for scientific
  conformance.
- Keep one normal product path after each accepted cutover; do not retain a
  compatibility or fallback product.
- Validate visible rendering, loading, and interaction changes in the normal
  application on a real display and relevant data.

## Process Rules

Development evidence and claim-bearing qualification are separate activities.
Qualification cannot block diagnosis or implementation.

A check is admitted only when it protects a named retained invariant, catches
an actual regression class, or supplies direct product feedback. Every slow
check has an affected-boundary trigger. Tests of source spelling, exact private
field layout, generated evidence bookkeeping, or the absence of work already
enforced by one owner are not product evidence.

Plans describe outcomes, risks, deletions, and useful checks. They do not
freeze implementation details, every counter, or a benchmark protocol before
the system can be observed.

Temporary branch prototypes are allowed. Only one implementation becomes the
normal product at an accepted slice cutover.

## Verification Tiers

### Edit loop

Use direct changed-crate compilation, Clippy, and focused tests. Warm feedback
should normally complete within two minutes. The full repository profile is
not an iteration command.

### Merge checkpoint

Keep the public required identities `PR / policy` and `PR / rust`. The Rust
critical path must fit a ten-minute hard ceiling and target five to eight
minutes without retries. It contains formatting/lint/build checks, cheap
unit/property tests, and a small representative contract/integration set.

Exhaustive process-crash matrices, repeated long import variants, empty
doctests, and unrelated boundary qualification do not run here.

### Changed boundary

Run trusted GPU, format lifecycle, project-store lifecycle, import/product, or
packaging checks only when their owned boundary changes. One representative
normal-product workflow is preferred to several internal counter
reconciliations.

### Release or public claim

Use a clean immutable revision, fixed workload and hardware, independent
scientific facts where applicable, and enough samples for the claim actually
made. Tail or relative claims require deliberately adequate populations; they
are not routine development prerequisites.

## Hash And Provenance Policy

Retain hashes that are part of product identity or integrity:

- scientific content and package identity;
- persisted project/artifact integrity;
- source-preservation facts at trust boundaries;
- independently promoted fixture facts; and
- release artifacts or explicit public performance evidence.

Development diagnostics normally record only revision, dirty state, workload
identifier, relevant settings, hardware, and observations. They do not require
hash-bound schema lattices, receipt reprojection, per-role executable hashing,
or detached clean builds.

Avoid duplicate full-content passes. Prefer one incremental write-time digest,
cheap descriptor/generation currentness checks during work, and one
boundary-appropriate final validation. Deep external verification remains an
explicit boundary, not a repeated internal ritual.

## Work Sequence

### R0 — Preserve And Reconstruct

Status: COMPLETE

- Preserve shared Git state, linked-worktree files, and the surviving delta.
- Reconstruct the recoverable chain on an independent repository.
- Verify history, tree equality, and the complete bundle.
- Install a live recovery ref before quarantining corrupt objects or metadata.

Exit: the live repository can reach both current `main` and recovered
`3dfe9de`; a fresh clone passes `git fsck`; no recovered source depends on the
damaged worktree.

### R1 — Authority And Process Cutover

Status: COMPLETE

- Make this plan and Current Work the sole active authorities.
- Mark EP-00/EP-01 qualification nonblocking and frozen for deletion.
- Update the agent, testing, development, contribution, and verification
  policies to the four verification tiers.
- Add an explicit rule that process must not grow faster than the protected
  product change without owner approval.

Exit: ordinary implementation may proceed with focused checks and immediate
product feedback.

### R2 — Required Verification Right-Sizing

Status: COMPLETE

- Preserve `PR / policy` and `PR / rust` names.
- Remove the empty doctest lane.
- Stop Clippy from compiling test targets before Nextest compiles them.
- Move exhaustive project-store transition/crash matrices and redundant
  long-running import/project-close variants to affected-boundary commands.
- Retain cheap unit/property coverage, representative application integration,
  source nonmutation, scientific oracle, and storage conformance cases.
- Remove exact lane-assignment or architecture checks that merely pin private
  implementation shape; retain dependency-direction and real boundary checks.

Exit: focused warm iteration is normally below two minutes and the required
Rust job fits its ten-minute hard ceiling.

The first post-cutover run discovered 1,357 routine cases and passed all of
them in 81.9 seconds after a 35.4-second library/binary Clippy phase. The six
demoted deep cases remain explicitly runnable and passed independently.

### R3 — Qualification Apparatus Deletion

Status: ACTIVE

- Delete the EP-01 selection authority and its bound schemas.
- Delete development raw-report-to-receipt projection, replay, and admission
  gates.
- Delete per-role clean-worktree, executable, profile, and authority hash
  cross-products.
- Delete parallel xtask implementations of successor checkpoint, import,
  package, reader, runtime, and GPU behavior.
- Retain one small bounded process supervisor, useful asynchronous GPU timing,
  a compact benchmark command, and focused independent numerical oracles.
- Remove product counters and receipt propagation whose only purpose was to
  prove that redundant verification did not run.

Exit: performance tooling is materially smaller than the product code it
exercises and is not on the edit or merge critical path.

### R4 — Recover Product Work

- Classify the recovered branch into product mechanisms, shared safety
  utilities, tests/oracles, and qualification-only scaffolding.
- Treat it as a research spike, not a mergeable whole.
- Retain only mechanisms tied to a current user-visible failure or a retained
  invariant.
- Prefer small product-only ports to importing the qualification substrate.
- Delete losing prototypes and parallel product paths at each slice cutover.

Exit: every retained successor module has a normal-product consumer, a focused
reason to exist, and proportionate checks.

### V1 — Resident Interaction And Per-View LOD

- Keep the current format unless measurement proves it blocks this slice.
- Make wheel and drag input update bounded hot state without static residency,
  dataset, or project reconstruction.
- Select LOD independently for each visible view/layer from projected sampling
  and retain explicit target/fallback facts.
- Coalesce transient input and commit durable view state once per gesture.

Checks: focused camera/LOD tests, trusted-GPU correctness where changed, and
normal real-display wheel, compound-angle, and four-panel exercises.

Exit: no observed wheel freeze, no unjustified settled coarse LOD, and no
per-sample durable revision churn on the representative workflow.

### V2 — Cold Refinement And Complete Presentation

- Measure unique reads, decodes, uploads, target settlement, and visible
  completeness only.
- Reuse overlapping residency and cancel obsolete work at bounded points.
- Keep a complete fallback current until complete target coverage is ready.
- Never present a partial brick mosaic as a complete target frame.

Checks: focused scheduling/residency tests plus real-display motion beyond the
resident envelope.

Exit: overlap is not repeated, the viewer stays interactive during refinement,
and arriving bricks are not visibly assembled into the current image.

### V3 — MIP, DVR, And ISO Kernels

- Profile each mode separately with production-shaped scenes.
- Fix measured descriptor, traversal, sampling, submission, and shader costs.
- Preserve dtype, validity, interpolation, projection, compositing, depth,
  picking, and lighting semantics with focused independent facts.
- Change brick geometry, codec, or persisted format only if measured unique
  byte/decode amplification remains a dominant blocker after runtime fixes.

Checks: focused numerical/oracle cases, trusted Vulkan execution, and
real-display MIP/DVR/ISO exercises.

Exit: each retained optimization has a measured end-to-end benefit and no
visible or scientific regression.

### C0 — Closeout

- Delete obsolete plans, commands, schemas, counters, prototypes, and corrupt
  worktree metadata after recovery remains independently verified.
- Update current-state, architecture, testing, development, and decisions to
  describe only the live system.
- Run the proportionate required and affected-boundary checks.
- Product-validate every changed visible workflow.

Exit: one understandable product architecture remains, ordinary changes have
fast feedback, and no development-evidence system is a prerequisite for
continued product work.

## Stop Conditions

- Stop if a cleanup risks losing unrecovered source or private evidence.
- Stop a new test/provenance mechanism if its protected risk and trigger are
  not explicit.
- Stop a product slice if real-app observation contradicts internal tests.
- Stop a format rewrite unless current measurements make it necessary.
- Stop and revise this plan if simplification itself becomes another broad
  foundation program that delays user-visible progress.

## Completion

This plan is complete only when recovery is durable, proportional verification
is active, qualification-only machinery is removed or isolated, the three
viewer slices are product-validated, and current documentation describes one
live implementation without obsolete process authority.
