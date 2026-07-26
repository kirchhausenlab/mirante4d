# Current Work

Last updated: 2026-07-26

## Current Status

The foundation refactor through WP-15, import/preprocessing cutover, and
corrective `uint8` sentinel restoration remain implemented. Their scientific,
source-safety, persistence, bounded-resource, and product-boundary guarantees
remain in force.

The subsequent viewer-performance program became disproportionate: development
qualification and provenance machinery grew larger than the product changes,
while owner testing still found wheel freezes, unjustified four-panel LOD, and
visible brick assembly. The owner therefore authorized the
[development-simplification and viewer-recovery plan](../plans/active/DEVELOPMENT_SIMPLIFICATION.md)
on 2026-07-26. It supersedes EP-00 through EP-07.

EP-00 and EP-01 evidence protocols are frozen, nonblocking, and scheduled for
deletion or reduction. They establish no current performance claim and are not
prerequisites for implementation.

## Active Checkpoint

R0 recovery and R1 authority/process cutover are active.

The damaged linked viewer worktree has been preserved outside the repository.
The 199 recoverable commits through `3d967f0` and the surviving seven-file
working delta were reconstructed as clean commit `3dfe9de`. A complete bundle
and an independent clone both preserve `main`, the recovered branch, and the
foundation tags; the clone passed `git fsck` and matched the worktree source
byte-for-byte.

The immediate ordered actions are:

1. install the recovered branch in the live repository and repair the corrupt
   linked-worktree metadata without deleting the verified recovery material;
2. activate proportional edit, merge, affected-boundary, and release
   verification tiers;
3. remove or isolate EP-00/EP-01 schemas, receipts, replay, repeated hashing,
   and parallel successor implementations;
4. classify and retain only useful successor product mechanisms; and
5. deliver resident interaction/LOD, cold complete refinement, and MIP/DVR/ISO
   improvements as separately product-validated slices.

No storage-format rewrite is presumed. It requires measured evidence that
unique byte/decode amplification remains a dominant blocker after runtime
fixes.

## Claim Boundary

The current normal product remains the integrated first-overhaul baseline on
`main`. Recovered successor code is an unpublished research spike until a
product-only slice is accepted.

No absolute performance, comparison-viewer, release, or successor-completion
claim is active. Rendering work becomes complete only after focused automated
checks and normal real-display exercise of the affected workflow.

Other maintenance remains in [the backlog](../BACKLOG.md). Public-data and
segmentation outcomes remain deferred and separate.
