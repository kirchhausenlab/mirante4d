# ADR-0010: Use Proportional Development Verification

Status: ACCEPTED AND IMPLEMENTED
Date accepted: 2026-07-26
Last reviewed: 2026-07-26
Supersedes: the six-leaf topology, universal routine-test assignment, and
fifteen-minute ceiling in ADR-0005

## Context

The foundation refactor corrected serious architecture, scientific, storage,
and persistence defects. Its verification system then expanded beyond the
needs of a one-maintainer academic application. Ordinary changes repeatedly
compiled test targets, ran exhaustive crash and process matrices, maintained
empty doctest inventory, and carried qualification provenance unrelated to the
changed product boundary.

The resulting pull-request job approached its fifteen-minute ceiling and local
iteration became slow, while a fully green suite still failed to expose
obvious viewer behavior. More evidence volume did not produce better product
feedback.

## Decision

- Keep the required public check identities `PR / policy` and `PR / rust`, the
  `$0` hosted-compute rule, least-privilege workflow constraints, and zero
  automatic retries.
- Use five public leaves: `policy`, `lint`, `unit`, `contract`, and `ui`.
  Remove the empty doctest leaf and its generated inventory.
- Lint ordinary library and binary targets. Test targets are compiled once by
  the test runner rather than first being compiled again by Clippy.
- Keep cheap unit/property coverage and a small representative integration set
  in routine PR verification. Put exhaustive transition, process-crash, and
  redundant long product variants in explicit affected-boundary local lanes.
- Use four verification tiers: focused edit-loop checks, the public merge
  checkpoint, changed-boundary local checks, and deliberate release or
  claim-bearing qualification.
- Require real-product validation for rendering, loading, interaction, GPU,
  and large-data changes. Automated breadth does not substitute for opening
  and inspecting the normal application.
- Add verification or provenance machinery only for a named scientific,
  user-data, security, product, or release risk. It must not grow faster than
  the product change it protects without explicit owner approval.
- Target five to eight minutes for the uncached Rust job and enforce a
  ten-minute hard ceiling.

## Retained Boundaries

This decision does not weaken independent scientific expectations, source
nonmutation, atomic create-only publication, bounded and cancellable work,
per-object storage integrity, project/package durability, dependency
direction, or release-artifact integrity.

Hashes remain appropriate for durable identities and trust boundaries. They
are not a default requirement for ordinary diagnostic output, internal
checkpoint chains, per-role executables, or test-result receipts.

## Consequences

- Developers receive faster feedback and can observe the actual product
  earlier.
- Deep fault coverage remains available when its owned boundary changes, but
  it no longer taxes unrelated pull requests.
- Routine verification intentionally samples some integration behavior instead
  of attempting exhaustive coverage.
- A green PR establishes only its named automated checks. Visible product and
  performance claims still require the appropriate higher tier.
- Historical foundation and qualification records remain revision-bound in Git
  history; ordinary changes do not reproduce them.

## Enforcement

- [Testing](../TESTING.md) owns tier definitions and changed-boundary triggers.
- `verification/registry.json` owns public selectors and explicit local-only
  cases.
- Workflow audits retain the two required identities, public-runner and
  permission rules, lack of retries/caches/artifacts, and the ten-minute Rust
  timeout.
- [Current state](../CURRENT_STATE.md) records the implemented cutover, and
  [current work](../planning/NOW.md) records the next product checkpoint.
