# Quality Assurance Specification

Status: DRAFT
Last updated: 2026-06-12

## Purpose

Define the required quality assurance behavior for Mirante4D development.

## Scope

This spec covers:

- verification expectations for implementation work
- quality gates
- regression testing
- final-response evidence
- handling unverified behavior

## Non-Goals

- Using the project owner as the first tester.
- Claiming correctness from code inspection alone.
- Treating tests as optional polish.
- Hiding unverified behavior.

## Requirements

- Every feature must have tests at the appropriate level.
- Every bug fix must include a regression test unless automation is not yet possible.
- Every final implementation response must report the checks that were run.
- High-risk changes must run broad verification gates before being called complete.
- Renderer changes must include visual, pixel, or resource-state verification where practical.
- UI changes must include screenshot, layout, or interaction verification where practical.
- Dataset format changes must include golden validation tests.
- Performance-sensitive changes must include benchmark evidence or a documented benchmark follow-up.

Concrete testing tools and gate composition are defined in `TESTING_TOOLING_SPEC.md`.

## Verification Levels

- Unit: local pure logic.
- Golden: serialized format and binary layout stability.
- Integration: subsystem boundaries.
- GPU/render: nonblank and behavior-specific render validation.
- UI/visual: screenshot, layout, overflow, and interaction validation.
- End-to-end: real user workflows.
- Stress: cache, cancellation, corruption, resource pressure.
- Benchmark: performance claims and regressions.

## Invariants

- "Implemented" means verified.
- Unsupported data must fail clearly.
- Tests must exercise the behavior being claimed.
- Unverified behavior must be disclosed.
- Regression-prone code must gain regression tests.

## Failure Modes

- tests pass but do not cover the changed behavior
- GPU tests unavailable in local environment
- UI screenshots unavailable or unstable in local environment
- e2e tests too slow for normal iteration
- benchmarks noisy without hardware context
- test fixture drift

## Testing Requirements

- The repository must provide named verification gates.
- Test fixtures must be deterministic.
- GPU diagnostics must be recorded for render failures.
- UI visual failures should preserve screenshots and layout diagnostics where practical.
- E2E failures must preserve logs and screenshots where practical.
- Benchmarks must record hardware and dataset context.

## Open Questions

- Golden fixture storage policy.
- Cross-platform release gate requirements.
