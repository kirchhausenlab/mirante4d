# Current Work

Last updated: 2026-07-11

## Current Checkpoint

This revision implements the WP-06A shadow machinery and is bound to the clean
`foundation-wp-05-exit-1` predecessor at
`97ba103463a419d696b445c414515b17a5df215f`. WP-06 is not yet accepted.

The checkpoint installs six nonrecursive leaves, exact test and fixture
ownership, trusted-local GPU/product separation, and shadow PR/Main policy and
Rust workflows. It discovers 879 live tests: 839 normal tests and 40 trusted
GPU tests. The legacy recursive gates, `verify-fast`, stale report scanner, and
ignored WGPU PNG snapshots are deleted rather than renamed.

`Bootstrap / required` remains the sole required status context. Target-format
T1 is still false, and package capability remains pending.

## Remaining WP-06 Checkpoints

1. Integrate and accept this checkpoint under `Bootstrap / required`.
2. Run and accept twenty consecutive cache-free Main attempts.
3. Replace and read back the required contexts with `PR / policy` and
   `PR / rust`.
4. Delete the bootstrap bridge in a separate protected checkpoint.
5. Complete exact-revision product-open validation and create the WP-06 exit
   tag.

The complete package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md). The
[backlog](../BACKLOG.md) contains only unresolved work outside this checkpoint.
