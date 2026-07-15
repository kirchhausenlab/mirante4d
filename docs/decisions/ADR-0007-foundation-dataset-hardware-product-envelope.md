# ADR-0007 — Use A Profile-Based Linux/GPU Foundation Envelope

Status: ACCEPTED AND IMPLEMENTED
Accepted: 2026-07-09
Last reviewed: 2026-07-14
Decision IDs: D-004, D-005, D-006, D-015, D-016

The foundation packages implemented this bounded product envelope. Current
facts and support claims live in [Product](../PRODUCT.md),
[Current State](../CURRENT_STATE.md), and [Release](../RELEASE.md).

## Context

The product needs a truthful, measurable workload and hardware boundary.
Combining every observed maximum would imply an untested dataset, storing a
nominal 1 TiB test would be wasteful, broad platform and 4K claims would exceed
available evidence, and a silent CPU product fallback would hide unsupported
interactive hardware. The former segmentation prototype also sat above
foundations that are now being replaced.

## Options Considered

1. Treat all dataset maxima as one Cartesian supported envelope.
2. Generate or store a physical 1 TiB stress dataset.
3. Claim broad Linux, Windows, and macOS support and include 4K qualification.
4. Preserve a silent CPU interactive fallback when no qualifying GPU exists.
5. Retain or repair segmentation during the foundation refactor.
6. Use separately qualified practical workload profiles, one measured
   Linux/Vulkan GPU reference, and remove segmentation first.

## Decision

- Support is the union of named practical profiles, never a Cartesian product
  of their maxima. Qualification does not require a stored or simulated TiB
  dataset.
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
- Linux x86_64/Vulkan is the sole current package boundary. Windows and macOS
  remain portability work until separately approved and validated.
- Qualify product viewports at 1280x720 and through a 1920x1080 exercise. Do not
  add 4K/3840-wide benchmarks, optimization, or claims.
- WP-02 deleted the segmentation prototype as a vertical hard cut. Keep
  segmentation absent; any return requires a separately approved capability
  decision.
- Use the approved CPU and GPU byte-ledger formulas as measured seed policy.
  Calibration may tune implementation details but may not silently broaden the
  support boundary or create another resource authority.

## Consequences

- The release claim is deliberately narrower and evidence-backed. A broader
  platform, GPU class, viewport, or profile requires explicit new acceptance.
- No TiB-scale dataset or 4K display is required for qualification.
- Machines without a qualifying GPU fail clearly before interactive viewing;
  correctness is not weakened through an undisclosed degraded route.
- Extreme spatial and temporal workloads remain independently qualified, and
  private evidence cannot be generalized to combinations that were never run.
- Segmentation remains deliberately absent rather than preserved on top of
  superseded model, storage, runtime, and verification foundations.

## Enforcement

- WP-02 deleted segmentation source, commands, persistence, UI, tests,
  benchmarks, reports, and documentation together and passed its product-open
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

- [Product](../PRODUCT.md)
- [Current State](../CURRENT_STATE.md)
- [Release](../RELEASE.md)
- [Deferred segmentation capability](../plans/deferred/SEGMENTATION.md)
