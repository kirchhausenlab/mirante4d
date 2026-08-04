# ADR-0010: Use Proportional Development Verification

Status: Accepted

Accepted: 2026-07-26

Last reviewed: 2026-08-04

## Context

Routine verification once repeated compilation and broad qualification work
for changes that did not touch those boundaries. Local feedback became slow,
while a green suite still did not prove visible product behavior.

More evidence volume is not automatically better evidence.

## Decision

- Keep two required public check identities: policy and Rust.
- Use focused tests during development and the public merge checks for routine
  integration.
- Run expensive GPU, product, package, filesystem, power-cut, and exhaustive
  process checks only when their owned boundary changes.
- Require normal-product validation for rendering, loading, interaction, GPU,
  and large-data changes.
- Keep independent scientific expectations, data safety, durability, and
  release integrity even when their checks are local.
- Use zero automatic test retries.
- Add verification or provenance machinery only for a named scientific,
  user-data, security, product, or release risk.
- Keep the machinery proportionate to the product change unless the owner
  explicitly approves a broader system.

## Consequences

Developers get faster routine feedback and reach direct product observation
earlier. Deep fault coverage remains available without taxing unrelated pull
requests.

A green public check proves only its named automated boundary. It does not
prove visible behavior, hardware support, durability on an unqualified
filesystem, or a performance claim.

## Guardrails

- Do not weaken an affected-boundary check to fit the routine lane.
- Do not add retries, empty inventory, duplicate compilation, or qualification
  receipts without a concrete risk.
- Keep hosted work on free public runners with least privilege and no trusted
  hardware or private data.
- Keep historical qualification results bound to their revision.

[Testing](../TESTING.md) owns current tiers, commands, and triggers.
