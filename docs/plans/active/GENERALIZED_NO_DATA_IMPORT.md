# Generalized No-Data Import Plan

- Status: COMPLETE — SPATIAL RECONSTRUCTION IMPLEMENTED AND AUTOMATED-VERIFIED
- Planning requested by owner: 2026-08-01
- Implementation authorized by owner: 2026-08-01
- Initial implementation completed: 2026-08-01
- Spatial-reconstruction correction authorized by owner: 2026-08-01
- Spatial-reconstruction correction completed: 2026-08-01
- Last reviewed: 2026-08-01

This is the implementation and handoff plan for replacing the uint8-only
manual sentinel importer with one typed, first-volume-derived no-data policy
for every admitted TIFF intensity dtype.

## Outcome

The preprocessing review exposes two independent choices:

1. an optional no-data value rule, selected as automatic detection for every
   admitted dtype or manual entry for uint8 only; and
2. an optional `Hide constant Z planes` rule.

Automatic detection examines only the first logical source volume and derives
one spatial mask from exact-value connectivity. That fixed geometry, rather
than global value equality, applies to every channel and timepoint.
Constant-plane detection records only the Z indices of exactly constant planes
in that same volume and applies those indices to every channel and timepoint.
Later timepoints are neither inspected nor used to validate either assumption.

## Exact Product Semantics

- The automatic block edge is fixed at five voxels. A candidate exists when
  all 125 stored samples in one `5 x 5 x 5` block have exactly the same typed
  value.
- The first candidate in deterministic Z/Y/X traversal resolves the sole
  dataset sentinel. A complete second pass forms the exact-sentinel binary
  field, finds every matching `5 x 5 x 5` seed, and reconstructs through
  face-connected exact-sentinel voxels. The automatic mask is exactly the
  union of six-connected components containing at least one seed.
- Isolated or disconnected source voxels numerically equal to the detected
  sentinel remain valid. The reconstructed first-volume spatial mask is reused
  unchanged for every channel and timepoint; later sample values do not alter
  it.
- There is no statistical tolerance, component-size threshold, ordinary
  morphological opening, histogram guess, or machine-learning model.
- A source with no such block continues normally without value-derived
  invalidity. This is an ordinary successful result, not a warning or error.
- Float equality is bound to the finite stored float32 value. Detection and
  recipes preserve its exact bits rather than round-tripping through editable
  decimal text.
- A Z plane is constant when every stored sample equals its first sample.
  Numerical variance is not calculated.
- A detected constant plane invalidates strictly that source Z plane. It does
  not trigger the sentinel rule's one-voxel invalid dilation in Z, Y, or X.
- Manual value-derived invalidity retains exact dataset-wide equality.
  Automatic value-derived invalidity begins from the reconstructed spatial
  mask. Both then retain the existing in-bounds one-voxel guarded dilation.
  Plane-derived invalidity and value-derived invalidity remain separately
  classified until their intended morphology is complete.
- Pyramid values average only valid contributing samples. A coarse sample is
  invalid from the constant-plane rule only when its complete contributing
  source-Z interval is hidden. A thin hidden plane may therefore disappear
  below the resolution of a coarser level without contributing its fill value.
- If neither enabled rule finds invalid source samples, the import retains the
  ordinary all-valid representation and allocates no validity payload.
- Existing complete packages are not reinterpreted. Incomplete predecessor
  checkpoints do not migrate into the new algorithm.

## Invariants

- Source TIFFs remain read-only and retain all generation/currentness checks.
- Detection is bounded, cancellable, and restricted to the first volume.
- Import memory, descriptor, queue, checkpoint, and publication ceilings stay
  explicit and finite.
- Invalid values are canonicalized according to their dtype; validity, not a
  display intensity comparison, remains the scientific no-data authority.
- Derived values numerically equal to the source sentinel remain valid.
- Publication remains staged, validated, create-only, and atomic.
- The active TIFF dtype set remains uint8, uint16, and finite float32. This
  package does not add float16 source admission.
- No private source path, dataset identity, calibration, or detected value is
  committed as repository evidence.

## Authority Cutover

### Corrective automatic-mask cutover

The initial implementation made the resolved automatic value itself the
dataset-wide classification authority. That predecessor is deleted. Automatic
mode now resolves an immutable row-packed first-volume spatial mask, its exact
digest, and its masked-voxel count. Base production and scientific-content
production consult spatial membership only; they never compare later source
samples with the detected value. Manual uint8 remains the only global
value-equality mode.

The first pass retains the deterministic rolling uniform-block detector. When
it resolves a value, a complete second first-volume pass builds exact-value and
seed facts outside independently hidden constant planes. A bounded
six-neighbour reconstruction visits only exact-value voxels reachable from a
seed. The packed result remains CPU-ledger-accounted for the import lifetime,
binds the exact plan/checkpoint digest, and is recorded by digest and count in
the package recipe. An automatic no-match allocates no retained mask.

The conservative plan admits the complete reconstruction state and bounded
frontier before source work. Insufficient configured working memory remains a
typed preflight failure rather than an unaccounted allocation or alternate
algorithm. Existing complete packages remain unchanged and predecessor
incomplete checkpoints are rejected by the new algorithm identity.

Risks checked by this correction are accidental equal-valued data holes,
diagonal-only attachment, disconnected seeded regions, thin face-connected
extensions, chunk/LOD boundaries, dataset-wide mask reuse, cancellation,
source mutation, retained-mask accounting, and no-match all-valid output.
Independent generated-volume tests must distinguish the reconstructed mask
from both global equality and arbitrary component-size filtering. Focused
pipeline/application/UI/tooling checks, documentation synchronization, the
bounded native import scenario, and the public PR gate close the correction.

### Request and resolution

Replace `Option<U8Sentinel>` and UI-owned `Option<u8>` with one typed request:

- value rule disabled, automatic, or manual uint8; and
- constant-Z-plane hiding enabled or disabled.

The import pipeline is the sole resolver. Before checkpoint planning it scans
the first volume when required and produces one immutable resolved policy with
an optional typed exact value and bounded constant-plane ranges. The plan
digest, checkpoint binding, package recipe, base production, scientific
content address, and pyramid production consume that same object.

### Detection

Use one streaming discovery pass over source-native first-volume planes. Exact
plane constancy is accumulated while a rolling equality-run detector
identifies a uniform five-voxel cube in linear voxel work and `O(Y x X)` state.
When constant-plane hiding is enabled, fully constant planes are classified
first and excluded from value inference so a run of empty planes cannot
impersonate the separate dataset sentinel rule.

The detector may stop value searching after its first candidate, but it still
finishes the first volume when the complete constant-plane set is requested.
When a value resolves, a complete second pass marks exact-value membership and
every matching five-cube seed, then performs face-six connected reconstruction.
The compact fixed mask is retained for production. Cancellation and source-
generation checks use the existing import authority.

### Typed validity production

Delete the uint8-only guarded-selector authority. One dtype-dispatched
implementation handles uint8, uint16, and float32 while sharing geometry,
validity, and reduction logic:

- automatic base classification applies fixed spatial-mask membership, while
  manual uint8 applies exact source-value equality; each applies guarded
  dilation and exact plane exclusion as distinct operations before their
  validity union;
- coarse values use valid-only factor-two means with the existing integer
  rounding rule and finite float32 arithmetic;
- sentinel support retains guarded dilation at every level;
- constant-plane support is derived geometrically without dilation; and
- final invalid integer and float values use the existing canonical zero
  representation.

No renderer-side sentinel comparison or alternate all-in-memory producer is
introduced.

### Recipe and checkpoint

Replace the uint8-specific operation identifier and parameters with one
general typed no-data recipe version 2 that records the request, resolved typed
value or no-match result, mask connectivity/encoding/digest/count, constant-
plane ranges, block edge, equality rule, morphology, reduction, support,
rounding, and canonicalization. The current plan digest binds the reconstructed
mask and new algorithm identifier so resume cannot mix predecessor and current
semantics.

### Application and UI

The application snapshot owns the typed review draft. Egui renders the two
independent checkboxes and the value-rule choice, enables manual entry only
for uint8, and describes first-volume/dataset-wide scope. Start remains
available for automatic detection because no-match is successful. Progress
reports the real detection phase rather than freezing at base production.

Product automation uses the same request and exposes the resolved detection
facts needed to prove that time advances into normal base production.

## Verification

Focused independent tests must cover:

- brute-force agreement for the rolling `5 x 5 x 5` detector across all three
  dtypes, including no match and exact float-bit behavior;
- detection from only the first volume and application to later timepoints;
- exact constant-plane discovery, no-op discovery, and no neighboring-plane
  dilation;
- value-only, plane-only, combined, and all-valid base masks;
- valid-only values and support at every generated LOD, odd tails, chunk
  boundaries, and constant-plane ranges narrower and wider than one coarse
  footprint;
- canonical invalid values, derived-sentinel non-reclassification, transforms,
  recipe facts, plan/checkpoint binding, cancellation, resume, source
  nonmutation, and create-only publication;
- uint8 manual UI behavior plus automatic uint8/uint16/float32 review behavior;
  and
- one bounded native generated-source import that reaches publication/open
  with independently checked value and plane masks.

Run focused package tests while iterating, then formatting, documentation and
verification synchronization, the affected import/product scenario, and the
public PR gate. A normal application exercise remains required before claiming
product validation; automated checks alone establish only implementation and
automated verification.

## Implementation Record

Completed on 2026-08-01:

- the product request, application draft, egui review, and automation schema
  now expose automatic typed value detection, manual uint8 entry, and
  independent constant-Z-plane hiding;
- one bounded first-volume resolver produces the immutable typed value,
  no-match result, and exact plane set used by planning, checkpoint binding,
  base production, scientific-content production, pyramid generation, and the
  published recipe;
- uint8, uint16, and finite float32 use one validity producer with separate
  value dilation and strictly plane-local exclusion, followed by valid-only
  coarse reduction and geometric hidden-plane support;
- no-match/no-plane imports retain the ordinary all-valid representation and
  allocate no validity payload; and
- predecessor checkpoint identities are excluded by the new plan and recipe
  contract rather than migrated ambiguously.

The corrective spatial-mask cutover is also complete:

- automatic mode keeps the first-candidate typed value only as a detection and
  provenance fact; production no longer performs dataset-wide equality against
  it;
- a complete second first-volume scan marks every same-value `5 x 5 x 5` seed,
  then face-six reconstruction selects exactly the connected components that
  contain a seed;
- the immutable row-packed mask is reused unchanged for all channels and
  timepoints, while disconnected equal-valued data remains valid and manual
  uint8 remains exact dataset-wide equality;
- the reconstruction state, compact 32-bit or wide 64-bit bounded frontier,
  and retained mask are admitted and charged by the CPU memory authority;
- mask shape, digest, and voxel count bind the exact plan/checkpoint and the
  version-2 recipe; and
- UI copy now states the distinct automatic spatial and manual equality
  semantics.

Automated verification passed:

- `cargo clippy -p mirante4d-import-pipeline --all-targets -- -D warnings`;
- `cargo test -p mirante4d-import-pipeline` (131 tests across unit and
  integration binaries);
- `cargo test -p mirante4d-application` (122 tests);
- `cargo test -p mirante4d-ui-egui` (39 tests);
- `cargo xtask product-validate import_preprocessing`, using the bounded public
  generated TIFF fixture, including cancel/resume, atomic publication, direct
  open, source-byte preservation, rendered output, and the real
  `no-data-detection` stage; and
- `cargo xtask verify-pr`, including formatting, registries, fixtures,
  architecture, documentation, dependencies, exact lint policy, and all 1,330
  unit, contract, and UI lane tests with no retries.

The generated-source integration suite independently proves typed automatic
detection, automatic no-match, first-timepoint-only inference, fixed-mask reuse
across time, thin face-connected inclusion, later disconnected seeded-
component inclusion, diagonal/equal-valued dust exclusion, and strict plane
behavior. The native product scenario proves the ordinary end-to-end import
route and stage integration; it is not represented as owner visual validation
of the new masking controls.

## Out Of Scope

- Inspecting or validating later timepoints.
- Per-timepoint, per-channel, tolerant, statistical, or learned sentinels.
- Manual uint16 or float32 sentinel entry.
- Float16 TIFF admission.
- Renderer sampling-policy changes, including SmoothLinear behavior beside
  invalid taps.
- Reprocessing or overwriting an existing private package.
