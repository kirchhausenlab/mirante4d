# Testing And Validation

Last updated: 2026-07-26

Testing exists to expose meaningful failures quickly. It is not a measure of
project seriousness, and a large passing suite is not a substitute for using
the product.

## Claim Language

- **Implemented:** the change exists in the named revision.
- **Automated-verified:** the named automated checks passed for that revision.
- **Product-validated:** the normal native application ran on a real mapped
  display and the affected workflow was exercised on relevant data and
  hardware, with its output and logs inspected.

Do not collapse these claims. Unit tests, snapshots, virtual-display
automation, benchmarks, render readbacks, and protocol receipts are supporting
evidence. They do not establish that the visible product works.

Historical qualification claims remain bound to their named revisions. Later
changes do not inherit them automatically, and routine development does not
need to reproduce them.

## Verification Tiers

Use the smallest tier that can falsify the change while iterating, then close
the work with the checks appropriate to the affected boundary.

### Tier 0: Documentation Or Static Metadata

Run:

```bash
cargo xtask docs-check
```

Use `cargo xtask verification-sync --check` as well when verification metadata
changes.

### Tier 1: Focused Development

For Rust changes, format the workspace and run the narrowest useful package,
module, or named test:

```bash
cargo fmt --all
cargo test -p <affected-package> <focused-test-name>
```

Prefer a few tests with independent expected results over duplicated matrices,
large snapshots, or tests that merely reproduce the implementation.

### Tier 2: Public Pull-Request Checks

Before handing off a substantial cross-cutting change, run:

```bash
cargo xtask verify-pr
```

The protected repository has two public checks:

- `PR / policy` validates repository policy, generated metadata, dependencies,
  licensing, documentation, and workflow shape.
- `PR / rust` formats, lints, builds, and runs the ordinary Rust test groups.

Focused public leaves remain available:

```bash
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
```

Tests are not retried automatically. Generated selectors and exact test
ownership live in `verification/registry.json` and test source; this document
does not duplicate their inventory.

Expensive exhaustive or fault-injection matrices belong in explicit
developer-local lanes. They are not hidden prerequisites for ordinary pull
requests.

When their owned project-store or imported-publication boundary changes, run:

```bash
cargo test -p mirante4d-project-store matrix -- --ignored
cargo test -p mirante4d-app imported_ -- --ignored
```

### Tier 3: Changed-Boundary Checks

Run these only when their boundary changes.

Renderer, GPU-resource, or dataset-runtime work can use the trusted local
Vulkan lane on the designated workstation:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu
```

Focused release diagnostics are available for resident rendering and
representation choices:

```bash
cargo test --release -p mirante4d-render-wgpu \
  resident_volume_gpu_timing -- --ignored --nocapture
cargo test --release -p mirante4d-render-wgpu \
  payload_buffer_vs_texture_gpu_timing -- --ignored --nocapture
```

Package-format, storage-reader/writer, identity, or publication-boundary
changes use:

```bash
python3 tools/target-fixtures/t1/validate.py \
  --manifest fixtures/target/manifest.json --self-test
cargo xtask verify-local format-lifecycle
```

Project-store durability changes use:

```bash
python3 tools/project-fixtures/validate.py \
  --manifest fixtures/project/manifest.json --self-test
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local project-store-lifecycle
```

The project-store lifecycle lane is intentionally not a routine PR check.

Import and preprocessing work can use:

```bash
cargo xtask product-validate import_preprocessing
cargo run --release -p xtask -- import-performance-t2 --help
cargo run --release -p xtask -- import-performance-t5 --help
```

Private datasets and configuration stay outside the repository. Source
nonmutation, bounded work, cancellation, and atomic create-only publication
remain mandatory. Full private qualification is run only when the affected
boundary or a new claim requires it.

### Tier 4: Product Validation

Rendering, viewport, GPU, loading, interaction, and large-dataset changes are
not complete until the actual viewer is exercised on a real display with the
relevant workload and hardware, unless the user explicitly waives that check.

Useful bounded scenarios include:

```bash
cargo xtask product-validate target_source_verification
cargo xtask product-validate target_fixture_render_modes
cargo xtask product-validate \
  target_fixture_resident_navigation_no_readback
```

For the real-display exercise:

```bash
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    target_fixture_resident_navigation_no_readback
```

Inspect the native mapped client, the changed workflow, visible output, and
logs. Confirm that the application remains alive without a hidden fallback or
repeating GPU error. Use the packaged application when packaging or release
behavior changed.

## What Must Remain Strong

The simplification of development process does not weaken these product
boundaries:

- scientific values, validity, axes, calibration, and independent numerical
  expectations;
- source nonmutation and identity where a source is imported or verified;
- bounded memory, descriptors, queues, and cancellation;
- atomic create-only publication and project/package durability;
- per-object integrity at storage and transport boundaries; and
- real viewer behavior on relevant data and hardware.

Exact hashing is appropriate for durable object identity, source identity,
scientific identity, publication integrity, and external downloads. Rehashing
the same immutable inputs at every internal step, hash-chaining development
checkpoints, or signing ordinary test output is not a default requirement.

## Retired Viewer Qualification Protocols

The EP-00 and EP-01 viewer-performance protocols are frozen historical
development artifacts. Their raw-report, receipt, replay, population,
selection, and per-role provenance machinery is not a current development
prerequisite and must not block product work. The transitional commands may
remain in a checkout until the active simplification plan removes them, but
new work must not extend or depend on them.

Viewer-performance work now proceeds in small product slices:

1. resident interaction and per-view LOD;
2. cold complete refinement, reuse, and presentation correctness; and
3. MIP, DVR, and ISO kernels.

Each slice closes with focused automated checks, a small independent
correctness oracle where needed, useful GPU timings, and real-product
validation. See the active
[development-simplification plan](plans/active/DEVELOPMENT_SIMPLIFICATION.md).

## Reporting

Report:

- the meaningful commands and results;
- the real dataset, display, and hardware when relevant;
- important skips or explicit waivers;
- the visible product result; and
- remaining risk.

A performance claim must also name its workload, metric, sampling method,
cache condition, and threshold. Avoid publishing private paths, labels,
geometry, or scientific identities.
