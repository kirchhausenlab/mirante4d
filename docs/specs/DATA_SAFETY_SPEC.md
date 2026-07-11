# Data Safety Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define how Mirante4D protects source data, preprocessing outputs, caches, generated datasets, and analysis artifacts.

## Scope

This spec covers source file handling, atomic writes, partial outputs, cache directories, cleanup, disk-space checks, corruption detection, and derived analysis output safety.

## Non-Goals

- Editing source microscopy files in place.
- Treating generated datasets as disposable without warning.
- Silent repair of corrupt data.
- Silently writing analysis outputs into native dataset packages by default.
- Treating preview/approximate analysis output as exact final output.

## Requirements

- Source data must never be modified without explicit user action.
- Preprocessing should write into a temporary/incomplete output location first.
- Output should be atomically finalized where the platform/filesystem permits.
- Incomplete outputs must be detectable.
- Critical payloads should have checksums or equivalent validation metadata where useful.
- Disk-space requirements should be estimated before large preprocessing writes where practical.
- Cache files must be separate from user source data and generated native datasets.
- Analysis artifacts must be written as structured project/session or sidecar outputs by default.
- Final analysis artifacts must record provenance and completion status.
- Cancelled or failed analysis jobs must not leave artifacts that look complete.

## Output Lifecycle

Recommended lifecycle:

1. create temporary output directory
2. write manifest with incomplete status or write manifest last
3. write shards/indexes
4. validate output
5. atomically finalize or mark complete
6. only then offer dataset for viewing

Current import implementation:

- TIFF import computes a deterministic storage estimate after metadata inspection and before source stack conversion.
- The estimate covers source payload bytes, derived multiscale payload bytes, metadata overhead, total native package bytes, and peak decoded stack bytes.
- The app shows the estimate during import review, and the background import task emits it as a progress event before reading/writing stack payloads.
- The estimate is intentionally not yet a hard free-space gate; adding platform-specific available-space checks requires a separate dependency/policy decision.

## Cache Policy

- Cache location should be platform-appropriate.
- Cache contents should be rebuildable.
- Cache cleanup should be explicit and safe.
- Cache corruption should not corrupt source data or native datasets.

## Analysis Artifact Safety

Analysis artifacts include annotations, masks, measurements, plots, statistics,
and exports.

Requirements:

- source data is never modified by analysis tools
- native dataset packages are not silently mutated by analysis tools
- large derived outputs are written through an incomplete/complete lifecycle
- final artifacts record provenance and exact/approximate/preview status
- preview artifacts cannot masquerade as exact final artifacts
- failed or cancelled jobs leave detectable incomplete output or clean up safely
- export paths require explicit user action

## Invariants

- No source overwrite by default.
- No viewer launch from unvalidated preprocessing output.
- No silent acceptance of incomplete output.
- No cleanup routine should delete paths outside its owned cache/output directory.
- No final analysis artifact without provenance.
- No final analysis artifact from incomplete data.
- No silent approximation.

## Failure Modes

- insufficient disk space
- partial write
- app crash during preprocessing
- permission error
- checksum mismatch
- stale cache
- user moves dataset while open
- analysis job cancellation leaves partial artifact
- derived output references missing source dataset
- approximation metadata missing

## Testing Requirements

- Partial-output detection tests.
- Cancellation cleanup tests.
- Atomic finalize tests where practical.
- Corrupt payload tests.
- Source data immutability tests.
- Cache path safety tests.
- Analysis artifact incomplete/finalized lifecycle tests.
- Provenance-required tests.
- Preview/approximate output safety tests.

## Open Questions

- Exact incomplete marker strategy.
- Whether manifest is written first with status or last after payloads.
- Cache location per OS.
- Cleanup UI policy.
