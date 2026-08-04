# Import, Preprocessing, And Storage Testing Refactor

- Status: IMPLEMENTED — PORTABLE VERIFIED; LOCAL PERFORMANCE/PRODUCT EVIDENCE AS TRIGGERED
- Planning requested by owner: 2026-08-03
- Target approved by owner: 2026-08-03
- Last reviewed: 2026-08-03
- Scope: TIFF-source conformance, preprocessing correctness, package-format
  conformance, cancellation/recovery faults, import application/UI handoff,
  and changed-boundary performance checks.

This is a focused testing follow-up to the completed import, preprocessing,
and storage cutovers. The implemented product architecture remains accepted.
The refactor strengthens a mostly good suite; it does not reopen the storage
format, importer design, no-data policy, resource model, or publication path.

The repository cutover is implemented. Existing ignored diagnostics,
`format-lifecycle`, private performance tools, and mapped product validation
remain separate changed-boundary evidence; the implementation does not turn
an unexecuted local check into a correctness, durability, or performance
claim.

## Outcome

The refactor must close the following gaps without building a large new
verification framework:

1. connect the promoted independent TIFF facts to production inspection,
   decode, preprocessing, and published scientific output;
2. add independent positive coverage for every admitted TIFF compression
   family instead of relying only on TIFFs written by the production Rust
   dependency's encoder;
3. expose each promoted package and mutation as an individually diagnosable
   case and check its intended rejection reason, not only a broad stage;
4. replace tests that inspect Rust source text with observable behavior;
5. cover cancellation, resumability, capacity failure, and crash recovery at
   the important end-to-end stage boundaries;
6. add small bounded hostile-input/property coverage for the parsers that
   accept TIFF, package metadata, paths, manifests, and shard indexes;
7. close the missing normal application/UI setup and imported-publication
   handoff cases; and
8. make the existing local performance and scale checks explicit obligations
   whenever their owned performance boundary changes.

## Contracts That Must Remain

- Independent scientific facts or an independent reader remain required;
  production writer/reader agreement alone is not proof.
- Source microscopy data is never modified.
- Valid zero, invalid/no-data, and missing remain distinct.
- Import remains bounded, cancellable, restartable, deterministic, and
  create-only atomic at publication.
- Ordinary package open remains structural and lazy; this plan does not add a
  mandatory whole-package scan.
- Performance checks cannot weaken scientific output, resource accounting,
  source safety, or publication behavior.
- Public fixtures remain small, synthetic, licensed, and free of private
  paths or dataset facts.

## Required Changes

### 1. Independent TIFF-to-package conformance

Add one production conformance suite that consumes the promoted source
manifest, independent-reader report, and expected facts directly. For each
accepted source/layout it must check the independently stated shape, dtype,
ordering, calibration, canonical S0 values, and source preservation. A small
representative set must then complete preprocessing and compare published
values, validity, transforms, and scientific identity against facts derived
without production importer or storage helpers.

The promoted negative source recipes must also be exercised through the
production inspector/importer with an exact expected rejection class. Fixture
validation by itself is not importer conformance.

The current broadly named promoted-source test must either gain those
independent assertions or be renamed to the narrower publication/statistics
contract it actually proves.

### 2. Independent TIFF breadth

Extend the small source corpus with independently produced and independently
read positive cases for uncompressed, LZW, current Deflate, old Deflate, and
PackBits. Include only distinct supported container boundaries that matter to
the product, such as tiled/striped, BigTIFF, and big-endian uint16. Do not
create a combinatorial matrix or large microscopy corpus.

### 3. Clear conformance cases and behavioral oracles

Split the three positive target packages, fifteen promoted mutations, and
writer cases into individually reported tests or an equivalent runner that
continues through every case and reports all failures. Each mutation must
check its precise production rejection contract where one is declared.

Delete the tests that read `package_write.rs` or `package_science.rs` as text
and count call spellings. Preserve their real intent through observable sync,
object-read, codec, publication-currentness, and fault-injection evidence.
Internal counters may remain resource assertions, but important integration
tests must lead with scientific or user-visible outcomes.

### 4. Cancellation, capacity, and recovery

Add a compact end-to-end cancellation matrix over inspection, source ingest,
no-data resolution when present, base/pyramid production, shard publication,
staged validation, and the pre-rename commit boundary. Every cancellable
prepublication point must leave no complete destination, preserve the source,
and either resume to the same package/scientific identity or identify why no
checkpoint is expected.

Add deterministic full-pipeline fault cases for storage-full and permission
failure before publication. At least one registered local subprocess test
must terminate an import after a durable checkpoint boundary and prove exact
recovery. These checks establish the named logical/process boundaries only;
they do not claim arbitrary-filesystem or power-loss durability.

### 5. Bounded hostile-input coverage

Add small deterministic property tests around pure path, TIFF-preflight,
metadata/manifest, and shard-index parsing. Checked arithmetic, size limits,
canonical encoding, and fail-closed rejection are the useful properties.
Any discovered failure becomes a minimized permanent regression case. A
continuous fuzzing service, large random corpus, and new hosted infrastructure
are outside scope.

### 6. Application and UI handoff

Add egui harness coverage for the setup rows and source-kind/path command
wiring, not only review and recovery states. Add a bounded display-free app
test that follows setup, inspection, review, start, cancellation/resume,
publication, and normal open using the same typed commands as the UI.

The three ignored imported-publication/project-close and publication-drift
cases must receive a fast routine contract where practical. Any genuinely
expensive remainder must stay explicitly registered as a required
changed-boundary local check, rather than being treated as general positive
coverage. The mapped `import_preprocessing` scenario remains the product
boundary; it is not replaced by headless tests.

### 7. Proportionate target-format breadth

Keep the existing independent target corpus and add only cases needed to
cover a currently unrepresented semantic contract, especially integer
explicit validity and portable records. Do not multiply fixtures across every
dtype, geometry, and storage permutation when a pure/unit oracle already
proves the same rule.

### 8. Performance and scale obligations

Performance checks remain local and opt-in. Import scheduling or throughput
changes run the temporal decode-ahead benchmark and the applicable T2/T5
tool. Storage read, caching, manifest, codec, or fan-out changes run the
applicable 1,024-chunk/brick diagnostics and `format-lifecycle` scalability
case. Normal application loading or import-workflow performance changes also
run the mapped product scenario on the relevant workload.

Every run must keep its existing workload, hardware, cache condition, sample
method, and threshold explicit. The unpinned current-format T5 tool remains a
diagnostic and cannot create a qualification claim until separately reviewed
and pinned.

## Implementation Order

1. Wire the independent source facts into production conformance and add the
   missing compressed fixtures.
2. Split the large conformance loops and replace source-text inspection.
3. Add the bounded cancellation/fault/property cases.
4. Close the application/UI handoff and register the changed-boundary
   performance obligations.

Each step should remain reviewable on its own. Fixture or registry changes
must update their existing manifests and generated metadata in the same
change.

## Implementation Record

The repository cutover completed on 2026-08-03. The promoted `source-tiff-v2`
archive now contains independently produced/read uncompressed, LZW, current Deflate,
old Deflate, PackBits, tiled/striped, BigTIFF, and big-endian uint16 facts.
Production conformance binds those facts through inspection, decode,
preprocessing, publication, transforms, validity, and scientific identity.
Positive target packages and mutation recipes have case-level production
classification; source-text call-count tests are replaced by observable
read/sync/fault behavior; cancellation, storage-full, permission, checkpoint
crash recovery, and bounded parser properties have permanent cases; and typed
application/UI setup through imported open-ready publication is covered.

The fixture validator, independent fact oracle, production source/target
conformance, import pipeline, storage, application, UI, and registry checks
form the portable closeout. `format-lifecycle`, T2/T5, 1,024-scale
diagnostics, and mapped `import_preprocessing` still run when their performance
or product boundary changes. They are not fabricated as executed evidence for
this refactor.

The final portable working-tree closeout passed source, target, project, VM,
architecture, dependency, workflow, and documentation policy. Clippy, exact
discovery ownership, and all 1,453 routine unit/contract/UI cases passed with
zero retries. Three substantial new import cases have exact-name Nextest
ceilings based on measured 14-second, 28-second, and 48-second local runs; no
crate-wide or lane-wide timeout was weakened. The 42 registered ignored cases
were audited but not executed.

The first standard public-runner activation supplied additional timing
evidence. The typed cancel/resume/publish/open workflow exceeded its former
20-second internal publication deadline under concurrent load, and the
control-plus-two-fault/resume finalization workflow reached its former
20-second runner ceiling while still making bounded progress. Each exact test
now has a registered 60-second Nextest ceiling; the typed workflow has a
45-second internal progress deadline for a more specific failure. Retries and
all package/lane defaults remain unchanged. A direct concurrent local run
passed the two cases in 13.046 and 12.743 seconds respectively.

With the ownership and deadline correction applied, the clean-revision
portable closeout passed all 1,483 routine unit, contract, and UI cases with
zero retries and reconciled all 46 exact non-routine cases without executing
them in the portable lane.

## Completion Standard

This plan is complete only when:

- production import is checked against independent source and scientific
  facts through publication;
- every admitted TIFF compression family has independent positive evidence;
- promoted positive and negative cases are individually diagnosable;
- the two source-text tests are gone and their behavior is still checked;
- the named cancellation, storage-full, permission, and process-recovery
  boundaries pass without source mutation or partial publication;
- setup/UI and imported-publication handoff coverage has an explicit routine
  or changed-boundary owner;
- performance-sensitive changes have explicit local check ownership; and
- focused suites, `cargo xtask verify-pr`, relevant changed-boundary lanes,
  documentation checks, and any required normal-product validation pass with
  important skips reported.

No increase in test count, fixture count, or verification machinery is a
completion criterion by itself.
