# Testing And Evidence

Last updated: 2026-07-11

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

## Current Local Checks

The temporary general check is:

```bash
cargo xtask verify-bootstrap
```

It checks formatting, workspace compilation, exactly 169 selected CPU tests,
and documentation. It has bounded phase timeouts and zero test retries. It
explicitly excludes the complete suite, Clippy, doctests, dependency policy,
GPU, UI snapshots, E2E, packaging, performance, scientific data, real data,
and product validation.

Documentation alone can be checked with:

```bash
cargo xtask docs-check
```

That command validates the exact documentation inventory, authority ownership,
navigation, local links, and heading anchors. Command discovery is owned by
`cargo xtask --help`; this document does not duplicate the full command list.

The old aggregate topology is not trusted: `verify-fast` fails a superseded
source-size rule, recursive aggregates duplicate work, `report-audit` has an
inherited mismatch, and the full suite is slow and integration-heavy.

## Hosted And Trusted-Local Boundary

The current public repository has one provisional required pull-request job,
`Bootstrap / required`, on a standard hosted runner. It executes the temporary
bridge. Hosted verification has a hard `$0` budget, no public self-hosted
workstation, no private data, no automatic retry, and no cache or artifact
storage by default.

GPU, packaged E2E, E0-E4, performance, stress, private T5, and scientific
evidence run only on trusted local machines. Private dataset paths stay in
local resolvers and never enter the public tree or hosted logs.

No benchmark baseline is currently authoritative. The temporary tool-owned
[baseline directory](benchmarks/baselines/README.md) remains only for WP-06
disposition.

## Product Validation

Rendering, viewport, GPU, data-loading, interaction, and large-dataset changes
are incomplete until the actual viewer is opened on a real display with the
relevant dataset and hardware, unless the user explicitly waives the gate.
Validation must exercise the changed workflow, confirm the app remains alive
without a hidden fallback or repeated GPU error, and inspect the resulting
logs and evidence. Packaging or release changes use the packaged application.

## WP-06 Target

WP-06 replaces the current bridge with six nonrecursive leaves and exactly two
required public checks, while keeping GPU/product/performance/scientific work
trusted-local. It also owns independent T1 fixtures, generated T2 support data,
private T5 identity, E0-E4 evidence, timeout policy, and the complete test
inventory disposition.

The exact target and thresholds live in the
[verification brief](plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md).
They are not current commands or proof until WP-06 accepts them.

## Reporting

Completion reports name the revision, commands, fixtures or datasets,
hardware/display where relevant, results, failures, skipped checks, waivers,
and remaining risk. Performance claims also name the workload, metric,
sampling method, and threshold.
