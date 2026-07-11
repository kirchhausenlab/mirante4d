# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-08A exit 2 is accepted and immutable. WP-08B is the active package.

The unified production runtime, current-source bridge, renderer lease bridge,
and atomic app cutover are implemented. All interactive consumers use that
runtime; predecessor read pools, payload mirrors, and unbounded interactive
result channels are deleted. Analysis execution remains unavailable until
WP-12.

## Remaining WP-08B Exit Work

1. Publish and review the verified WP-08B candidate.
2. Merge under `PR / policy` and `PR / rust`.
3. On the exact protected-main revision, pass trusted Vulkan checks and open
   the real viewer at 1280x720 on the T2 dataset.
4. Seal the evidence and create the WP-08B exit tag.

The next package after WP-08B follows the approved integration order in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
