# Dataset Fixtures

Status: APPROVED FOUNDATION POLICY
Last updated: 2026-07-11

## Public Fixture Tiers

- `T1`: small, immutable, independently produced/read source and target-format
  conformance vectors. WP-03 publishes only the approved TIFF source families;
  target-format T1 authority begins after WP-10A freezes the schema.
- `T2`: generated support fixtures used for routine unit, component, UI, and
  stress tests. They are useful evidence but cannot establish independent
  format or scientific correctness by themselves.
- `T5`: private qualification data used only on trusted machines. Public files
  may contain the opaque IDs `T5-QUAL-001`, `T5-QUAL-002`, and
  `T5-QUAL-003`; exact paths, experiment labels, identities, and digests live
  only in the private resolver.

No full microscopy dataset is committed to this repository. External dataset
contributions are not accepted under the source-contribution policy.

## Required Coverage

The evolving fixture registry must cover bounded examples of:

- integer and finite floating-point intensity data;
- zeros and explicit no-data values;
- anisotropic calibration;
- multiple channels and timepoints;
- chunk/shard boundaries and sparse signal;
- corrupt, truncated, contradictory, ambiguous, and unsupported inputs; and
- cancellation, capacity, and transactional failure cases where applicable.

Every authoritative fixture records provenance, license, immutable digest,
archive limits, producer/fact/reader lineage, and the exact requirements it
supports. Private data never becomes an implicit CI dependency.
