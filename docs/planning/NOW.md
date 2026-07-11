# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-08A is the active preparatory checkpoint. It freezes the dataset/source,
runtime request/lease, progressive render/presentation, dependency,
side-effect, and allocation-owner contracts needed by WP-08B, WP-09A, and
WP-10A.

The current product reader, scheduler, renderer, and presentation bridge stay
unchanged and remain the sole live route. WP-08A does not implement the new
runtime, storage backend, or GPU renderer.

## Remaining WP-08A Exit Work

1. Pass the focused contracts, exact dependency/API/side-effect/ledger audit,
   and synchronized public verification.
2. Merge under `PR / policy` and `PR / rust`, then require matching exact-main,
   trusted Vulkan, and real-display T2 no-regression evidence.
3. Create `foundation-wp-08a-exit-1` only from that clean protected-main
   revision.

The next package after that exit is WP-08B, the unified dataset runtime.
Package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
