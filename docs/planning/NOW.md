# Current Work

Last updated: 2026-07-14

## Current Checkpoint

WP-12 is accepted on protected main at
`5be750d060284d0a591ea6b5c0007bfeb136ac8d` (tree
`d704f02e4dc530c8c144fc5a2f29c572012835a2`), tagged
`foundation-wp-12-exit-1`. The exact analysis path now runs through the shared
bounded scheduler and publishes table/plot pairs atomically through the project
store. The predecessor analysis crate and segmentation code are deleted.

WP-10C is active on `wp10c-storage-runtime-cutover`.

The candidate now uses target storage and import throughout the product,
deletes the predecessor data/format/import crates and obsolete fuzz package,
and passes the focused import-to-project-reopen integration. Remaining
acceptance work is the supported-resolution product exercise and normal PR
checks; inherited exhaustive suites are not rerun.

## WP-10C Entry Note

Outcome: make the accepted sharded target format and importer the only dataset
path used by Mirante4D. Opening remains responsive while verification runs in
the background; only fully proved package and scientific identities may be
saved in a project.

Work is deliberately small and serial: add the target storage adapter, switch
open/verification and project binding, switch the importer, then remove
`mirante4d-data`, `mirante4d-format`, and `mirante4d-import` plus their expired
fixtures and branches. There is no compatibility reader or dual product route.

Evidence is limited to focused adapter tests on the existing small target
archives, one import-to-reopen integration, one relevant corruption or
source-change check, supported 720p/1080p product use, and a mechanical check
that the old crates have no remaining dependents. Earlier storage, import,
project, and analysis evidence is inherited rather than rerun.

Non-goals: huge or simulated datasets, KVM or power-cut matrices, 4K,
segmentation, performance claims, and new workflows, manifests, or ledgers.
