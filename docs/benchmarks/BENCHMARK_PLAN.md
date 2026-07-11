# Benchmark Plan

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the current benchmark policy without preserving completed phase logs in
the active read path.

## Benchmark Roles

- **Smoke benchmarks** prove commands run and reports are shaped correctly.
- **Native package benchmarks** measure current package open/read/render
  behavior for a named package.
- **Import benchmarks** measure source import/preprocessing into strict native
  packages.
- **Renderer benchmarks** measure mode-specific rendering, resource use,
  residency, and failure behavior.
- **Product validation reports** exercise the real native app and are not
  timing baselines by default.
- **Curated baselines** are small committed JSON reports used only when schema,
  scenario, dataset class, hardware class, and host context match.

## Required Context

Every promoted benchmark report must identify:

- command and scenario
- app version and git revision
- dirty-worktree state
- build profile
- OS, CPU, GPU adapter/backend/driver, and hardware class
- dataset identity, dataset class, native format, and schema version
- report schema/version
- timing metrics and relevant resource counters
- whether heavy/private sample data was used

Reports from dirty worktrees may be useful investigation evidence, but they are
not promotable curated baselines.

## Benchmark And Evidence Commands

The exact command surface is classified by `cargo xtask command-audit` and
printed by `cargo xtask --help`. This section lists benchmark and
benchmark-adjacent evidence commands by role.

Benchmark commands:

```bash
cargo xtask bench-smoke
cargo xtask bench-runtime-stress
cargo xtask bench-native-package <native-package.m4d>
cargo xtask bench-import-sample <experiment>
cargo xtask bench-phase11-large-view <native-package.m4d>
cargo xtask bench-phase11-interaction <native-package.m4d>
cargo xtask bench-phase11-viewport-matrix <native-package.m4d>
cargo xtask bench-phase11-synthetic-matrix
cargo xtask bench-phase13-renderer <native-package.m4d>
cargo xtask bench-phase13-viewport-matrix <native-package.m4d>
cargo xtask bench-phase14-multichannel
cargo xtask bench-phase15-analysis
```

Audit/evidence commands:

```bash
cargo xtask phase10-audit
cargo xtask phase12-audit
cargo xtask phase14-audit
cargo xtask phase15-audit
cargo xtask phase17-audit
cargo xtask phase19-audit
cargo xtask phase20-smoke-audit
cargo xtask phase20-extreme-audit
cargo xtask phase20-extreme-sample <T5-QUAL-002|T5-QUAL-001>
```

Baseline commands:

```bash
cargo xtask bench-check <current-benchmark.json> <baseline-benchmark.json>
cargo xtask baseline-audit
cargo xtask baseline-refresh-plan [benchmark-report-root]
cargo xtask baseline-promote <current-benchmark.json> <baseline-name.json>
cargo xtask baseline-promote-manifest <promotion-manifest.json>
```

Commands that touch heavy local packages require:

```bash
MIRANTE4D_XTASK_ALLOW_HEAVY_BENCHMARK=1
```

## Baseline Policy

Curated baseline files live under `docs/benchmarks/baselines/`.

Baseline comparison must refuse mismatched:

- report schema/version
- scenario
- hardware/host identity
- baseline class
- dataset class/context

Do not refresh a baseline solely to hide a regression. Refresh only after a
deliberate rerun from a clean worktree with matching context and a reviewed
promotion path.

## Heavy Data Policy

Local T5-QUAL-001/T5-QUAL-002/extreme sample reports are private/heavy evidence unless
explicitly promoted through a documented policy. They should normally remain
under `target/mirante4d/`.

Do not commit private source data, generated large packages, or target report
directories.

## Acceptance Policy

Performance claims require report paths and context. A benchmark without
hardware, dataset, schema, and build context is diagnostic only.

Renderer or interaction work still needs the product-open validation gate from
`../TESTING.md` when the work touches renderer, viewport, GPU, data-loading,
interaction, or large-dataset behavior.
