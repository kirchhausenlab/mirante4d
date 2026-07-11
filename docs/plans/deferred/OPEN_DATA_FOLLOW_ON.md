# Mirante4D Open Data — Deferred Follow-On Plan

Status: DEFERRED under D-021
Program version: 0.21 (deferred scope split under D-021)
Last updated: 2026-07-10
Implementation authorization: NO
Scope source: D-021
Parent boundary: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Entry gate: accepted technical foundation plus separate explicit owner approval
Intended successor: `docs/plans/active/OPEN_DATA_RELEASE_HANDOFF.md`

This plan is outside the foundation implementation handoff. It cannot block
technical-foundation or public-source completion, and it authorizes no dataset
selection, upload, DOI, visibility, or release action.

## Deferred Public Data Release Contract

Status: FOLLOW-ON ONLY under resolved D-021.

Public code does not automatically authorize public data. Each dataset release
must pass a separate data-release gate in a separately approved follow-on
handoff. The foundation handoff must make the format, importer, provenance,
identity, validation, and small-fixture contracts capable of supporting that
future release, but it must not select/upload full release candidates or wait on
hosting, DOI, or dataset-specific release approval.

SpatialDINO's already public S3 objects are the first candidate inventory for
the follow-on. WP-13A must map exact object/release identities to ownership,
data licenses, citations, and source-to-native provenance; it must not treat a
public bucket, repository README, or the preprint's own publication license as
a substitute for those artifact-level records.

### Required Release Classes

The plan must distinguish:

- original/raw acquisition data;
- canonical processed scientific data;
- Mirante4D native packages;
- regenerable acceleration/index artifacts;
- annotations, tracks, measurements, or other derived artifacts;
- small redistributed test/benchmark subsets.

Each class needs a declared owner, source of truth, versioning policy, license,
provenance relationship, and retention policy.

### Required Dataset Release Metadata

Every public dataset release must include:

- stable dataset and release identifiers;
- immutable file/object manifest with sizes and cryptographic digests;
- dataset and artifact licenses;
- rights/consent/privacy/ethics statement appropriate to the source;
- data-use agreements, material-transfer terms, embargoes, export restrictions,
  institutional restrictions, and named release-approval roles where
  applicable;
- creators, institution, funders, citation text, and machine-readable citation
  metadata;
- acquisition context and scientific metadata sufficient for interpretation;
- axis, unit, dtype, channel, time, spatial-transform, validity/no-data, and
  preprocessing semantics;
- raw-to-processed-to-native provenance graph;
- application, importer, writer, pipeline, schema, and storage-profile versions;
- exact/approximate/preview status for derived data;
- validation report and known limitations;
- download/resume/integrity instructions;
- reproducible commands or workflow for derivable artifacts;
- deprecation/supersession relationship to earlier releases.

### Follow-On Open Data Decisions

- Which datasets may legally and ethically be published.
- Whether raw acquisitions, processed data, native packages, or all three are
  published.
- The data license for each artifact class.
- The primary archival host, DOI/citation provider, and optional high-throughput
  mirror.
- Sustainable storage and egress expectations for multi-gigabyte or larger
  releases.
- How immutable versions, corrections, and superseding releases are represented.
- Which public dataset becomes the canonical T3 integration fixture and which
  becomes the T4 performance/stress fixture.
- Which independent OME-NGFF/M4D conformance evidence accompanies the exact
  already-approved D-007 target profile in a named public release.
- Which approved D-008 regeneration/conversion remedy and deprecation metadata
  accompany any future incompatible stable-major cutover.

These decisions do not block foundation-hand-off approval or public source
release. No public-data upload should begin until a separate follow-on is
approved, these decisions are resolved, and the release candidate can be
reproduced and validated from a clean environment.

### WP-13A — Public Data Registry And Staged Release Candidates

Status: DEFERRED FOLLOW-ON — excluded from the foundation implementation
handoff by resolved D-021.

Goal: prepare rights-cleared staged datasets and reproducible Mirante4D
derivatives for a versioned scientific release without claiming candidate
acceptance or publishing before WP-13V.

Required work:

- Resolve the decisions gated at WP-13A start/exit, especially D-011, D-012,
  and D-014. For publication-only decisions, record the owner, blocked default,
  and latest gate rather than pretending they are already closed.
- Create a public dataset registry and candidate-manifest schema. Archival host,
  DOI, mirror, and contribution-governance fields remain explicitly pending
  until their declared publication gates when not already resolved.
- Validate rights, privacy/ethics, licenses, citation, provenance, checksums,
  metadata, and known limitations.
- Produce T3 and T4 staged release candidates through the approved import
  pipeline.
- Test resumable download, integrity verification, open, render, and every
  declared release workflow from a clean environment. Analysis and derivation
  are candidate gates only when the candidate includes analysis-derived
  artifacts or the release makes analysis/reproduction claims about it.
- Separate canonical data from regenerable acceleration artifacts.
- Define whether each derived artifact requires bitwise reproducibility or
  semantic equivalence with declared tolerances; do not infer byte identity
  merely from the presence of a digest.

Exit proof:

- Staged medium and large candidates have immutable candidate manifests,
  provisional citation metadata, and independently reviewed expected facts.
- Candidate reproduction/validation reports bind the exact candidate digests or
  declared semantic-equivalence policy.
- No private T5 dataset is required for ordinary public contributor gates.

### WP-13V — Candidate-Specific Public-Data Acceptance

Status: DEFERRED FOLLOW-ON — excluded from the foundation implementation
handoff by resolved D-021.

Goal: bind the exact immutable WP-13A candidate to the completed technical
foundation and release evidence before any irreversible upload/DOI action.

Required work and exit proof:

- Re-run every follow-on-approved public-data reproducibility, integrity, format,
  import, render, analysis-if-in-scope, performance-if-claimed, package, and
  product-open gate against the exact candidate digest after WP-15 passes.
- Produce a signed candidate-acceptance evidence-set manifest with the named
  technical, scientific, data-custodian, and rights approvals required for
  candidate acceptance, plus any valid waivers, artifacts, and freshness
  bounds. This manifest does not claim the later publication/DOI approvals.
- Any candidate byte change invalidates acceptance and repeats WP-13V. Later
  publication may add release IDs, DOI, locations, and metadata that do not
  alter the accepted data bytes; otherwise validation repeats.

### WP-13B — Public Data Publication

Status: DEFERRED FOLLOW-ON — excluded from the foundation implementation
handoff by resolved D-021.

Goal: publish only the exact WP-13A candidate digest accepted by WP-13V and the
required rights/institutional/data-owner approvers.

Required work:

- Obtain the named release approvals and verify no embargo, agreement, privacy,
  ethics, export, ownership, or institutional blocker remains.
- Verify the upload input byte-for-byte matches the WP-13V evidence-set digest;
  WP-14 or WP-15 evidence alone cannot authorize publication.
- Upload immutable versioned artifacts to the approved archival host/mirror.
- Assign stable identifiers/DOIs, publish the already accepted candidate
  digests, licenses, citation metadata, release notes, and
  supersession/withdrawal policy.
- Verify public anonymous download, resume, integrity, open, render, and declared
  reproduction workflows.
- Produce a separately signed final publication manifest that references the
  immutable WP-13V candidate-acceptance manifest and records the exact accepted
  byte digests, release identifiers/DOIs, public locations, licenses, named
  publication approvers, and post-publication verification results.
- Corrections create superseding releases rather than silently replacing
  content.

Exit proof:

- The approved public-data scope has stable citation and anonymous verified
  access.
- Published bytes match the WP-13V candidate digest, and the published bytes,
  release metadata, locations, and approvals match the separately signed final
  publication manifest.
- No private path, identity, credential, or unapproved metadata appears in the
  release or retained public evidence.
