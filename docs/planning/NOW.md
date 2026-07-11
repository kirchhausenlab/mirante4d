# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-05 is the completion checkpoint represented by this revision. It is bound
to the clean `foundation-wp-04-exit-1` predecessor at
`5872e7cdf27040dd65fe324d6daf6b0e4e7bd32e`.

WP-05 installs one 32-document authority tree, deletes the 50-file
specification directory and ten other redundant documents, replaces the nested
release document, and adds the bounded `cargo xtask docs-check`. It changes
documentation and developer verification only; it does not change product
behavior.

## Next Package

WP-06 replaces the temporary bootstrap and legacy test topology with bounded,
nonrecursive, requirement-owned verification leaves. No WP-06 implementation
starts until its entry brief is bound to the accepted WP-05 exit revision.

Known inputs to WP-06:

- `verify-fast` has a superseded source-size failure;
- `report-audit` has an inherited evidence mismatch;
- the full test topology is slow and duplicated; and
- the current hosted workflow is a provisional single bootstrap job.

The complete package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md). The
[backlog](../BACKLOG.md) contains only unresolved work outside this checkpoint.
