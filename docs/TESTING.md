# Testing And Validation

Last updated: 2026-08-04

Testing must expose useful failures quickly. More tests do not replace correct
product behavior or direct product use.

## Claim Language

- **Implemented:** the change exists in the stated revision.
- **Automated-verified:** the relevant automated checks passed for that
  revision.
- **Product-validated:** the normal native application ran on a real display,
  and the affected workflow was exercised on relevant data and hardware.

Do not combine these claims. Unit tests, fixtures, snapshots, headless GPU
tests, render readbacks, benchmarks, and automation are supporting evidence.
They do not prove that a user-visible workflow works on the normal product.

Historical results stay bound to their revision. A later change does not
inherit them automatically.

## Verification Tiers

Use the smallest check that can falsify the current edit. Close the work with
the checks required by the changed boundary.

### Tier 0: Documentation And Static Metadata

```bash
cargo xtask docs-check
```

When verification metadata changes, also run:

```bash
cargo xtask verification-sync --check
```

### Tier 1: Focused Development

Format Rust and run the narrowest useful package or named test:

```bash
cargo fmt --all
cargo test -p <affected-package> <focused-test-name>
```

Prefer tests with independent expected results and negative cases. Do not
write tests that only repeat the implementation.

### Tier 2: Public Pull-Request Checks

```bash
cargo xtask verify-pr
```

The public checks are:

- `PR / policy` for policy, generated metadata, dependencies, licenses,
  documentation, and workflow shape; and
- `PR / rust` for formatting, lint, build, unit, contract, and UI tests.

Focused leaves are available:

```bash
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
```

Tests have no automatic retry. Exact selectors and test ownership live in
`verification/registry.json` and test source.

### Tier 3: Changed-Boundary Local Checks

Run these only when their owned boundary changes.

| Changed boundary | Required local route |
| --- | --- |
| Renderer correctness, GPU residency, render API, or dataset-runtime GPU handoff | `verify-local trusted-gpu-correctness` |
| GPU shaders, work, scheduling, presentation, interaction, or performance claims | `gpu-performance` and the relevant mapped product workflow |
| Project-store durability or accepted-filesystem lifecycle | `verify-local project-store-lifecycle` |
| Import, TIFF parsing, no-data, package production, or publication | source fixture validation and `product-validate import_preprocessing` |
| Target package profile, reader, writer, or independent corpus | target fixture validation and affected storage checks |
| Package construction or packaged lifecycle | `package-linux-release` and package product checks |
| Product state, interaction, or visible rendering | the relevant normal mapped product scenario |

The trusted GPU correctness command is:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu-correctness
```

It discovers and reconciles the exact registered 25-case inventory. It runs
serially with zero retries on the designated Vulkan adapter. It proves
correctness only, not performance.

The project-store lifecycle command is:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local project-store-lifecycle
```

Use it only for the qualified durability boundary. Public runners keep
portable unsupported-filesystem behavior. They do not claim accepted ext4
durability.

### Tier 4: Product Validation

Rendering, loading, GPU, interaction, and large-data changes require the
normal native application on a real display unless the owner explicitly
waives that check.

Current product scenarios include:

```bash
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes

MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    /absolute/path/to/dataset.m4d representative_native_navigation

MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    /absolute/path/to/time-series.m4d representative_temporal_playback

cargo xtask product-validate import_preprocessing
cargo xtask product-validate target_package_integrity_audit
```

Automation through the normal application is automated evidence. Direct owner
observation is still required when a plan requires owner product acceptance.

## Independent Scientific Evidence

Writer and reader agreement is not enough for a scientific or format claim.
Use independent expected facts, hand vectors, or an independent reader.

The source TIFF corpus separates byte production, fact calculation, and
independent reading. The target corpus separates the producer, fact oracle,
independent reader, hand vectors, and mutation recipes. Product conformance
must consume those facts through production code.

Do not regenerate a fixture, snapshot, or golden only because product output
changed. Explain and review an intentional expected-result change.

Tests for concurrent work must wait on the exact request, ticket, result, or
generation they own. Aggregate queue or worker counts are not exact barriers
while unrelated work can advance.

## GPU Performance Campaign

Performance uses a separate local campaign:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
MIRANTE4D_GPU_ENVIRONMENT_QUIESCENT=1 \
MIRANTE4D_GPU_DISPLAY_PROFILE_ID=<configured-id> \
MIRANTE4D_GPU_COMPOSITOR_SESSION_ID=<configured-id> \
MIRANTE4D_GPU_POWER_POLICY_ID=<configured-id> \
MIRANTE4D_GPU_QUIESCENCE_POLICY_ID=<configured-id> \
  cargo xtask gpu-performance --config /absolute/private/config.json
```

The private file follows
`verification/gpu-performance-config.schema.json`. It must use mode 0600. It
pins clean revisions, target directories, workload identity, adapter, driver,
display, power, thermal, and quiescence facts. The runner rejects GitHub
Actions, Wayland, dirty revisions, automatic retries, and changed environment
facts.

Component functions use 30 warmups and 120 measured frames. A component GPU
p95 above 33.3 ms fails its 30 Hz feasibility check. A result at or below that
limit is necessary but does not prove the full product. A 16.667 ms component
result is preferred, not a universal hard requirement.

The mapped product uses present completion, an independent X11 marker, and
`VK_EXT_present_timing` first-pixel-out feedback. The accepted image must have
the correct product identity. Internal publication or readback alone is not a
visible-product performance result.

The absolute representative-product limits are:

| Situation | Metric | Limit |
| --- | --- | ---: |
| Standalone interaction | first-pixel-out interval p95 | 33.3 ms |
| Four-panel interaction | first-pixel-out interval p95 | 33.3 ms |
| Resident input response | input to next correct change p99 | 50 ms |
| Active visible stall | maximum gap | 100 ms |
| Resident exact settlement | release to exact/current | 250 ms |
| Prepared nonresident replacement | admission to exact/current | 1 second |
| Fresh app with warm OS cache | demand to complete coarse output | 250 ms |
| Fresh app with warm OS cache | demand to exact settlement | 2 seconds |

Cold storage time is reported separately. A stale, blank, partial, repeated,
or silently coarser image cannot reset an interaction or stall clock.

An accepted relative baseline needs ten fresh calibration runs and explicit
owner acceptance. The component GPU p95 limit is 1.05 times the comparable
baseline. Warm product p95 and refinement latency must be at most the greater
of 1.10 times baseline or baseline plus 1 ms. A relative gate is inactive when
calibration variation exceeds half its allowed regression.

Baseline and candidate runs use fixed A/B, B/A, and A/B pair order. Scientific
work, pixels, quality, and environment must match. The campaign never replaces
its own baseline.

The accepted baseline is currently pending. Do not present an absolute smoke
run as baseline acceptance or a performance-improvement claim.

## Active Static-Recovery Evidence

The current viewer correction uses one controlled A/B/C campaign. A and B are
eligible pre-correction revisions. C is the current correction. One identical
evidence overlay measures matching scientific work across the eligible
revisions.

Fixed-LOD GPU p95 must be at most 1.05 times the better valid baseline. Warm
navigation retains zero data reads, decodes, requests, uploads, and evictions.
Settled idle submits no renderer work. Each direct color cutoff uses at most
one color submission. Product UI-update p95 and exact-settlement time must be
at most the greater of 1.10 times baseline or baseline plus 1 ms.

If no eligible baseline can express the same work, the row is unevaluated.
The result cannot claim improvement. Correctness work can remain implemented
while performance recovery stays open.

## Host-Stress Warning

The linked-S0 diagnostic previously froze the desktop. It has no valid monitor
performance result. The command remains quarantined behind explicit
`--allow-host-stress` acknowledgement. Do not run it unattended or treat the
flag as a safety guarantee.

## Reporting

A completion report must state:

- the revision and meaningful command;
- the relevant fixture, dataset class, display, and hardware;
- the exact result and claim class;
- failures, skips, waivers, or quarantined checks;
- whether the tree was clean when the check required it; and
- remaining risk.

A performance claim must also state the workload, metric, sample method,
threshold, environment, and comparison rule. Do not expose private paths or
unpublished dataset metadata.
