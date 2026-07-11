# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-07B-B is the active live-cutover checkpoint. The canonical application,
project model, dataset catalog, render API, and settings owner are live;
`AppState`, `WorkbenchCommand`, project-v14/preferences-v1 authority, and
`mirante4d-core` are deleted.

The local candidate passes the synchronized public and trusted-GPU gates. This
is pre-commit evidence only; exit still requires the same checks and the
product-open scenario on the accepted clean revision.

## Remaining WP-07B Exit Work

1. Commit the atomic checkpoint, merge under `PR / policy` and `PR / rust`, and
   require the matching exact-main checks.
2. Run the trusted Vulkan lane and the machine-stamped real-display T2 scenario
   on that clean protected-main revision.
3. Create `foundation-wp-07b-exit-1` only after both exact-revision results
   pass.

The next package after that exit is WP-08A, the dataset/runtime contract.
Package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
