# Current Work

Last updated: 2026-07-12

## Current Checkpoint

WP-09A is accepted and immutable at `foundation-wp-09a-exit-1`
(`1b1e7d5534f29b010cc346d434811a3906fb40e1`). WP-10B's entry is accepted at
`4d0e4853637a0466ca37548134822c5ec83a240f`. Its B1 wire contract and
independent fixture are frozen; B2 transactional store implementation is
active off-product. Its first core checkpoint provides the exact crate/API
boundary, control-record wire, and immutable object primitive. Its current
checkpoint adds typed canonical generations, direct and deterministic paged
closure, generation-last immutable publication, process-held maintenance and
writer leases, an exact no-replace initial manual head, and crate-private
established manual recovery-before-head replacement with bounded store
inventory and recovery-ahead retry. The same transaction now creates and
advances an established-project autosave lane, including divergent-base and
lower-revision cases. A crate-private established-session actor now owns the
root and leases, serializes those manual/autosave primitives, and enforces the
frozen queue, coalescing, cancellation, close, and shutdown rules. One shared
private inspection core now opens and validates established stores for actor
startup and transaction preflight, with bounded object-metadata checks and no
eager bulk-payload hashing. A shared crate-private initial-package transaction
now gives future Create and Save As one destination-local sibling-stage,
full-tree-sync, no-clobber install path with a caller-bound fork tuple and
retained leases. The private established-session actor now authenticates Save
As against the live manual head and scientific identity, then changes its owned
root and leases only after that transaction durably installs the fork. B2
now also has one bounded metadata graph over established or provisional store
state, canonical generation/object namespace enumeration, exact ref/pin roots,
and capped orphan candidates. It performs no repair, payload hashing, or
deletion. B2 remains active off-product: public Create/Open/Save As execution,
provisional autosave publication, recovery/open, timers, garbage collection,
full verification, and public actor wiring remain later work, with product
activation at B4.

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
