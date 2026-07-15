# ADR-0005: Use Six Verification Leaves, Two Zero-Cost Checks, And Trusted-Local Evidence

Status: ACCEPTED AND IMPLEMENTED
Date accepted: 2026-07-10
Last reviewed: 2026-07-15
Decision IDs: D-022, D-023
Current-state effect: SIX-LEAF TOPOLOGY AND TWO ZERO-COST CHECKS ACTIVE

This ADR did not independently authorize a test rewrite, fixture generation,
hosted run, runner registration, workflow change, branch rule, cache, artifact
upload, or production implementation. WP-04 separately installed the former
transitional `Bootstrap / required` gate. WP-06 implemented the six-leaf
topology, calibrated and installed the two required checks, then removed that
bridge. [Current State](../CURRENT_STATE.md) remains the authority for current
commands, tests, workflows, reports, and evidence.

## Context

The predecessor recursive verification stack duplicated work, mixed proof
classes, depended on ignored/name-based selection, and could let stale reports
or incomplete execution look authoritative. It was too slow for useful pull-
request feedback yet still could not establish independent format, scientific,
GPU, packaged-product, or performance claims.

The replacement must keep public CI genuinely free and safe for untrusted
contributions while moving machine-specific evidence to the trusted local
workstation. It must also distinguish independent conformance facts from
self-agreement and use failure/statistical rules that cannot turn missing or
unstable proof green.

## Options Considered

1. Repair the current `verify-fast`/`full`/`nightly` recursion and report stack.
2. Put all cases in one universal fast lane or make every lane a required
   hosted check.
3. Register the GPU workstation as a public self-hosted runner.
4. Use automatic retries or quarantine to preserve green required checks.
5. Treat Mirante writer/reader round trips, CPU/GPU agreement, or noisy hosted
   timing deltas as conformance and performance authority.
6. Use six nonrecursive public leaves aggregated into two required checks, with
   independent fixtures and trusted-local product/scientific evidence.

## Decision

- The six independently selectable leaves are `policy`, `lint`, `unit`,
  `contract`, `ui`, and `doctest`. Every normal case has exactly one primary
  owner and lane. Leaves never invoke aggregates or recursively rerun another
  leaf.
- The only required public PR checks are `PR / policy` and `PR / rust` on a
  named standard public `ubuntu-24.04` runner. The Rust job shares one test-
  binary build only across `unit`, `contract`, and `ui`; lint and doctest remain
  honest separate phases. The uncached PR critical-path target is p95 below ten
  minutes over at least twenty qualifying runs, with a fifteen-minute hard
  ceiling.
- Public CI starts cache-free and artifact-free, uses no paid/larger or public
  self-hosted runner, and remains bounded by organization `$0` stop-usage,
  least-privilege, pinned-action, fork-approval, storage, and retention rules.
  Exhausted free capacity blocks a lane; it does not authorize spending or a
  bypass.
- `HW-2` is never attached to the public repository. Trusted GPU, performance,
  T5, scientific, and E4 evidence runs locally from a clean worktree at an
  immutable commit/tag. Dependencies are prefetched, T5 is read-only, scratch
  is isolated, and the tested process receives no upload/status credential. A
  later trusted step sanitizes and binds small evidence manifests to exact
  inputs, environment, package, revision, and artifact digests.
- T1 is the portable conformance authority. Fixture-byte producer, expected-
  fact oracle, and independent reader are pairwise-independent implementation
  lineages for authoritative facts. Production code cannot generate or bless
  its own oracle. T2 broadens properties and pressure coverage but is support
  evidence, not independent conformance authority.
- Storage evidence proves exact addressed-versus-actual outer-shard and total
  object counts, directory depth/fan-out, and one-brick read/decode
  amplification at the storage boundary. Any unbounded per-logical-brick file,
  sidecar, or manifest-record relation fails.
- Product evidence uses E0 through E4. E4 runs the exact packaged application on
  the real `HW-2` display through OS keyboard/mouse/window/file-dialog input and
  external window/compositor pixels; at least one required workflow is
  T1-backed. Gate at an externally observed `1280x720` and exercise the same
  candidate at `1920x1080`; there is no 4K scenario.
- Blocking performance runs only on fixed, fingerprinted `HW-2` with
  predeclared metrics. Tail gates use sixty independent trials/sessions with
  zero threshold violations and the stated one-sided Clopper-Pearson rule.
  Relative claims use at least twenty interleaved independent baseline/candidate
  pairs and the fixed paired bootstrap policy. Hosted-runner timing never gates
  product performance.
- Required cases have zero automatic retries. One manual rerun is allowed only
  for a recorded external infrastructure failure. Quarantine is visible,
  remediation-only, at most fourteen days, proves no requirement, enters no
  release evidence, and never unblocks missing proof. Native exit status and
  unique same-revision run manifests are authoritative; missing, skipped,
  cancelled, unsupported, stale, or incomplete evidence fails closed.

## Consequences

- Pull requests receive bounded, nonduplicated, no-cost feedback, while GPU,
  scientific, performance, and real-display claims require explicit maintainer
  operation on the trusted machine.
- Independent fixtures, readers, facts, and frozen harnesses cost more to build
  than self-round trips but can reveal shared production defects.
- Cache/artifact-free startup may expose a feasibility failure. The target must
  first remove duplication; any bounded cache or diagnostic-artifact exception
  requires a separate reviewed activation and demonstrated `$0` headroom.
- Performance gates take enough fixed-machine repetitions to support the named
  statistical claim. A noisy, censored, incomplete, or inconclusive sample is
  not silently converted into a pass.
- Flakes and missing assertions remain visible and can block closure; quarantine
  is a repair deadline, not a green status mechanism.
- The policy did not validate or replace any predecessor test or report until
  WP-06 implemented it.

## Enforcement

- A machine-readable verification registry owns lanes, selectors,
  requirements/assertions, owners, fixtures, capabilities, timeouts, evidence
  schemas, and retention. Generated selector/Nextest configuration must have a
  clean diff; discovery proves every case is assigned exactly once, and normal
  cases cannot hide behind `#[ignore]`.
- Required jobs are unconditional and uniquely named: no path filter,
  job-level skip route, `continue-on-error`, duplicate identity, retry, private
  data, GPU discovery, mutable service, or performance timing is permitted.
- The implementation handoff owned the D-022 bootstrap, twenty-attempt shadow
  window, transactional required-check replacement/readback, and separate
  bridge deletion. The sequence installed the two target names before removing
  the bridge; exact-revision product validation and the WP-06 exit tag remain.
- Workflow and repository-policy audits enforce standard public runners,
  read-only tokens, no PR secrets, full-SHA actions, no external reusable
  workflows initially, bounded retention, no initial cache/artifact upload,
  inspected inherited trust, and net `$0` billing.
- Fixture/oracle dependency audits, archive extraction bounds, immutable
  promotion manifests, exact physical-layout assertions, E4 external oracles,
  fixed-hardware statistical profiles, and fail-closed evidence assembly are
  release gates. A JSON report or persistent `target/` scan cannot override a
  failed native process or certify stale work.

## Owning Documents

- [Testing](../TESTING.md)
- [Development](../DEVELOPMENT.md)
