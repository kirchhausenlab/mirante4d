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

## WP-06A Checks

This revision exposes six independently selectable leaves:

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

`verify-pr policy` and `verify-pr rust` select one group when focused feedback
is useful. The Rust group performs one discovery/build, checks exact ownership,
then runs the unit/contract/UI union once before doctests. Tests are never
retried.

The generated selectors and Nextest configuration must match their registry:

```bash
cargo xtask verification-sync --check
```

The checkpoint inventory has 879 live tests: 839 normal public-CPU tests and 40
ignored trusted-GPU tests. The old recursive aggregates, `verify-fast`, and
target-directory `report-audit` are no longer live authorities in the
checkpoint.

Documentation alone can be checked with:

```bash
cargo xtask docs-check
```

That command validates the exact documentation inventory, authority ownership,
navigation, local links, and heading anchors. Command discovery is owned by
`cargo xtask --help`; this document does not duplicate the full command list.

## Hosted And Trusted-Local Boundary

The protected repository still has one required pull-request context,
`Bootstrap / required`. Candidate `PR / policy` and `PR / rust` jobs, plus
matching non-required `Main / ...` jobs, run in shadow on standard public
runners. They do not become required until the twenty-run cache-free window is
accepted and the branch rule is replaced and read back.

Hosted verification has a hard `$0` budget, no public self-hosted workstation,
no private data, no automatic retry, and no cache or artifact storage.

GPU, product E1-E4, packaged E2E, performance, stress, private T5, and
scientific evidence are separate trusted-local lanes. The current GPU command
is deliberately local-only:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu
```

Run it only on the designated clean Vulkan workstation. Private dataset paths
stay in local resolvers and never enter the public tree or hosted logs. The
package-capability lane is registered as pending; WP-06 does not invent a
passing package claim before an honest unsupported-GPU command exists.

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

## WP-06 Remaining Gates

The machinery alone is not a completed cutover. WP-06 still requires protected
integration and acceptance, twenty consecutive qualifying cache-free Main
attempts, required-context replacement, separate bootstrap cleanup,
exact-revision product-open validation, and its create-once exit tag.

The checked independent report supports only the WP-03 source-TIFF archive.
Current schema-1 packages remain non-authoritative T2 support fixtures;
target-format T1 conformance cannot begin before WP-10A.

The exact thresholds live in the
[verification brief](plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md).

## Reporting

Completion reports name the revision, commands, fixtures or datasets,
hardware/display where relevant, results, failures, skipped checks, waivers,
and remaining risk. Performance claims also name the workload, metric,
sampling method, and threshold.
