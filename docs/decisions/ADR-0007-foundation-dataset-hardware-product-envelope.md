# ADR-0007 — Use A Profile-Based Linux/GPU Foundation Envelope

Status: ACCEPTED TARGET DECISION
Accepted: 2026-07-09
Last reviewed: 2026-07-10
Decision IDs: D-004, D-005, D-006, D-015, D-016
Implementation authorization: NO

This ADR fixes target policy only. Current product behavior and support evidence
remain factual until approved work packages change and validate them. The
current segmentation prototype, current CPU fallback behavior, current formats,
and existing diagnostics are not changed or qualified by this ADR.

## Context

The foundation needs a truthful, measurable workload and hardware boundary.
Combining every observed maximum would imply an untested dataset, storing a
nominal 1 TiB test would be wasteful, broad platform and 4K claims would exceed
available evidence, and a silent CPU product fallback would hide unsupported
interactive hardware. The existing segmentation prototype also sits above
foundations that are about to be replaced.

## Options Considered

1. Treat all dataset maxima as one Cartesian supported envelope.
2. Generate or store a physical 1 TiB stress dataset.
3. Claim broad Linux, Windows, and macOS support and include 4K qualification.
4. Preserve a silent CPU interactive fallback when no qualifying GPU exists.
5. Retain or repair segmentation during the foundation refactor.
6. Use separately qualified workload profiles, one measured Linux/Vulkan GPU
   reference, a lazy structural scale simulator, and remove segmentation first.

## Decision

- Support is the union of named `DS-0` through `DS-4` profiles, never a Cartesian
  product of their maxima. `DS-X` is a deterministic lazy structural simulator;
  it advertises large logical scale without storing the payload or per-key table,
  and its own disk footprint is capped at 64 MiB.
- Distinguish representable, functionally supported, usable, and
  performance-qualified claims. Public wording may use only the strongest level
  proved for the exact profile, machine, viewport, build, and revision.
- Use one externally resolved Linux/Vulkan machine as opaque class `HW-2`, the
  initial reference class. Keep the weaker 16-GiB-RAM/4-GiB-VRAM `HW-1`
  candidate unadvertised until an exact machine passes its required evidence.
- Require a qualifying hardware GPU for the interactive viewer. Unsupported
  adapters receive an explicit pre-viewer diagnostic. CPU rendering remains
  available only for reference, testing, diagnostics, and export, never as a
  silent product fallback.
- Linux x86_64/Vulkan is the sole foundation release claim. Windows and macOS
  remain portability work until separately approved and validated.
- Qualify product viewports at 1280x720 and through a 1920x1080 exercise. Do not
  spend foundation work on 4K/3840-wide benchmarks, optimization, or claims.
- Delete the current segmentation prototype as a vertical hard cut in WP-02.
  Keep segmentation absent for the rest of the foundation program; any return
  requires a separate post-foundation capability decision.
- Use the approved CPU and GPU byte-ledger formulas as measured seed policy.
  Calibration may tune implementation details but may not silently broaden the
  support boundary or create another resource authority.

## Consequences

- The release claim is deliberately narrower and evidence-backed. A broader
  platform, GPU class, viewport, or profile requires explicit new acceptance.
- No 1 TiB allocation, storage purchase, 4K display, or new hardware purchase is
  required for foundation closure.
- Machines without a qualifying GPU fail clearly before interactive viewing;
  correctness is not weakened through an undisclosed degraded route.
- Extreme spatial and temporal workloads remain independently qualified, and
  private evidence cannot be generalized to combinations that were never run.
- Segmentation functionality is temporarily absent rather than preserved on top
  of superseded model, storage, runtime, and verification foundations.

## Enforcement

- WP-02 must delete segmentation source, commands, persistence, UI, tests,
  benchmarks, reports, and documentation together and pass product-open
  regression proof without leaving a hidden route or compatibility shim.
- Dataset, scheduler, renderer, import, package, and performance evidence must
  name the exact `DS-*` profile, support level, hardware manifest, viewport,
  build, revision, resource budgets, and clean command.
- WP-08/WP-09 enforce one byte authority per CPU/GPU resource domain and reject
  silent unbudgeted residency or a second product runtime.
- WP-14 qualifies the Linux/Vulkan package on `HW-2`, including the 720p gate and
  1080p product exercise, and verifies the unsupported-GPU diagnostic separately.
- Adding segmentation, claiming `HW-1`, broadening platform/4K support, changing
  the GPU-required policy, or materially changing a profile/resource formula
  requires reopening the owning decision rather than silently editing a test.

## Owning Documents

- [Foundation Refactor Implementation Handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
- [Dataset And Hardware Envelope Brief](../plans/active/foundation-refactor/DATASET_HARDWARE_ENVELOPE_BRIEF.md)
- [Foundation Entry Work Packages](../plans/active/foundation-refactor/FOUNDATION_ENTRY_WORK_PACKAGES.md)
