# Testing And Evidence

Last updated: 2026-07-15

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
  "schema": "mirante4d-private-import-performance-t5-1",
  "workload_id": "t5-0123456789abcdef0123456789abcdef",
  "source": "/absolute/qualified/ext4/scratch/private-source",
  "scratch_root": "/absolute/qualified/ext4/scratch/t5",
  "qualification_profile": "/absolute/local/hw-2-import-profile.json",
  "expected_profile": "DS-3",
  "spacing_zyx_um": [1.0, 1.0, 1.0],
  "time_step_seconds": null,
  "no_data_sentinel": null,
  "working_memory_bytes": 268435456,
  "primary_timeout_seconds": 1200,
  "cache_condition": "warm",
  "competing_activity": "none",
  "expected": {
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
    ]
  }
}
```

Replace every placeholder with independently frozen private facts and include
exactly one entry for every physical image/scale and logical scientific layer.
The source-inventory digest uses the `mirante4d-t5-source-inventory-1` domain
and binds the sorted relative names, lengths, and complete contents of every
source file. Freeze it independently before owner acceptance; a diagnostic run
can expose the candidate only in its external private raw report. The separate
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

An explicit diagnostic run can collect the candidate opaque configuration and
profile digests without creating a qualification claim:

```bash
export MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display
cargo run --release -p xtask -- import-performance-t5 \
  --config /absolute/private/t5-config.json \
  --diagnostic --samples 1
```

Bootstrap is deliberately at least two-pass. The first diagnostic uses
syntactically valid placeholders and reveals the actual inventory, reviewed
source fingerprint, canonical base-byte total, scientific/layer facts, and
per-scale facts only in its private raw report. Replace every expected fact,
then run the diagnostic again with that complete candidate configuration and
confirm that all workload/science gates pass and the facts remain stable. Pin
only the final configuration digest from this second stable run; the first
digest commits placeholders and is never eligible for owner acceptance.

The reviewed HW-2 profile and stable two-pass T5 configuration commitments are
now pinned in the following authorities. The mandatory protocol omits
`--diagnostic` and uses its fixed three fresh process sessions:

- pin the accepted shared profile digest in
  `crates/xtask/src/host.rs`;
- pin the accepted private configuration digest in
  `crates/xtask/src/import_performance_t5.rs`.

```bash
export MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display
cargo run --release -p xtask -- import-performance-t5 \
  --config /absolute/private/t5-config.json
```

Each session drives the normal application setup/review/start importer, starts
the monotonic primary clock at the accepted worker spawn, includes the
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
layer roots, and every scale digest. Source inventory is streamed before,
between, and after sessions, and the in-clock strong source-revalidation byte
counter must equal the exact reviewed source total. Missing counters, stale
output/checkpoint state, source or executable drift, dirty repository state,
non-release binaries, missing display attestation or external mapped-client
proof, profile/config mismatch, and every mandatory resource or timing gate
fail closed.

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
Both owner-accepted digest
constants are currently unset, so only explicitly diagnostic runs can execute,
and `--diagnostic` can never emit qualification evidence even after the
constants are pinned.

Rendering, linked-panel, or packaged-viewer changes use:

```bash
cargo xtask product-validate target_fixture_render_modes
```

Both use promoted small fixtures and preserve their source packages. The
render scenario covers MIP, DVR, ISO, linked panels, 1280x720, and a short
1920x1080 exercise. There is no 4K or simulated TiB requirement. Packaging
changes run the scenario against the packaged executable as described in
[Release](RELEASE.md).

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
