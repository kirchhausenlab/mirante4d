# Testing And Evidence

Last updated: 2026-07-18

## Claim Language

- **Implemented:** the change exists.
- **Automated-verified:** the named automated checks passed for the stated
  revision.
- **Product-validated:** the normal native application ran on a real mapped
  display and the affected workflow was exercised on the relevant dataset and
  hardware, with logs and evidence inspected.

Do not collapse these claims. Unit tests, smoke tests, virtual/no-display
automation, snapshots, benchmarks, preflight runs, and render readbacks are
supporting evidence, not product validation.

Tests should be proportionate to the change: focused unit and contract tests,
a few useful integrations, and understandable product-level checks.

## Public Checks

The public test surface has six independently selectable leaves:

```bash
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
cargo xtask verify-leaf doctest
```

Run both public groups locally with:

```bash
cargo xtask verify-pr
```

`verify-pr policy` and `verify-pr rust` select one group when focused
feedback is useful. The Rust group shares one test-binary build across the
unit, contract, and UI leaves before running doctests. Tests are never
automatically retried.

The generated selectors and Nextest configuration must match their registry:

```bash
cargo xtask verification-sync --check
```

Documentation alone can be checked with:

```bash
cargo xtask docs-check
```

The protected repository requires exactly `PR / policy` and `PR / rust`.
Matching non-required `Main / policy` and `Main / rust` jobs run on protected
main. Hosted verification uses standard public runners with a hard `$0`
budget, no public self-hosted workstation, no private data, no automatic
retry, and no cache or artifact storage.

Test discovery and exact case ownership live in the verification registry and
test source. This document does not duplicate their inventories.

## Change-Specific Local Checks

Run these only when their boundary changes. They are not a recurring release
ritual, and accepted foundation evidence does not need to be reproduced for
unrelated work.

### GPU And Runtime

Renderer, GPU-resource, or dataset-runtime changes use the trusted local
Vulkan check on the designated clean workstation:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu
```

This is component evidence. It does not replace opening the viewer when the
visible product path changed.

The viewer-overhaul GPU diagnostics are intentionally focused and ignored by
ordinary public test selection:

```bash
cargo test --release -p mirante4d-render-wgpu \
  resident_volume_gpu_timing -- --ignored --nocapture
cargo test --release -p mirante4d-render-wgpu \
  payload_buffer_vs_texture_gpu_timing -- --ignored --nocapture
cargo test --release -p mirante4d-render-wgpu --lib \
  -- --ignored --nocapture
```

`resident_volume_gpu_timing` uses timestamp queries after an excluded warmup
and reports upload and volume-pass distributions separately. It is a direct-
renderer diagnostic, not an end-to-end viewer result. The representation test
alternates a storage buffer and filterable `R32Float` texture across two
warmups and seven trials for one `float32`, all-valid, voxel-exact fetch-only
case. It exists only to justify the selected storage-buffer representation;
it makes no dtype, validity, interpolation, streaming, presentation, or
product-performance claim.

The full ignored renderer command additionally exercises multichannel order,
MIP/DVR/ISO validity and picks, affine/perspective and SmoothLinear behavior,
long rays, and multi-presentation residency on the trusted Vulkan adapter. It
is correctness/component evidence; only the two named diagnostics emit timing
observations.

The EP-00 predecessor-development protocol is frozen through one external,
owner-bound v5 profile plus strict workload, v5 interaction-script, and
independent-oracle bundles. Individual automation input scripts remain schema
5, while application automation reports use the schema-6 hard cut. The viewer
interaction-script bundle remains schema 5, so this report change does not
rotate the existing EP-01 trace-geometry authority. Build the release runner
through its canonical builder, then invoke that exact executable for preflight
and execution:

```bash
cargo xtask viewer-performance-build-runner
target/release/xtask viewer-performance-preflight --help
target/release/xtask viewer-performance-run --help
```

Those inputs remain outside the repository because they bind private package
and source locations. The committed profile-contract digest, strict schemas,
clean-revision check, hardware/display/storage observations, source inventory,
bundle commitments, and sanitized receipt prevent a different workload or
local edit from inheriting the result. Raw evidence stays private; public
reporting may name only path-free counters, gates, and reason codes.
Both preflight and execution require the invoking release `xtask` to carry the
exact clean profile-bound repository revision, compiler, and standard build
provenance. A stale cached preflight executable fails before its result can be
used to authorize a measurement. The canonical builder explicitly binds every
release profile dimension, including assertions, overflow, incremental, LTO,
panic, codegen units, rpath, stripping, and the compiler host target.
Execution builds and runs conformance from a detached clean worktree of the
profile revision. The live repository, detached source, and app digest are
rechecked before every role; drift stops before the next process launch.

The predecessor run is exactly three ordered samples of all ten frozen
scenarios. Every scenario has one fresh instrumented process and one fresh
matched instrumentation-control process, for exactly 60 role attempts. Role
order is balanced from sample and scenario parity. There are zero automatic
retries, dropped observations, substitutions, or post-hoc filters.

Owner-declared product acceptance observations use the v5
`observe_gate_batch` automation command. All observations at one contiguous
acceptance checkpoint are sampled concurrently from one shared origin and
retain independent typed, ordered outcomes. The checkpoint wall bound is the
maximum applicable deadline,
not the sum of serial gate timers. A gate miss is retained as a product
failure and does not suppress later diagnostic checkpoints; it is never
converted into an infrastructure failure or a pass. The same split applies to
executable numerical conformance: an exact parsed marker bound to the
canonical oracle may report a fidelity miss and remain an authoritative
product outcome, while a missing marker, bad commitment, unexpected process
exit, or malformed evidence is fatal. Ordinary `wait_for`
commands remain fatal prerequisites and may not be used for an acceptance miss
that must preserve later evidence. Typed gate misses continue through the
remaining role checkpoints and complete population. The first fatal setup,
process, hard-safety, or evidence-integrity failure instead stops the launch
immediately and preserves the exact partial lineage; later roles are not run
after the evidence envelope has become unusable.

An explicitly null current-presentation generation, displayed scale, or
settlement milestone and a present, reconciled, exactly empty
admitted-generation latency population are authoritative product failures;
they mean the corresponding presentation state was never reached. An absent
field, malformed value, missing ring, or unreconciled population remains an
evidence-integrity failure. The optional hidden staging-renderer fact follows
the same typed boundary: explicit null authoritatively contributes zero to the
per-target structural sum, while an omitted or malformed fact is invalid.

Every checkpoint acceptance batch resolves before its phase-end diagnostic.
It is followed by one bounded, nonblocking
`await_active_view_gpu_timing` command for the oracle-bound active target and
pass immediately before every GPU-gated end diagnostic. A valid schema-6
report has exactly one of two typed outcomes for that await. When an exact
current presented-interval ticket exists, the command freezes its execution
identity, waits for completion of that same record even if normal rendering
installs a newer execution, and publishes the completed identity and timing.
When no identity exists, exact unavailable evidence is permitted only when it
is bound to the unique failed, timed-out
`coordinated_presentation_settled` row in the immediately preceding gate batch
and the current-presentation generation is explicitly noncurrent for the
awaited display generation. That unavailable outcome completes immediately
after the already-terminal gate observation, possibly with zero additional
wait, and remains a product failure; it supplies no numeric GPU sample or
timing pass. Both variants are published as a distinct qualification
checkpoint in the adjacent diagnostic during the same UI callback, and the
normal current-execution facts remain unmodified.

A captured but still-pending exact interval continues to wait only within the
fixed five-second ceiling. A captured interval that is evicted, a malformed or
unlinked unavailable authority, or an absent candidate without that exact
adjacent terminal settlement authority remains an evidence-integrity failure.
A stale, execution-only, interval-only, adapter-global, or synchronous
readback cannot satisfy the available variant, and plain null timing facts
cannot satisfy the unavailable variant.
The removed diagnostic-before-gate ordering and a callback boundary between
readiness and capture are rejected. Phase diagnostics also bind the canonical
current time index and application snapshot currentness.
Project revision and undo/history deltas are required only for bound projects;
unbound provisional-viewing roles instead require those project-only fields to
remain explicit nulls while retaining the same exact finalized-view-commit
gate.

Instrumentation overhead is evaluated once per scenario over its complete,
balanced three-pair development population. Each pair retains the raw
instrumented app wall clock, matched control wall clock, raw process CPU
clocks, and an observation-only basis-point result. The runner subtracts from
the instrumented wall clock only the checked sum of integer `waited_ns` values
from structurally completed qualification-only
`await_active_view_gpu_timing` commands, for either the available or exact
unavailable variant. Every subtracted event must have the exact schema-6
shape, agree with its `waited_ms` value and fixed five-second bound, and match
the adjacent diagnostic checkpoint: the frozen execution identity for an
available ticket, or the exact preceding terminal-gate authority for an
unavailable outcome. A valid zero wait is retained and subtracts zero. The
adjusted wall sum must also reconcile exactly as raw wall minus that wait sum.
Process CPU time is never adjusted. Automation-command completion means that
the evidence was collected, not that an unavailable product gate passed. The
200-basis-point gate is applied to the summed adjusted-wall and raw-CPU
operands for each exact three-pair scenario population, not to an individual
pair. There are no retries, dropped pairs, outlier filters, or negative-
overhead credits. Missing, inconsistent, overflowed, incomplete, or reordered
operands make the evidence invalid rather than making the gate pass.

Claim-bearing timing histories retain 4,096 allocation-free samples. This is
large enough for the frozen maximum 480-sample interaction plus the
30-second verification gate and bounded polling headroom; a phase that still
overwrites that ring remains an evidence-integrity failure.

Deadline values are exact validated profile, oracle, or protocol operands
rather than private script policy. Resident gates use the frozen current-presentation bound plus
one polling grace; cold, nonresident, and verification classes retain short
explicit ceilings; and all import-primary observations share the import-
primary origin. Every non-import fatal prerequisite is fixed by the validated
profile or protocol and is at most 30 seconds. No private script may author a
multi-minute viewer prerequisite. The runner accepts no caller-authored
role-process timeout; it
derives the exact process bound from the validated action, prerequisite, and
concurrent-batch schedule plus bounded launch and closeout grace. IP includes
its import-primary wall bound exactly once. A script/profile mismatch fails
preflight. The predecessor v4 baseline launch is preserved as interrupted
evidence but never reused: owner observation rejected its serialized long
waits after they left otherwise active product processes visibly static for
minutes. It produced no complete sample and supports no performance claim.

The ordinary product-validation scenarios and private T5 runner use the same
schema-5 automation input and schema-6 automation report envelope even when a
script needs no acceptance batch. Earlier input or report versions are
rejected; there is no compatibility reader.

Qualification resource ceilings are not automation abort conditions. The app
records their observed maxima and the runner evaluates them on the product-gate
axis after the role closes. Each v5 script instead carries a distinct required
`hard_safety_limits` object: its total-CPU cap is the configured CPU-ledger
capacity, its decoded-residency and upload-staging caps are that total
capacity, and its queued-request cap is exactly twice the qualification queue
ceiling. Crossing one of those wider fail-safe bounds is an evidence-integrity
failure because the bounded attempt must stop; the removed `limits` spelling is
not accepted.

The EP-00 private raw report and sanitized development receipt each use their
schema-5 hard cut and separate two axes. Evidence is complete only when
bindings and executable conformance pass, all 60 role attempts and their
schema-6 automation reports close, every required checkpoint and timing/
resource population is present and reconciled, source currentness and cleanup
hold where applicable, instrumentation-control overhead is valid, and the
repository and release executable remain unchanged. Product status is the
ordered population of typed gate outcomes and may be failed for the
predecessor. Missing or malformed reports, commands, gate rows, checkpoints,
clocks, controls, or operands; derived role-process deadline expiry or
abnormal process exit; binding/currentness drift; or any retry remains an
evidence failure. Native command success means the evidence envelope is
complete, not that the predecessor passed its product gates.

This protocol establishes EP-00 baseline and later work-package evidence; it
is not yet the final EP-07 qualification protocol. Before a performance pass
can be claimed, the final exact sampling rule must move into this document and
the clean atomic successor must pass the owner-bound workload, fidelity floor,
all absolute gates, and externally inspected real-display exercise.
Development observations and the small fixture remain diagnostic only.

### Format And Storage

Validate the small independent target fixtures with:

```bash
python3 tools/target-fixtures/t1/validate.py \
  --manifest fixtures/target/manifest.json --self-test
```

Changes to the native package format, storage reader/writer, identities, or
independent conformance boundary use:

```bash
cargo xtask verify-local format-lifecycle
```

These checks use bounded repository fixtures. They make no stable-format,
generic OME-Zarr, huge-dataset, or product-performance claim.

Storage-reader changes can also use the release-only scale diagnostics:

```bash
cargo test --release -p mirante4d-storage \
  aligned_verified_1024_brick_storage_scale_diagnostic \
  -- --ignored --nocapture
cargo test --release -p mirante4d-storage \
  aligned_verified_1024_brick_eight_consumer_storage_scale_diagnostic \
  -- --ignored --nocapture
cargo test --release -p mirante4d-storage \
  three_dimensional_1024_unique_chunk_io_scale_diagnostic \
  -- --ignored --nocapture
```

The first two deliberately repeat the normal u16 64-cubed fixture resource
1,024 times, sequentially or across eight consumers. They exercise the normal
secure cohort/currentness/range/codec/direct-sink path but are structural
surrogates, not a large unique-dataset workload. Their output separates direct
writable-span bytes from post-decode copies and reports cohort membership, so
an apparent syscall gain cannot hide serialized codec work or another full
payload copy. The third creates 1,024 unique inner chunks in 16 indexed shards
and isolates the normal handle/index/range/decode path.
Always report filesystem, cache condition, build, hardware, run count, and the
exact metric when citing their output.

### Project Persistence

Validate the independent project fixture with:

```bash
python3 tools/project-fixtures/validate.py \
  --manifest fixtures/project/manifest.json --self-test
```

Only changes to the qualified project-store durability boundary use the
trusted local lifecycle check:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local project-store-lifecycle
```

Do not rerun its exhaustive fault or power-cut coverage for unrelated changes.
The fixture validator checks canonical examples and recovery classification;
it does not by itself establish filesystem durability or product save/open
behavior.

### Product Scenarios

Storage-source and verification changes use the bounded viewer scenario:

```bash
cargo xtask product-validate target_source_verification
```

Import/preprocessing product changes use the generated public full-strip TIFF
scenario:

```bash
cargo xtask product-validate import_preprocessing
```

It drives the normal native setup/review/start route, waits for a durable base
prefix, cancels, re-inspects and resumes, waits for verified publication and
the destination-bound capability to become directly open-ready, then renders
and navigates the imported package. The absence of an ordinary verifier start
is observed by a successful-spawn counter. Failed runs, accepted progress,
cancelled runs, and accepted successes are also measured from the import-start
baseline and must all remain zero. The storage-issued transfer receipt binds
the closed inventory/snapshot/inventory contract to separate strict object-open
deltas: matching inventory passes, one proof-derived complete snapshot count,
an exactly reconciled total, and zero codec decodes. A storage architecture
test separately forbids exact hashing, scientific validation, and brick reads
in that route. Together these replace the former self-declared zero-work
fields. The scenario also compares the generated source closure before and
after. A passing automated scenario is supporting evidence; a real
mapped-display run on the relevant workload/hardware is still required for
product validation.

The public performance workload runs only in release mode:

```bash
cargo run --release -p xtask -- import-performance-t2 \
  --samples 5 \
  --scratch /qualified/ext4/scratch/mirante4d-t2 \
  --qualification-profile /local/non-repository/hw-2-import-profile.json \
  --cache-condition warm --competing-activity none
```

The successful import receipt is the counter authority consumed by both
performance runners. `sync_calls` and `sync_time_ns` cover canonical-cache and
spool file/directory durability plus one counted staged-filesystem `syncfs`,
every staged-directory `fsync`, and the post-rename parent-directory `fsync`.
The `syncfs` duration can include unrelated dirty writeback on the same
filesystem; an unrelated writeback error fails the import conservatively.
Package creation rejects Linux kernels older than 5.8 because their `syncfs`
result did not reliably report writeback failures. Codec call/time fields
aggregate checkpoint pixel/validity encoding and checkpoint-dependent decoding,
package-construction inner encoding (including packed-index inners), and
staged-validation decoding; the encoded pixel/validity publication boundary
must not add a second inner encode. Package-object reads are actual successful
strict-reader OS opens, split into staged structure, exact, and scientific
phases and reconciled to a checked total. Whole-object reads, range reads,
streamed hashes, and snapshot revalidations count; directory-only metadata
operations do not. The separate scientific-payload read count must be a subset
of the scientific phase total. The report also reconciles the sampled
descriptor peak against the structural bound, the import ledger against
external RSS, and observed checkpoint/stage bytes against preflight.

Qualification additionally requires a clean immutable worktree/executable,
the locally qualified HW-2 ext4 tuple, five independent process sessions,
unchanged source inventory, deterministic exact/scientific identities, and
every recorded runtime/resource gate. The dirty-worktree override is
diagnostic only and cannot produce qualification evidence. Private workload
evidence remains ignored local material.

The qualification profile is a local binding document, not by itself a trust
root or caller-supplied hardware label. It must be a regular JSON file whose
canonical path is outside the repository, must not be a symlink, is capped at
64 KiB, and has a `qualified_root` containing every selected source and
scratch/output path. Use this strict schema,
filling every value from the workstation and storage tuple that the owner has
qualified:

```json
{
  "schema": "mirante4d-import-performance-qualification-profile-2",
  "hardware_class": "HW-2",
  "host": {
    "cpu_model": "exact first model name from /proc/cpuinfo",
    "logical_cpu_count": 1,
    "mem_total_kib": 1
  },
  "scratch": {
    "qualified_root": "/absolute/qualified/ext4/scratch",
    "filesystem_type": "ext4",
    "filesystem_source": "exact findmnt SOURCE value",
    "filesystem_uuid": "exact findmnt UUID value",
    "device_major_minor": "exact findmnt MAJ:MIN value"
  },
  "protocol": {
    "cache_condition": "warm",
    "competing_activity": "none"
  }
}
```

`logical_cpu_count` is the process-visible parallelism (normally the result of
`nproc`); `mem_total_kib` is `/proc/meminfo`'s `MemTotal`. Obtain the storage
fields with `findmnt --noheadings --raw --output
FSTYPE,SOURCE,UUID,MAJ:MIN --target QUALIFIED_ROOT`. The protocol values are
exact owner-bound declarations shared by T2 and T5; they record a stable sample
condition but do not claim to control the operating-system cache or other
processes.

The v2 evidence report records profile, observed-host, and observed-storage
SHA-256 fingerprints plus the filesystem class and fail-closed reason codes.
It does not repeat the raw host tuple and never records the profile path,
qualified root, source-device path, or filesystem UUID. The repository
commit/clean state, profile binding, canonical session storage,
host/storage fingerprints, and parent/worker executable digests must all match
at both run boundaries. Build-time metadata additionally binds the executable
to its Git revision, clean/dirty build state, exact Cargo profile, compiler,
and absence of custom Rust flags or compiler wrappers. Qualification accepts
only a standard `release` build whose embedded revision matches the clean
runtime checkout.

A matching freshly authored profile is still self-attestation. Unless its
opaque SHA-256 is the exact owner-accepted commitment in the repository
qualification authority, it reports either pending acceptance or an owner
digest mismatch and remains diagnostic. One reviewed local HW-2 profile
commitment is now pinned; its raw host, storage, and path tuple remains local.
Qualification still requires every observed boundary to match that exact
commitment and its declared protocol.

The private T5 product protocol has a separate strict release-only runner. Its
configuration must be a nonsymlink regular JSON file outside the repository;
the source and scratch root must also resolve outside the repository and under
the storage root accepted by the shared qualification profile. This synthetic
shape-free example shows the fields, not accepted private facts:

```json
{
  "schema": "mirante4d-private-import-performance-t5-2",
  "workload_id": "t5-0123456789abcdef0123456789abcdef",
  "source": "/absolute/qualified/ext4/scratch/private-source",
  "scratch_root": "/absolute/qualified/ext4/scratch/t5",
  "qualification_profile": "/absolute/local/hw-2-import-profile.json",
  "expected_profile": "DS-3",
  "spacing_zyx_um": [1.0, 1.0, 1.0],
  "time_step_seconds": null,
  "no_data_sentinel": 255,
  "working_memory_bytes": 268435456,
  "primary_timeout_seconds": 1200,
  "cache_condition": "warm",
  "competing_activity": "none",
  "expected": {
    "expected_fact_authority": "mirante4d-t5-source-derived-guarded-sentinel-oracle-1",
    "source_inventory_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
    "reviewed_source_fingerprint_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
    "canonical_source_pixel_bytes": 1,
    "scientific_content_id": "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000",
    "scientific_layer_roots": [
      {
        "logical_layer": 0,
        "digest_sha256": "0000000000000000000000000000000000000000000000000000000000000000"
      }
    ],
    "scale_digest_scheme": "mirante4d-t5-canonical-scale-voxels-1",
    "scales": [
      {
        "image_ordinal": 0,
        "scale_ordinal": 0,
        "digest_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
        "brick_reads": 1,
        "logical_voxels": 1
      }
    ],
    "transforms": [
      {
        "scale_ordinal": 0,
        "scale_zyx": [1.0, 1.0, 1.0],
        "translation_zyx": [0.0, 0.0, 0.0]
      }
    ]
  }
}
```

Replace every placeholder with independently frozen private facts and include
exactly one scale and centered-transform entry for every physical image/scale,
plus one root for every logical scientific layer. DS-3 requires scale ordinals
zero through six; the single entries above show field shape only.
Schema v2 requires an explicit uint8 sentinel and the exact source-derived
guarded-sentinel fact authority. The v1 configuration and opaque commitment
are rejected rather than treated as a compatibility input by the formal
qualification runner.
The source-inventory digest uses the `mirante4d-t5-source-inventory-1` domain
and binds the sorted relative names, lengths, and complete contents of every
source file. It was frozen independently before owner acceptance; candidate
facts and receipts remain only in external private material. The separate
reviewed-source fingerprint is the importer's domain-separated commitment to
the exact accepted layout, dimensions, dtype, relative names, lengths, and
per-file SHA-256 values. Each sample binds that fingerprint and the reviewed
source-byte total to its accepted Start event, operation token, successful
receipt, and primary clock.
The scientific identity and layer roots cover base values, validity, axes, and
calibration. The per-scale digest independently traverses canonical effective
validity and value bytes in logical order; predecessor output is not the
oracle. The independent package traversal also derives the canonical base pixel
byte count from actual base shapes and dtypes; it must equal the frozen value
before serving as the 1.10-times decode denominator. T5 is intentionally frozen
to `cache_condition: "warm"` because its inventory proof streams the source,
and to the public `competing_activity: "none"` declaration. Both must exactly
equal the shared profile protocol.

#### One-Time T5 Source-Fact Freeze

At clean revision `d6478d9`, two independent full source-oracle derivations
enumerated and decoded the accepted source TIFF planes, applied the restored
sentinel contract through all seven LODs, and derived scientific, layer,
scale, and centered-transform facts without importing or reading a candidate
package. They produced byte-identical schema-v2 candidate configurations,
observed unchanged source, stayed within the admitted memory, descriptor, and
two-file scratch bounds, and left no oracle scratch. Those completed runs are
the one-time fact freeze; their private raw receipts and facts remain outside
the repository.

The oracle uses fixed plane/slab rings below 256 MiB, at most two create-new
row-major level files, admitted external scratch space, and bounded
descriptors. The first `SIGINT` or `SIGTERM` requests cooperative cancellation
and permits identity-checked cleanup. A repeated termination signal forces
immediate exit; `SIGKILL` and power loss cannot run process cleanup and may
retain only private external session material.

Routine correctness and performance runs never recompute this oracle. A new
offline oracle audit is required only when explicit sentinel semantics, the
fact schema or authority, or the oracle implementation changes while the
pinned source remains unchanged. Unexpected source-byte drift is rejected by
the audit's inventory preflight before the expensive derivation. Intentionally
accepting a different source is a new two-run fact-freeze and owner-pinning
work item, not an audit of this frozen workload. The audit never updates
expected facts.
The legacy schema-v1 bootstrap is not a compatibility path; after the accepted
schema-v2 commitment is pinned, only schema v2 is admitted and Git history is
the archive of the one-time bootstrap.

Run the expensive audit only after one of those triggers:

```bash
cargo run --release -p xtask -- import-performance-t5-oracle-audit \
  --config /absolute/private/t5-config.json
```

The audit accepts only the pinned v2 configuration, uses bounded temporary
scratch, writes no configuration or report artifact, and emits only a
private-redacted Boolean result. It is not a prerequisite for routine product
correctness or performance runs.

#### Routine T5 Correctness

The reviewed HW-2 profile and source-oracle-derived T5 configuration
commitments are pinned in the following authorities:

- pin the accepted shared profile digest in
  `crates/xtask/src/host.rs`;
- pin the accepted private configuration digest in
  `crates/xtask/src/import_performance_t5.rs`.

```bash
export MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display
cargo run --release -p xtask -- import-performance-t5 \
  --config /absolute/private/t5-config.json
```

This default command runs exactly one fresh normal-product process session. It
is the routine correctness protocol, not a performance sample set. It can
establish package/scientific correctness and product evidence, but its elapsed
time is only an observation and cannot establish a median or distribution.

Each requested session drives the normal application setup/review/start
importer and starts the monotonic primary clock at the accepted worker spawn.
It includes the
publication-capability currentness check and verified runtime construction
through open-ready, then performs a rendered navigation assertion. Setup,
inspection, and any review interval have separate
monotonic wall and process-CPU evidence and are excluded from the primary
clock. The worker's exact `Published` event through verified open-ready has its
own monotonic wall/process-CPU sub-clock and must reconcile entirely inside the
primary clock. That sub-clock requires transfer mode
`staged_verified_capability`, the inventory/snapshot/inventory currentness
contract, and the storage-issued phase receipt described above. T5 independently
requires both inventories to have the same positive strict-open delta, the
snapshot delta to equal the retained exact proof's expected object count, the
total to equal the checked phase sum, and codec-decode delta zero. Runtime
diagnostics require no active verifier at open-ready and zero successfully
started, failed, accepted-progress, cancelled, or accepted-success ordinary
verification runs since import start.
`xdotool` plus `xwininfo` must independently observe the requested mapped X11 client.
Physical-display attachment remains an explicit owner attestation through
`MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS`; X11 client geometry alone cannot
cryptographically distinguish every physical, VNC, and virtual server. The
parent samples RSS externally, and post-open
readback independently validates the exact closure, scientific identity and
layer roots, all seven scale digests, and every centered transform against the
frozen schema-v2 facts. Source inventory is streamed before and after the
sample set, and the in-clock strong source-revalidation byte counter must equal
the exact reviewed source total. Missing counters, stale
output/checkpoint state, source or executable drift, dirty repository state,
non-release binaries, missing display attestation or external mapped-client
proof, profile/config mismatch, and every mandatory resource or timing gate
applicable to the requested correctness or performance mode fail closed.

The routine runner does not invoke the source oracle. Each produced package
must agree directly with the source-oracle-derived pinned configuration,
including centered transforms, while the before/after inventory proves that
the frozen source binding still holds. Every product sample must also observe
exactly the fixed six checkpoint regular files; the former permissive
eight-file ceiling is not the restored-policy gate.

#### Optional T5 Performance Qualification

A current restored-policy performance claim is explicit and optional:

```bash
export MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display
cargo run --release -p xtask -- import-performance-t5 \
  --config /absolute/private/t5-config.json --performance
```

`--performance` runs exactly three fresh process sessions and evaluates the
existing 15-minute median gate plus cross-sample identity determinism. It is
the only T5 mode that can emit restored-policy performance-qualification
evidence. The default one-sample correctness command records
`three_sample_performance_qualification_not_requested` as an intentional skip;
that is not a correctness failure. Do not rerun the offline oracle before
either mode unless one of the audit triggers above has changed.

The complete raw report, app reports, logs, screenshots, paths, dimensions,
and identities stay below a mode-`0700` external scratch session; runner-created
raw files use mode `0600`. The command prints
only an ignored `target/mirante4d/import-performance-t5/` summary path; that
summary contains no private paths, labels, filenames, dimensions, source or
package identities, or private scientific/source digests. It does retain the
owner-accepted opaque, dataset-linked private-configuration commitment, the
profile binding, a commitment to the finalized private raw report, repository
revision, release-app digest, build/toolchain facts, commands, failures, skips,
waivers, and remaining risks required to audit a qualification claim. Retaining
that opaque configuration commitment is an explicit owner-approved disclosure;
it can confirm a guessed complete private configuration. The runner uses a
fresh private Cargo target, rejects external Cargo configuration and custom
compiler/profile/toolchain overrides, and requires the app report to carry
embedded revision, release-profile, compiler, and fresh-target provenance.
Both owner-accepted digest constants are pinned. Their opaque values remain in
their source authorities and are not repeated here. Diagnostic runs can never
emit qualification evidence.

The private raw report is finalized and synced before the sanitized summary is
constructed. If summary construction, privacy validation, or publication
fails after that point, do not repeat the dataset run. Repair the reporting
code and replay only the finalized raw receipt:

```bash
cargo run --release -p xtask -- import-performance-t5-publish \
  --config /absolute/private/t5-config.json \
  --raw-report /absolute/private/raw-private-report.json
```

This command accepts only the pinned configuration and the exact bounded
private raw-report schema. It rechecks sample, gate, invariant, median, and
qualification consistency; constructs the same allowlisted summary; runs the
private-value validator; and publishes create-new evidence with a binding to
the exact raw bytes. It records measurement and publisher revisions
separately. It does not open the source or package, launch the application or
importer, recompute an oracle, or re-execute a measurement.

#### Sentinel Restoration Evidence Status

The current source implements the guarded `uint8` sentinel policy described in
[Data format and safety](DATA_FORMAT.md). Independent integration coverage
constructs source-adjacent bright boundaries, holes, interrupted lines, odd
tails, and 2D/3D chunk crossings, then reads every package value and validity
bit at every generated LOD. It separately assembles scientific identity and
checks centered axis-aware transforms and canonical-zero invalid values.
Focused production cases cover sentinel zero, a non-255 sentinel, and a valid
derived mean numerically equal to that sentinel. Exhaustive bounded and
deterministic randomized masks compare the optimized base dilation with a
direct neighborhood oracle.

The public T2 authority now derives base and recursive LOD facts through an
independent restored-policy oracle and validates all five scales through exact
package readback. The oracle keeps at most two row-major scratch levels and
computes and hashes fixed Z/Y slabs; its full T2 calculated working peak is
22,578,060 bytes, and no oracle scratch remained after the diagnostic. A
one-sample release run on a dirty worktree matched every frozen fact with a
5.539560251-second primary time, 267,131,688-byte import-ledger peak, and
67,313,664-byte external RSS delta.

On the same dirty worktree, both public fixture validators, `cargo fmt --all --
--check`, workspace all-target Clippy with zero warnings, the full import-
pipeline test set, `cargo xtask verify-pr`, and the six-phase local format
lifecycle passed. `verify-pr` reported 904 selected Nextest cases passed, three
skips, and passing doctests. The two required `product-validate` commands also
passed as E1 automated supporting evidence: `target_source_verification`
accepted two internal GPU readback captures, and `import_preprocessing`
accepted cancellation, resume, and publication with a 267,074,040-byte peak
below 256 MiB. Neither report satisfies E4 product-open or the affected-
dataset real-display product-validation gate. Dirty-tree reports cannot become
qualification evidence.

Interim clean revision `f73cb36d050d701eba5a1c197a802759efdf5137`
(tree `6330b074d5a34dc53177e7e23ca63a36575fdab1`) subsequently passed the
same repository/local set with qualifying evidence. Its five owner-qualified
HW-2/ext4 T2 sessions were 13.654825657, 14.477087517, 14.033167153,
16.611035435, and 13.707822937 seconds, producing a 14.033167153-second median
against the 60-second gate. Every runtime/resource gate passed, the 256 MiB
ledger peak was 267,116,376 bytes, maximum external RSS delta was 68,206,592
bytes, the checkpoint remained six files, source inventory was unchanged, and
exact/scientific identities were deterministic.

Owner review confirmed that the pinned T5 workload is the affected private
sentinel-selected dataset and that the visible defect is fixed. The earlier
no-sentinel assumption was incorrect. Its frozen scientific ID, layer root,
and per-scale digests describe pre-restoration output, so copying a diagnostic
package's new values would be self-blessing. At clean revision `d6478d9`, two
full bounded source-oracle derivations completed independently and produced
byte-identical schema-v2 candidate configurations without source mutation or
retained scratch. That one-time fact freeze is complete. The five-session T2
evidence at `f73cb36` remains accepted because subsequent changes affect only
the private xtask fact/evidence boundary.

Clean measurement revision
`30bb16758d22055deb1e52fe6803b95592094eee` passed all 11 `verify-pr` policy
phases and all eight Rust phases with 924 tests discovered. Exactly one fresh
release normal-product T5 correctness sample then passed on the accepted
profile and owner-attested real display. All 25 per-sample gates passed,
including package/scientific identity, layer root, all seven scale digests,
centered transforms, canonical invalid values, source preservation, fixed
checkpoint and resource limits, verified publication, mapped display,
navigation, and counter reconciliation. Every cross-run invariant passed; the
report has no failures or waivers. Its sole skip is the intentionally
unrequested three-sample performance qualification.

The primary time was 960.549555223 seconds. This single observation is not a
median or distribution and does not qualify current T5 performance. A
post-measurement sanitizer false positive rejected the Boolean
`canonical_base_pixel_bytes` gate name as though it were the private numeric
fact. The finalized mode-`0600` raw receipt remained valid and unchanged.
Clean publisher revision `5a9077d407bf5bdc9c78caf75e558e0c63950afa`
corrected the type-aware sanitizer and replayed only summary publication; it
did not re-execute the measurement. The raw-report SHA-256 is
`9d71ca22aa993c212c7f93de215b952f7b5b059faa8aae8de5181c131ca1c9e6`,
and the sanitized-summary SHA-256 is
`377af022d4c1205031e3eb8d5d6a874ce2339026560445887b42beff5f20b365`.
Private paths, labels, geometry, source/package identities, and raw facts remain
external. No restored-policy three-sample T5 performance qualification is
claimed unless the optional `--performance` run is explicitly requested.

#### Accepted Import And Preprocessing Qualification

The immutable qualification revision for the pre-restoration
import/preprocessing implementation is
`eb5c9ffd12cbd9fce65bd03559b8e7f93170d72e`, with tree
`fec040f4772fbfa0e59bfd18babf178030b073c3`. Its report records
`evidence_class: qualification`. The complete required automated check set
passed at that revision. Both sample sets ran on the owner-accepted HW-2/ext4
tuple with a 256 MiB import budget, declared warm cache, and declared no
competing activity. The normal native T5/DS-3 route was product-validated there
with an externally observed mapped native X11 client and owner-attested
physical-display attachment.

Those performance and product claims remain valid only for that named
revision and its recorded executables. They predate the sentinel semantic
cutover and do not qualify the current source.

| Workload | Sampling and primary-clock observations | Median gate | Result |
| --- | --- | --- | --- |
| Public generated full-plane-strip T2 | Five independent release process sessions: 4.581971238 s, 4.609749805 s, 4.624230190 s, 4.593098360 s, and 4.654008439 s | 4.609749805 s at most 60 s | Passed |
| Private T5/DS-3 | Three fresh release normal-product sessions: 702.537630793 s, 678.022452456 s, and 691.093848488 s | 691.093848488 s (11 min 31.094 s) at most 15 min | Passed; non-blocking 10-minute stretch missed |

All recorded per-sample runtime and resource gates passed. T5 observed a
251.075 MiB maximum import-ledger peak within the 256 MiB budget, a 98.180 MiB
maximum external RSS delta against the 384 MiB gate, 35 peak import-owned file
descriptors against the 64-file bound, six checkpoint files against the
eight-file bound, and 1,817 counted durability calls per sample against the
5,000-call exclusive ceiling. Every exact, scientific, independent per-scale,
source-preservation, determinism, counter-reconciliation,
publication-capability, open-ready, and rendered-navigation check passed. The
T5 report records no failures, skips, or waivers, and no mandatory closeout
check was skipped or waived.

The final deletion audit at that revision found the intended single importer
and six-file checkpoint authority. It found no predecessor checkpoint reader,
per-work-unit or per-regular-object durability predecessor, decoded
publication route, automatic exact/scientific rescan in the imported
publication-to-open-ready route, fixed-percentage progress route, alternate
importer, or hidden selector. A later explicit external open remains a
separate normal route that performs full background verification.

The applicable executables remained unchanged within each sample set, and the
`xtask` digest matched across T2 and T5.
The standard release build used
`rustc 1.96.1 (31fca3adb 2026-06-26)` with LLVM 22.1.2, no custom Rust flags,
and no compiler wrapper. The release `xtask` SHA-256 was
`3c952cb3a80313d07db2b790b556f79153d5a2632fe8bbfb4ae490a369f3e7aa`;
the fresh-private-target application SHA-256 was
`e7dbe36d2b0abc049510207ef9daafdb884a122cfb8dfd4f22738839a935a0bb`.
The T2 report SHA-256 was
`33a27f748b012d1548391f0c8320056e1ecf11932c925b8366a033278cb38fe8`.
The sanitized T5 summary SHA-256 was
`2d53394f67d7c4e82ec422750f576f8f860dc543c97a86e6491f9f0295eaa4c9`,
and it binds the finalized private raw report through SHA-256
`1bbf0f18c19fb7fe2c72cade9f0bd50e8cb774ae1ec2c24d13a16e36c1a3502e`.
No private path, label, filename, dimensions, source identity, package
identity, or scientific digest is published here.

The final revision-bound checks were:

```bash
python3 tools/source-fixtures/validate.py \
  --manifest fixtures/source/manifest.json --self-test
python3 tools/target-fixtures/t1/validate.py \
  --manifest fixtures/target/manifest.json --self-test
cargo fmt --all
cargo xtask verify-pr
cargo xtask verify-local format-lifecycle
cargo xtask product-validate target_source_verification
cargo xtask product-validate import_preprocessing
cargo run --release -p xtask -- import-performance-t2 \
  --samples 5 --scratch <owner-qualified-ext4-scratch> \
  --qualification-profile <owner-pinned-profile> \
  --cache-condition warm --competing-activity none
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo run --release -p xtask -- import-performance-t5 \
  --config <owner-pinned-private-config>
```

The owner accepted these remaining risks:

- no relative-speedup or timing-tail claim is made from these sample counts;
- cache state and competing activity are declarations, not operating-system
  controls;
- physical-display attachment is owner-attested and is not cryptographically
  proved by mapped X11 geometry;
- the retained owner-approved private-configuration commitment can confirm a
  correctly guessed complete configuration; and
- the non-blocking 10-minute stretch target was not met.

The filesystem-wide `syncfs` latency, writeback-error, and cancellation
coupling and the cooperative destination-parent threat-model limitation in
[Current state](CURRENT_STATE.md) also remain accepted limitations; neither was
weakened to meet the timing gate.

This record is revision-bound. Revision
`85350219efcc0c96b492f9a5029ba80752b49306`, the clean predecessor to the
sentinel restoration, was documentation-only and was not performance-
qualified. The current sentinel cutover does not inherit the older
qualification and still requires the one-sample correctness evidence described
above. Later product, performance,
threshold, evidence-schema, or evidence-policy changes affecting this boundary
also require fresh evidence.

Rendering, linked-panel, or packaged-viewer changes use:

```bash
cargo xtask product-validate target_fixture_render_modes
cargo xtask product-validate \
  target_fixture_resident_navigation_no_readback
```

Both use a promoted small fixture and preserve its source package. The render-
modes scenario covers MIP; perspective SmoothLinear; DVR; flat and gradient-
lit ISO; attached and detached ISO light; orthographic linked panels;
crosshair, ROI, and distance tools; 1280x720; and a short 1920x1080 exercise.
The resident-navigation scenario requests a complete scale-0 frame, waits for
runtime idle, records diagnostics, performs a small orbit/pan, waits for the
new complete scale-0 frame, then observes 120 settled frames. It intentionally
does not enable validation capture or synchronous readback, so its navigation
path is representative of ordinary presentation. Its internal automation and
diagnostic snapshots remain supporting evidence, not a timing qualification.
There is no 4K or simulated TiB requirement. Packaging changes run the
scenario against the packaged executable as described in [Release](RELEASE.md).

For the required real-display exercise, attest the display class explicitly
and inspect the native mapped client and logs rather than treating automation
as product validation by itself:

```bash
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    target_fixture_resident_navigation_no_readback
```

## Product Validation

Rendering, viewport, GPU, data-loading, interaction, and large-dataset changes
are incomplete until the actual viewer is opened on a real display with the
relevant dataset and hardware, unless the user explicitly waives that check.
Exercise the changed workflow, confirm the application remains alive without a
hidden fallback or repeated GPU error, and inspect the resulting logs. Use the
packaged application when packaging or release behavior changed.

Scientific checks should use independent expected facts where correctness is
in question. Storage and import checks must prove source nonmutation, bounded
and cancellable work, atomic publication, and sharded output without
file-per-brick growth. Exact cases belong in focused tests, not a copied matrix
in this guide.

Historical foundation acceptances remain available in Git history and
create-once tags.

## Reporting

Report the meaningful commands and results, the real dataset/display/hardware
when relevant, important skips or waivers, and remaining risk. A performance
claim must also name its workload, metric, sampling method, and threshold.
