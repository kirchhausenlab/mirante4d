# Current Work

Last updated: 2026-07-16

## Current Status

The foundation implementation and deletion audit through WP-15 are complete,
as is the import/preprocessing hard cutover. [Testing and
evidence](../TESTING.md) owns the exact revision-bound qualification record and
accepted remaining risks.

The corrective `uint8` sentinel no-data restoration is also complete. The
production importer now has one guarded sentinel route: exact source
classification plus clipped one-voxel invalid dilation at base, valid-only
half-up recursive means plus the same dilation at every coarse LOD,
canonical-zero invalid storage, centered sentinel-LOD transforms, selective
validity-only halo reads, and explicit recipe/checkpoint bindings. The former
sentinel point-validity route and private-fact bootstrap are gone;
non-sentinel point decimation remains isolated and unchanged.

The closeout evidence is intentionally proportionate:

- Clean production revision `f73cb36` passed the repository/local product set
  and the accepted five-session T2 qualification with a 14.033167153-second
  median against 60 seconds.
- At clean revision `d6478d9`, two independent bounded source-oracle
  derivations produced byte-identical private schema-v2 facts without source
  mutation or retained oracle scratch. This is a one-time fact freeze, not a
  routine prerequisite.
- Clean measurement revision
  `30bb16758d22055deb1e52fe6803b95592094eee` passed all 11 policy phases and
  all eight Rust phases with 924 tests discovered, then passed exactly one
  real-display normal-product private correctness sample. Its package matched
  the frozen scientific, layer, all-seven-scale, centered-transform, canonical
  value/validity, source-preservation, resource, publication, and navigation
  gates.
- A post-measurement sanitizer false positive affected only public-summary
  publication. The finalized private raw receipt remained valid. Clean
  publisher revision `5a9077d407bf5bdc9c78caf75e558e0c63950afa` replayed that
  receipt in reporting-only mode without opening the source or package,
  launching the app/importer, recomputing the oracle, or re-executing the
  measurement.
- Owner observation on the affected workflow also confirms that the visible
  boundary fringe is fixed.

The single correctness sample records timing only as an observation. It does
not establish a median, distribution, determinism across runs, or a current
restored-policy T5 performance claim. A three-sample T5 performance run remains
optional and must be explicitly requested. The accepted T2 evidence is reused
because the later changes are confined to the private fact/report authority,
not the production importer or its performance boundary.

No corrective plan is active. Other unresolved work remains in [the
backlog](../BACKLOG.md); deferred public-data and segmentation outcomes remain
separate from ordinary development.
