# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-06A merged at `c7cc4636a6fd8555fb58100311f1db35e40db28b` and its
twenty-attempt cache-free Main calibration passed. The accepted revision and
measurements are recorded in [current state](../CURRENT_STATE.md).

Repository rules now require exactly `PR / policy` and `PR / rust`. This
WP-06C checkpoint deletes the transitional Bootstrap workflow, command,
profile, and audit rules. The six nonrecursive leaves and exact public/trusted-
local ownership remain authoritative.

Target-format T1 is still false, and package capability remains pending.

## Remaining WP-06 Checkpoints

1. Merge this cleanup under `PR / policy` and `PR / rust`.
2. Product-validate the exact protected-main merge revision on the real Vulkan
   workstation at the required 1280x720 and 1920x1080 scenarios.
3. Create the annotated `foundation-wp-06-exit-1` tag.

The complete package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md). The
[backlog](../BACKLOG.md) contains only unresolved work outside this checkpoint.
