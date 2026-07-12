# Testing And Evidence

Last updated: 2026-07-12

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

## WP-06 Checks

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

The live test inventory is discovered and assigned by the verification
registry; generated selectors must not be hand-edited. The old recursive
aggregates, `verify-fast`, and target-directory `report-audit` are not live
authorities.

Documentation alone can be checked with:

```bash
cargo xtask docs-check
```

That command validates the exact documentation inventory, authority ownership,
navigation, local links, and heading anchors. Command discovery is owned by
`cargo xtask --help`; this document does not duplicate the full command list.

## Hosted And Trusted-Local Boundary

The protected repository requires exactly `PR / policy` and `PR / rust`.
Matching non-required `Main / policy` and `Main / rust` jobs run on protected
main. The twenty-attempt cache-free calibration passed before the required-
context list was replaced and read back; the transitional Bootstrap bridge is
no longer part of the repository.

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
[baseline directory](benchmarks/baselines/README.md) remains diagnostic until
WP-14 owns its final disposition.

## Product Validation

Rendering, viewport, GPU, data-loading, interaction, and large-dataset changes
are incomplete until the actual viewer is opened on a real display with the
relevant dataset and hardware, unless the user explicitly waives the gate.
Validation must exercise the changed workflow, confirm the app remains alive
without a hidden fallback or repeated GPU error, and inspect the resulting
logs and evidence. Packaging or release changes use the packaged application.

## Current Boundaries

WP-06 is complete: its exact protected-main revision passed the real Vulkan
viewer exercise at 1280x720 and 1920x1080 before
`foundation-wp-06-exit-1` was created. WP-07A is accepted at
`foundation-wp-07a-exit-1` (`5383cbb93c13c59e6f035bfa551356c75fb426dc`).
WP-07B is accepted at `foundation-wp-07b-exit-1`. WP-08A's corrected contract
is accepted at `foundation-wp-08a-exit-2`
(`f2e520da891134d1b3f65d8fcac7afb4140579a2`). WP-08B is accepted at
`foundation-wp-08b-exit-1`
(`0e3bdb0f5257c820841cee215cee38747efbda75`) after exact-main public,
trusted-GPU, source-nonmutation, and real-display 1280x720 T2 product checks.
Internal automation remains supporting evidence, not product-open proof.

The checked independent report supports only the WP-03 source-TIFF archive.
WP-10A-B implementation is active. `mirante4d-storage` is assigned to the
existing contract leaf, but its current tests prove only pure profile, path,
arithmetic, supporting exact-identity, scalar-wire, and restricted-JCS
contracts, including the closed profile, canonical-value, scientific, and
display-defaults grammars, verified recipe payloads, and exact manifest
descriptor/page/root bytes. Closed portable-record tests prove structural
canonical bytes; derivation and detached-release tests additionally prove typed
identity verification. They make no filesystem-package, T1 conformance,
independent-reader, lifecycle, or product-support claim. Current schema-1
packages remain non-authoritative T2 support fixtures until WP-10A accepts the
independent target evidence.

The exact thresholds live in the
[verification brief](plans/active/foundation-refactor/VERIFICATION_EVIDENCE_BRIEF.md).

## Reporting

Completion reports name the revision, commands, fixtures or datasets,
hardware/display where relevant, results, failures, skipped checks, waivers,
and remaining risk. Performance claims also name the workload, metric,
sampling method, and threshold.
