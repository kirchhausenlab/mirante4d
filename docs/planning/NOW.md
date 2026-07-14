# Current Work

Last updated: 2026-07-14

## Current Checkpoint

WP-10C is accepted on protected main at
`b9ac2a5f08101094933f80a0ce98fbdbdbe6c8d6`, tagged
`foundation-wp-10c-exit-1`. The sharded target package and bounded importer are
the sole product storage/import route; the predecessor crates are deleted.

WP-09B is active on `wp09b-product-render-cutover`.

## WP-09B Entry Note

Outcome: make the accepted progressive WGPU renderer the only renderer used by
the viewer, present useful current partial frames, and delete the predecessor
renderer and complete-residency gate.

The cutover will reuse the viewer's existing WGPU device, connect target leases
to backend-neutral render intents, retain one narrow native-texture
presentation seam in the composition root, then remove the old renderer crate
and fallback branches.

Evidence is limited to focused intent/lease, partial/current, stale-frame, and
typed-capacity checks; a mechanical old-route deletion check; the normal PR
checks once; and one real viewer exercise at 1280x720 and 1920x1080 using the
existing small target package. WP-09A's renderer qualification is inherited.

Non-goals: performance claims, large-data simulations, 4K, segmentation, a new
workflow, or repeating the WP-09A qualification matrix.
