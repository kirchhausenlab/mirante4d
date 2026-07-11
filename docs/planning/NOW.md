# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-08A exit 1 is accepted and immutable. WP-08A is temporarily reopened for
one corrective contract checkpoint before WP-08B. The correction adds
effective validity to payloads, isolates cancellation by scope, makes request
IDs runtime-owned, exposes bounded runtime configuration/diagnostics/progress,
and permits the dataset-runtime crate to own its future workers.

The current product reader, scheduler, renderer, and presentation bridge stay
unchanged and remain the sole live route. WP-08A does not implement the new
runtime, storage backend, or GPU renderer.

## Remaining Corrective Exit Work

1. Complete the narrow contract correction and its focused tests.
2. Pass the dependency/API/side-effect audit and synchronized public
   verification.
3. Merge under `PR / policy` and `PR / rust`, then require matching exact-main,
   trusted Vulkan, and real-display T2 no-regression evidence.
4. Create `foundation-wp-08a-exit-2` from that accepted protected-main revision.

The next package after exit 2 is WP-08B, the unified dataset runtime.
Package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
