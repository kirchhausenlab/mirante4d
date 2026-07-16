# Current Work

Last updated: 2026-07-16

## Current Status

The foundation implementation and deletion audit through WP-15 are complete.
The first post-foundation task, the multi-hour TIFF import and preprocessing
bottleneck, is complete. At immutable revision
`eb5c9ffd12cbd9fce65bd03559b8e7f93170d72e`, the hard cutover is implemented
and automated-verified; the five-session T2 qualification passed its 60-second
median gate, and the three-session normal-product T5/DS-3 qualification was
product-validated on the owner-accepted HW-2/ext4 tuple with an 11-minute
31.094-second median against the 15-minute gate. [Testing and
evidence](../TESTING.md) owns the exact protocol, revision-bound evidence, and
accepted remaining risks.

The final deletion audit at that revision found the intended single importer
and six-file checkpoint authority, with no predecessor checkpoint reader,
per-work-unit or per-object durability predecessor, decoded publication route,
automatic exact/scientific rescan in the imported publication-to-open-ready
route, fixed-percentage progress route, alternate importer, or hidden selector.
The completed performance handoff has been deleted; Git history is its archive.
All performance and product-validation claims attach only to the named
qualification revision and recorded executables. Revision
`85350219efcc0c96b492f9a5029ba80752b49306`, the clean predecessor to this
restoration, was documentation-only and was not performance-qualified. The
current sentinel cutover does not inherit the older qualification.

Read-only investigation of a new sentinel-bearing dataset found that the prior
importer no longer applied the predecessor's one-voxel invalid guard or
valid-only mean pyramid. The owner approved the active
[uint8 sentinel no-data restoration
plan](../plans/active/UINT8_SENTINEL_NO_DATA_RESTORATION.md) on 2026-07-16.
ND-00 through ND-03 are now implemented: exact source classification and
one-voxel invalid dilation run at base and scientific identity, recursive LODs
use valid-only half-up means plus per-level dilation, halo-only spool parents
perform selective validity reads, centered transforms and explicit recipe and
checkpoint bindings are active, and the old sentinel point-validity route is
gone. Non-sentinel point-decimation remains isolated and unchanged.

Independent generated 2D/3D packages match every expected value and validity
bit across chunk faces and all LODs. A one-sample dirty-worktree release T2
diagnostic also matched the independently frozen scientific identity and all
five scale digests in 5.540 seconds, with a 267,131,688-byte import-ledger peak
under 256 MiB and a 67,313,664-byte RSS delta. Its independent recursive oracle
used temporary-file-backed fixed slabs with a calculated 22,578,060-byte peak
and removed both scratch levels. This is diagnostic evidence, not
qualification.

Clean revision `f73cb36` passed both fixture validators, formatting, qualifying
`verify-pr` with 904 selected Nextest cases passed and three reported skips,
the qualifying six-phase local format lifecycle, and both E1 product-
automation scenarios. Its five owner-qualified T2 sessions had a
14.033167153-second median against 60 seconds; every runtime/resource gate
passed and identities were deterministic.

Owner review then confirmed that the pinned private T5 workload is the
affected sentinel-bearing dataset and that its visible boundary defect
is fixed in normal use. The previous no-sentinel T5 assumption was wrong: its
frozen semantic facts describe pre-restoration output. At clean revision
`d6478d9`, two independent full bounded source-oracle derivations completed and
produced byte-identical schema-v2 candidate configurations without altering the
source or retaining oracle scratch. Those results are the one-time private fact
freeze.

The hard-cut v2 fact/config commitment is now implemented and the legacy
bootstrap path is gone. ND-04 remains active only to run exactly one clean
release normal-product T5 correctness sample, compare its package with the
frozen scientific, layer, seven-scale, and centered-transform facts, and record
the closeout. Routine correctness does not recompute the oracle. A new oracle audit is trigger-only after sentinel
semantics, fact authority/schema, or oracle implementation changes; unexpected
source drift fails its inventory preflight, while intentional source
replacement requires a new fact freeze. A three-sample T5 run is optional and
explicit when a restored-policy
performance claim is wanted; it is not part of correctness closeout. The
five-session T2 evidence at `f73cb36` is reused because subsequent changes are
confined to private xtask fact and evidence authority, not the production
importer or its performance boundary.
Other unresolved work remains in [the backlog](../BACKLOG.md).
