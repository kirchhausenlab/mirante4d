# Current Work

Last updated: 2026-07-13

## Current Checkpoint

WP-10B B1 and B2 are complete. B2 is accepted on protected main at
`4a246a1bb7bfe099673ef10d6cb5951729b3ff37` (tree
`af5531d8ffbda0c13b342a0b4df47a894e7f99fb`). Its clean aggregate passed all
120 hosted tests and 60 rootless-VM cuts with zero retries; protected-main
policy and Rust checks passed in
[run 29273392030](https://github.com/kirchhausenlab/mirante4d/actions/runs/29273392030).

B3 is the current checkpoint. It adds bounded current-source D-009
verification, source-generation-aware completion and invalidation, atomic
verified-catalog/runtime replacement, exact revision-based autosave scheduling,
and an application service over the accepted project-store actor. The new
service remains unreachable from the product during B3: the private project-v15
bridge and `CurrentProjectRuntime` stay the sole product route until B4.

B4 will switch product save/open/autosave/recovery atomically to the successor,
delete the complete project-v15 path, and run the required real-viewer checks at
1280x720 and 1920x1080. WP-10B does not exit before that deletion and evidence.

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

WP-10B is a product persistence hard cutover: the private project-v15 bridge
must be deleted in the same accepted package, no legacy reader or converter may
remain, and real product-open validation is required. WP-11 is the next
protected-branch checkpoint, following the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
