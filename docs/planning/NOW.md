# Current Work

Last updated: 2026-07-14

## Current Checkpoint

WP-11 is accepted on protected main at
`04987f64c309166caddf931be9c1ef4948010128` (tree
`fd39e0be3a0883726972b25d037c916f0e3ca4c0`), tagged
`foundation-wp-11-exit-1`. Main run
[29330265968](https://github.com/kirchhausenlab/mirante4d/actions/runs/29330265968)
passed policy and Rust checks. The accepted producer remains off-product;
`mirante4d-import` stays the sole product route until WP-10C activates the
replacement and deletes the predecessor.

WP-12 is active on `wp12-analysis-runtime`. Analysis execution remains
unavailable while the successor is built and cut over.

## WP-12 Entry Note

Predecessor: clean `foundation-wp-11-exit-1` at the commit and tree above.

Outcome: one typed, bounded product analysis path for exact full-intensity
summaries/time traces and axis-aligned box-ROI intensity statistics. Work uses
the shared scheduler below interactive priority, supports progress and scoped
cancellation, and makes a complete table/plot bundle visible only after the
existing project store commits it.

Inherited invariants: dataset access uses semantic scheduler requests and
accounted leases; viewing remains responsive; source and accepted dataset
packages are immutable; incomplete, failed, cancelled, or stale work cannot
appear complete; artifact identity and provenance are reproducible; and
segmentation remains absent.

Allowed scope: new `mirante4d-analysis-core` and
`mirante4d-analysis-runtime` crates, focused changes to the accepted dataset
runtime, application, project model/store, product shell, architecture checks,
and owning documentation. WP-12 reuses those authorities rather than adding a
second scheduler or persistence path.

Scientific scope: the authoritative route is exact-only for `uint8`, `uint16`,
and finite `float32`. It freezes validity handling, deterministic traversal and
accumulation, and population-variance semantics against small hand-computed
facts. Approximate and preview execution are rejected rather than partially
implemented.

Authority and deletion: WP-12 switches the sole product analysis route to the
successor and deletes `CurrentAnalysisRuntime`, direct dataset scans, and the
`mirante4d-analysis` predecessor without a compatibility facade. WP-10C later
changes only the dataset-source binding beneath this contract.

Non-goals: segmentation or tracking algorithms, additional scientific
operations, percentile or approximate analysis, a general export redesign,
performance claims, private or simulated huge datasets, KVM/power-cut reruns,
GPU work, 4K requirements, and new evidence manifests or workflows.

Risks and stop conditions: stop for owner review if the accepted dataset,
identity, or project-store wire must change incompatibly, if table and plot
artifacts cannot commit in one project generation, or if the runtime cannot
remain bounded without a second scheduler/poller.

Evidence: a few hand-computable scientific cases; focused scheduler-priority,
memory, cancellation, stale-result, and failure tests; one atomic table/plot
save-and-reopen integration; and one small product exercise covering cancel,
complete, save, and reopen at the supported display sizes. Accepted WP-10B
durability evidence is inherited and is not rerun.

Rollback unit: the WP-12 branch/checkpoint; the predecessor is deleted only in
the final product cutover commit.
