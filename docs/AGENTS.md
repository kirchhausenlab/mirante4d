# Agent Guide

Mirante4D is a pre-alpha academic open-source desktop viewer for large 4D
microscopy data.

## Before Changing The Repository

Follow the sole read order in the [documentation index](README.md), then read
the domain document that owns the change.

Authority resolves as follows:

- [Product](PRODUCT.md) owns product scope.
- [Current state](CURRENT_STATE.md) owns implemented-versus-planned status.
- [Current work](planning/NOW.md) owns the active checkpoint.
- The relevant domain document owns technical detail.

Plans and ADRs record accepted targets and rationale. Tests and reports are
evidence. None overrides current-state facts by itself. Treat conflicting
active authorities as a documentation defect; do not silently choose one.

## Project Invariants

- Only report to me in ASD-STE100 Simplified Technical English.
- Use hard cutovers. Do not add compatibility readers, migration shims,
  fallback branches, dual-format paths, commented-out predecessors, or other
  legacy machinery unless the user explicitly requests it.
- Keep one live authority for each model, field, resource, operation, and
  persisted identity. Delete the predecessor in the accepted cutover.
- Never mutate source microscopy data. Validate output before publication and
  never expose an incomplete result as complete.
- Bound large work in memory, VRAM, queues, open objects, I/O, and physical
  filesystem objects. It must be cancellable and suppress stale results.
- Dataset storage must be sharded with a bounded physical-object count.
  File-per-brick layouts and comparable sidecar explosions are forbidden.
- Capacity and capability failures must be typed and visible. They must not
  silently select dense, CPU, legacy, or alternate product paths.
- Scientific conformance needs independent expected facts or an independent
  reader. Writer/reader self-agreement is insufficient.
- Segmentation remains absent. Restoring it requires a separately approved
  capability plan.
- Do not commit secrets, private paths, or unpublished dataset metadata.
- Hosted verification must cost `$0`: standard public runners only, no paid
  runners, and no public self-hosted workstation.
- Keep process proportionate to a small academic project. Add it only when it
  protects scientific correctness, user data, security, or release integrity.
  Verification or provenance machinery must not grow faster than the product
  change it protects without explicit owner approval.
- UI Descriptions: Do not add subtitles, helper text, or descriptive copy beneath
  headings, labels, cards, or settings by default. Prefer one concise, self-explanatory
  heading or label. Only add supporting copy when the user explicitly asks for it or
  when it is necessary to prevent misunderstanding or error, and never use it to
  restate the heading.

## Tangible Progress, Anti-Ceremony, and Honest Credit

The purpose of this project is working, deployable software delivered
accretively in the shortest time compatible with correctness, performance,
reliability, and innovation. Process exists to serve that outcome; it must
never become the product.

- **No process porn.** Certificates, ledgers, dashboards, meta-reports,
  and process documents are not progress. A process artifact may exist
  only when it is a hard gate for a named feature or capability — the
  conformance validator and required release evidence qualify;
  self-referential paperwork does not. Choosing process artifacts because
  they are easy and low-risk is reward hacking, and it is treated as such.
- **Feature-first ratio.** The overwhelming majority of open work items
  must deliver runnable behavior — code, schemas, and contracts that an
  end user or consuming agent can actually exercise. Process/ops items are
  capped (guideline: at most ~5% of open beads), and each must name the
  feature work it gates; a process item that gates nothing does not get
  created.
- **Honesty is absolute.** Never fake a test, present a fixture or mock as
  live proof, weaken an assertion to make it pass, hard-code a success
  path, or close work that is not done. A false close is reopened with an
  incident comment on the record.
- **Refusal is not delivery.** A correctly typed refusal is far better
  than a fabricated result — and far less valuable than the real
  capability. Implementing only the refusal path earns partial credit at
  most: it never closes a feature work item. Full credit requires the
  positive capability implemented for real, tested, and verified. Mark
  refusal-only states explicitly (e.g., a `refusal-only` label plus a
  follow-up item) so they read as unfinished, never as shipped.

These rules bind human-directed sessions and agent sessions alike, and they
must be encoded into the acceptance criteria of the work items themselves.

## High-Risk Work

Architecture, domain APIs, ownership/concurrency, persistence, data formats or
identity, import/preprocessing, rendering/viewport/GPU, data loading,
large-data performance, scientific analysis, verification/release
architecture, and broad corrective refactors require an approved plan before
implementation.

Before editing:

1. Write a short, concrete plan naming the outcome, important invariants,
   scope, authority changes, deletions, risks, and useful checks.
2. Obtain user approval before materially changing the requested scope,
   architecture, or evidence class.
3. For a cutover, define how the predecessor is deleted and how the new
   authority will be checked. A cutover is incomplete while a hidden alternate
   path remains.

Qualification can constrain a public or release claim. It must not block
diagnosis, implementation, or focused product observation.

## Verification Language

- **Implemented:** the change exists.
- **Automated-verified:** the relevant automated checks passed for the stated
  revision.
- **Product-validated:** the normal native application was opened on a real
  display and the affected workflow was exercised on the relevant dataset and
  hardware.

Rendering, viewport, GPU, data-loading, interaction, and large-dataset work is
not complete without product validation unless the user explicitly waives it.
Unit tests, smoke tests, virtual/no-display automation, benchmarks, snapshots,
and internal readbacks are supporting evidence, not substitutes.

Performance claims must name the workload, hardware, metric, sampling method,
and threshold. Completion reports should state the meaningful checks and
results, important skips or waivers, and remaining risk.

## Named reward-hacking patterns (all forbidden)

Beyond refusal-farming and process porn, these patterns are called out by
name because this architecture specifically invites them:

1. **Gate self-weakening** — editing validator/conformance code so a
   failing check passes. Conformance code is a separate single-owner lane
   with reviewer sign-off; batch verify diffs it every wave.
2. **Proof-class inflation** — presenting fixtures, retained captures,
   mocked endpoints, or hand-inserted database rows as live proof. Live
   proof requires runtime-selected subjects with recorded selection
   seeds, receipts chained to real route manifests and accounts, and
   fresh-process readback.
3. **Golden regeneration reflex** — regenerating goldens to match broken
   output instead of fixing the output. Golden changes require an
   explicit GOLDEN-CHANGE commit note and a semantic diff review.
4. **Commit-stream pumping** — trivial or artificially split commits, or
   `todo!()`/`unimplemented!()` scaffolds that pass `cargo check`.
   Placeholder macros are banned in committed code (batch verify greps
   for them); every commit names its bead and touched scope.
5. **Tautological tests** — tests that assert the code does whatever the
   code does, or that omit negative cases. Every feature bead
   pre-specifies its key behavioral assertions, including at least one
   negative case a naive wrong implementation would fail.
6. **Easy-bead cherry-picking** — repeatedly claiming low-risk beads
   while articulation-point beads starve. Claim the highest-priority
   ready bead; act on staleness alerts for unclaimed P0/P1 work.
7. **Close-pump abuse** — closing beads (yours or a peer's) to flood the
   ready pool, since closure is what unblocks dependents. Only the
   orchestrator closes; violations are reopened with an incident comment.
8. **Scope-splitting** — splitting one unit of work into
   types/impl/tests mini-closures to harvest multiple credits. Code and
   its tests ship in the same bead; test-only follow-ups exist only for
   cross-cutting integration suites.
9. **Spec-editing as progress** — weakening a plan, spec, or frozen
   decision instead of implementing it. Plan edits are a chore lane,
   never close feature beads, and frozen decisions change only through
   the joint decision protocol.
10. **Conformance metastasis** — adding speculative checks, matrices, or
    reports because they are safe and satisfying. New checks must cite an
    observed defect class or a named release gate.
11. **Dependency smuggling** — vendoring or shimming around the banned
    runtime/database dependencies to "make progress". Batch verify
    enforces the dependency deny-list.
12. **Demo-path hardcoding** — special-casing pilot SKUs, stores, or
    properties so the happy path passes. Conformance subjects are
    runtime-selected and differ from development fixtures.
