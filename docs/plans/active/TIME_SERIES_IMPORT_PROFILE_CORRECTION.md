# Time-Series Import Profile Correction Plan

- Status: COMPLETE — IMPLEMENTED, AUTOMATED-VERIFIED, AND OWNER PRODUCT-VALIDATED
- Planning requested by owner: 2026-07-31
- Implementation authorized by owner: 2026-07-31
- Completed: 2026-08-01
- Last reviewed: 2026-08-01

This document is the completed implementation handoff for restoring TIFF
preprocessing of large-time-axis datasets after the geometry-derived pyramid
cutover, making source-layout and capacity failures actionable, and producing
one validated maintainer-requested local package without modifying its source.

## Post-Closeout Supersession

The shared pyramid-geometry authority and truthful rejection diagnostics from
this plan remain current. Later large multichannel/time-series investigation
showed that calibrating aggregate DS ceilings to representative complete
geometries is not a sound long-term admission policy. The approved
[large-dataset preprocessing and storage cutover](LARGE_DATASET_PREPROCESSING_AND_STORAGE_CUTOVER.md)
supersedes that envelope policy with compositional admission. This plan remains
the historical record of the completed geometry correction; it is not the
target authority for aggregate dataset scaling.

## Closeout

Storage now owns the shared pyramid geometry used by profile accounting and
import production, representative DS envelopes follow that authority, and
typed profile rejections survive through actionable source and UI diagnostics.
Focused checks and the broad PR gate passed. The owner subsequently completed
the normal preprocessing workflow and opened the resulting package in the
application. No work remains in this plan's scoped geometry/diagnostic
correction; its later aggregate-scaling replacement is owned by the
large-dataset cutover above. Private product evidence stays outside the
repository.

## Outcome

The completed change provides all of the following:

1. pyramid geometry has one shared storage-owned authority used by import
   generation and storage-envelope accounting;
2. every representative DS envelope is recalibrated against the current
   terminal geometry rather than a stale ordinal scale count;
3. a source that exceeds every supported profile reports the concrete profile
   metric, observed value, and ceiling that rejected it;
4. selecting a parent whose children are multipage TIFF series explains the
   interpretation and directs the user to the appropriate child series rather
   than implying that those TIFFs are malformed;
5. the import window no longer follows a precise capacity failure with generic
   and contradictory TIFF-selection advice; and
6. the requested local time-series source is imported create-only and the
   published package passes the normal structural, exact, and scientific
   validation route.

The local source identity, filesystem paths, calibration values, and package
identities remain private evidence and must not enter repository documentation
or fixtures.

## Diagnosed Regression

The native inspector accepts the intended directory as an ordered series of
multipage grayscale float TIFF stacks. The subsequent planning failure is not
a TIFF compatibility failure.

The geometry-derived pyramid cutover correctly changed generation to continue
until both terminal conditions are satisfied, but the logical-brick and shard
ceilings for the DS envelopes still encode their predecessor scale counts. A
representative multi-timepoint boundary therefore acquires one legitimate
terminal resource per timepoint and is rejected by exactly that omitted tail.
Several other representative envelopes have the same latent mismatch.

The importer currently owns a second copy of the terminal predicate, while
storage tests count a caller-supplied number of levels. That split allowed the
generation contract and admission limits to drift. Profile selection then
discards every concrete `StorageProfileError` and returns a generic
`NoSupportedProfile` message. The UI compounds that problem by suggesting a
different TIFF selection for every non-checkpoint failure.

## Non-Negotiable Invariants

### Scientific and source safety

- Source microscopy files are read-only and must be generation-checked before,
  during, and after import through the existing importer.
- Missing spatial calibration is never silently replaced with unit spacing.
  A package is produced only from explicit positive finite Z/Y/X micrometre
  spacing and, for a time series, an explicit positive finite time step.
- The existing dtype, finite-float, codec, TIFF-IFD, page-count, and grayscale
  admission checks remain unchanged.
- Segmentation is not inferred from neighboring directories and remains out of
  scope.
- Publication stays create-only, staged, validated, and atomic. An incomplete
  stage never appears as a complete `.m4d` package.

### One pyramid and profile authority

- `mirante4d-storage` owns the active profile's terminal constants and pure
  factor-two geometry sequence.
- `mirante4d-import-pipeline` owns pixel reduction and production, but consumes
  the storage-owned geometry sequence instead of carrying another terminal
  predicate.
- Profile ceilings remain finite. They are recalibrated only to the exact
  counts required by their existing representative envelopes under the active
  geometry contract.
- Tests derive representative scale counts and brick/shard counts from that
  shared authority. No test may hard-code a predecessor ordinal independently
  of the current geometry.
- There is no compatibility reader, alternate package profile, implicit
  profile escalation outside the fixed DS order, or dataset-specific branch.

### Actionable failures

- Profile selection may skip only typed envelope-count mismatches.
- If every profile rejects the plan, the terminal error retains and displays
  the concrete rejection from the final attempted profile rather than erasing
  it.
- A channel-folder interpretation that encounters multipage files reports
  that interpretation and tells the user to select the multipage series child
  directly. It does not recurse heuristically or ignore sibling content.
- UI guidance must agree with the failure text and must not relabel a capacity
  fault as an unsupported TIFF source.

### Bounded local production

- Before import, the exact plan must prove working-memory and free-space
  requirements against the selected settings and destination filesystem.
- Existing bounded worker, checkpoint, cancellation, descriptor, and spool
  contracts remain authoritative.
- A pre-existing destination is never overwritten. A pre-existing checkpoint
  is reused only through its existing binding and integrity checks.
- Completion requires the importer-issued validated publication capability;
  writer/reader self-agreement alone is not substituted for the established
  scientific validation route.

## Architecture And Hard Cut

### Shared geometry

Storage will expose the active terminal constants and a pure bounded function
that returns the complete `[S0, ..., terminal]` `Shape4D` sequence. Its result
preserves time and ceil-divides every spatial dimension by two. The importer
deletes its local stop constants, terminal predicate, and shape-generation
loop and delegates to that function.

Storage count checks will accept a base geometry and derive the scale count
from the same sequence. The existing explicit-scale arithmetic remains only
where package admission must count the levels actually declared by an
external package.

### Recalibrated DS envelopes

Each existing representative DS geometry will be run through the shared
sequence. Its profile's logical-brick, pixel/validity-shard, and packed-index
ceilings will be updated to those exact derived counts. Tests will prove:

- every representative envelope fits its intended profile;
- one unit above every ceiling still fails;
- the time axis is not reduced;
- small, odd, long-thin, and maximum-representable shapes still terminate
  within the 64-level bound; and
- the defining multi-timepoint case selects a supported profile without
  private filesystem data.

### Diagnostics

`ImportError::NoSupportedProfile` will retain the final typed envelope
rejection and include it in its display text. Application failure
classification remains `ImportCapacityExceeded`. Source discovery will keep
its strict layouts but replace the misleading multipage-in-channel-folder
message with an explanation of the selected-root interpretation. The import
window's non-retry footer will instruct the user to correct the displayed
problem and begin again, rather than always recommending another TIFF.

## Deleted Behavior

Implementation removes:

- importer-local terminal constants and geometry loop;
- representative profile tests whose caller-supplied scale ordinal can drift
  from the active terminal contract;
- erasure of the last concrete storage-envelope failure; and
- unconditional unsupported-TIFF advice after capacity or execution failures.

## Milestones And Checks

### T0 — Authority cut and profile correction

- Add the storage-owned geometry authority and route importer generation to it.
- Derive and update every representative DS boundary.
- Add focused storage and importer regressions for the multi-timepoint case.

Checks:

- `cargo test -p mirante4d-storage`
- `cargo test -p mirante4d-import-pipeline`

### T1 — Failure truth

- Preserve the final typed profile rejection.
- Correct the parent-directory multipage explanation.
- Replace contradictory import-window guidance and cover the projection in UI
  tests.

Checks:

- focused application, app, and UI tests;
- `cargo xtask docs-check`.

### T2 — Repository verification

- Run formatting and the proportional import/storage checks.
- Run the repository's required PR gate after focused failures are resolved.

Checks:

- `cargo fmt --all -- --check`
- `cargo xtask verify-pr rust`

### T3 — Local preprocessing and validation

- Reinspect the exact intended source directory with the corrected binary.
- Record explicit spatial and temporal calibration outside the repository.
- Select the first supported profile, compute exact memory/free-space needs,
  and confirm the destination is absent.
- Run the normal bounded importer to the requested output parent, monitor
  progress and cancellation responsiveness, and retain its checkpoint on an
  interrupted run.
- Confirm atomic publication, open the published package through the strict
  catalog, and report its public-safe shape/scale/profile facts and validation
  result without committing private identities or paths.

## Completion Standard

This package is complete only when the authority cut, recalibrated envelopes,
truthful diagnostics, focused checks, required repository gate, create-only
local import, and post-publication validation have all succeeded. If explicit
scientific calibration cannot be recovered, implementation may complete but
preprocessing remains blocked; inventing calibration is not an acceptable way
to claim completion.
