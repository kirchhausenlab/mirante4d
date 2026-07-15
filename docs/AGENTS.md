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
