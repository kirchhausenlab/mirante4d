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
