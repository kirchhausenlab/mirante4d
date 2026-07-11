# Current Work

Last updated: 2026-07-11

## Current Checkpoint

WP-07A is accepted at `5383cbb93c13c59e6f035bfa551356c75fb426dc`
(`foundation-wp-07a-exit-1`).

WP-07B-A now implements four real boundary candidates:
`mirante4d-application`, `mirante4d-settings`, `mirante4d-dataset`, and
`mirante4d-render-api`. They remain unreachable from every existing product
crate, so they do not change viewer behavior, live state, persistence,
settings, or `mirante4d-core` authority.

## Remaining WP-07B-A Acceptance

1. Complete the candidate audit and required local checks.
2. Merge the unreachable candidate under `PR / policy` and `PR / rust`.
3. Require matching exact-main policy and Rust checks. This checkpoint cannot
   affect the product path and makes no live-cutover claim.

## Following Live Checkpoint

WP-07B-B will activate the accepted boundaries, make the canonical project
model the sole durable authority, move non-durable facts to their named
temporary owners, hard-cut settings and project persistence, and delete the
application god-state and `mirante4d-core` authority atomically. Its product
attach/save/open routes remain unavailable until a real verified
`ScientificContentId` exists.

The complete package order and acceptance rules live in the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md). The
[backlog](../BACKLOG.md) contains only unresolved work outside this checkpoint.
