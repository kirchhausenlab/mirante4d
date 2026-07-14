# Current Work

Last updated: 2026-07-13

## Current Checkpoint

WP-10B is accepted on protected main at
`8257f8c5bdc011651c8e74ab85dfdc86717b82d6` (tree
`56e4ac27f50311b49226520ae6c382aacfe9dde6`), tagged
`foundation-wp-10b-exit-1`. Native project persistence is the sole product
route. Its exhaustive durability matrix is not a recurring gate for unrelated
work.

WP-11 is an exit candidate on the current branch. The off-product
`mirante4d-import-pipeline` now performs bounded, cancellable, restartable
TIFF/OME-TIFF import into validated sharded target packages. Its focused tests
pass, and an importer-produced package passed the existing independent target
reader. The current importer remains the sole reachable product route until
WP-10C activates the replacement and deletes the predecessor.

Protected-main acceptance and the create-once WP-11 exit tag remain pending,
so WP-12 has not started. Analysis execution remains unavailable until WP-12.

## WP-11 Entry Note

Predecessor: clean `foundation-wp-10b-exit-1` at the commit and tree above.

Outcome: one bounded, cancellable, restartable, deterministic importer that
turns reviewed TIFF/OME-TIFF sources into the accepted sharded target format,
records reproducible source/recipe/derivation facts, validates the result, and
publishes it atomically to a previously absent destination.

Inherited invariants: source bytes are never modified; memory, queues, I/O, and
physical objects are bounded; target storage stays sharded; cancellation never
exposes an incomplete package; scientific identity is storage-independent;
segmentation remains absent.

Allowed scope: the new `mirante4d-import-pipeline` crate, its focused tests,
small supporting changes to accepted target APIs where immediately necessary,
the existing contract-lane registry, and owning documentation. The target
crate may depend only on `domain`, `identity`, `dataset`, and `storage`.

Authority and deletion: WP-11 owns the off-product replacement producer. It
does not wire a second UI/application route or delete `mirante4d-import`;
WP-10C performs that activation and predecessor deletion.

Non-goals: replacement/backup publication, proprietary formats, generic
OME-Zarr import, remote stores, public-data release machinery, private-data or
simulated-TiB qualification, hard throughput claims, GPU/display work, and
segmentation.

Risks and stop conditions: stop for owner review if the accepted storage or
identity wire must change, if the TIFF decoder cannot honor the declared byte
budget without narrowing supported input, or if restart would require a
file-per-brick scratch layout.

Evidence: focused contract tests for source layout/dtype rejection, bounded
streaming, deterministic output, cancellation/restart and corrupt-checkpoint
handling, free-space refusal, source/destination preservation, and atomic
publication; one accepted-source-fixture import checked with exact/scientific
target validation and the existing independent target reader. Product-open,
private T5, and repeated performance matrices do not apply while WP-11 remains
off-product.

Rollback unit: the WP-11 branch/checkpoint; no product authority changes before
WP-10C.
