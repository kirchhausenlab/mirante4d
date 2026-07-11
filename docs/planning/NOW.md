# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-07A freezes the canonical model before live product state moves. The
checkpoint adds three pure crates, their exact dependency and side-effect
allowlists, 53 contract/property tests, and a machine-checked disposition for
all 152 fields in the current `AppState` and `MiranteWorkbenchApp`.

The new model remains unreachable from every existing product crate. It does
not add serialization, I/O, canonical hashing, a second live state model, or a
viewer behavior change.

## Remaining WP-07A Acceptance

1. Merge the candidate under `PR / policy` and `PR / rust`.
2. Require matching exact-main policy and Rust checks. Product-open validation
   is not repeated because the candidate cannot affect the product path.
3. Create the annotated, create-once `foundation-wp-07a-exit-1` tag.

## Following Checkpoint

WP-07B will make the canonical project model the sole durable authority,
introduce the typed application command/reducer/event/snapshot boundary, move
non-durable facts to bounded temporary owners, and delete the predecessor
application god-state and `mirante4d-core` authority in the same hard cutover.

Before editing product source, its entry brief must freeze the exact migration
checkpoints, temporary bridges and deletion gates, settings cutover, scientific-
identity gate, and product-open scenario. No product attach/save/open route may
pretend that the current package slug or BLAKE3 value is a verified
`ScientificContentId`.

The complete package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md). The
[backlog](../BACKLOG.md) contains only unresolved work outside this checkpoint.
