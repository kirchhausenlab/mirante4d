# ADR-0009: Keep One Canonical Product Model

Status: Accepted

Accepted: 2026-07-11

Last reviewed: 2026-08-04

## Context

Durable scientific facts, project state, runtime resources, UI interaction,
diagnostics, storage details, and renderer state have different lifecycles.
Combining them in one application object creates duplicated authority and
makes persistence and stale-result handling unsafe.

## Decision

- mirante4d-domain owns validated framework-neutral scientific and view values.
- mirante4d-identity owns strict typed identity parsing and descriptors. It
  does not claim scientific conformance.
- mirante4d-project-model owns one validated durable project state. It owns no
  payloads, live tasks, renderer state, or UI state.
- Use stable logical-layer keys as identity. Names, positions, and storage
  channel numbers are not substitutes.
- Keep dataset locator hints optional and separate from identity.
- Keep artifact references closed, typed, and versioned.
- Keep durable project, transient application, transient UI, runtime,
  diagnostic, and settings facts in separate classes.
- Let only the application reducer change durable model authority.
- Admit asynchronous results only when their typed identity and currentness
  still match.
- Keep the project model independent of serialization, filesystems, I/O,
  renderer, runtime, UI, and GPU types.

## Consequences

Persistence receives a semantic durable projection instead of a snapshot of
the running application. Runtime caches and workers cannot become accidental
project data. Scientific identity, exact package identity, and reopening
location remain distinct.

The application needs explicit adapters and validation at ownership
boundaries. That cost is accepted to avoid synchronized duplicate models.

## Guardrails

- Do not add a second durable model, compatibility DTO authority, or
  index/name identity fallback.
- Validate a durable change before mutation and reject it atomically.
- One effective durable change advances one revision. No-ops and transient
  changes do not make the project dirty.
- New durable concepts need an explicit canonical owner and bounded
  validation.

[Architecture](../ARCHITECTURE.md) owns the current package and state flow.
