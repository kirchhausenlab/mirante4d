# Current Work

Last updated: 2026-07-13

## Current Checkpoint

WP-10B B1 through B3 are complete. B3 is accepted on protected main at
`8fdd94dc9c60406e8de8a96749d7148d38b1dc7a`.

B4 is the current implementation candidate. The product now constructs and
polls `ProjectStoreApplicationService` as its sole project-persistence route;
New, Open, Save, Save As, revision-aware autosave, recovery, dirty close, and
actor join use the accepted project-store actor. The project-v15 bridge and
`CurrentProjectRuntime` files are deleted, with architecture and predecessor
guards enforcing their absence.

The fixed `b4_project_persistence` automation implements the required bounded
three-launch save/autosave/external-kill/recovery/Save-As/final-reopen
scenario. B4 and WP-10B remain unaccepted until one exact clean revision passes
the public, trusted project-store-lifecycle, and required real-display evidence
at 1280x720 and 1920x1080, then lands on protected main.

The unified runtime is the sole live interactive dataset-demand and CPU-byte
authority. Analysis execution remains unavailable until WP-12.

## WP-10B Entry Boundary

The accepted entry's sole correction permits `mirante4d-project-store` to
depend directly on the unchanged `mirante4d-domain` API so it can encode and
reconstruct the domain-owned values in `ProjectGenerationProjection`. It does
not authorize another persistence DTO owner or any other dependency change.

The entry must bind to tag `foundation-wp-09a-exit-1`, commit
`1b1e7d5534f29b010cc346d434811a3906fb40e1`, and tree
`42846232a04ec7548a3bb9b1b6598e79be29e72b`. It must freeze the final
directory-backed project-store contract, persistence-owned canonical records
and independent fixtures, background actor and lease ownership, exact
revision/autosave/recovery behavior, bounded object growth, and the declared
Linux durability/fault-injection matrix. Project binding remains gated on a
verified D-009 scientific identity.

WP-10B is a product persistence hard cutover. The B4 candidate deletes the
private project-v15 bridge without a legacy reader or converter; acceptance
still requires the clean-revision evidence above. WP-11 is the next protected-
branch checkpoint, following the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
