# ADR-0009 — Freeze The Canonical Model Before Product Cutover

Status: ACCEPTED AND IMPLEMENTED BY WP-07A
Accepted: 2026-07-11
Last reviewed: 2026-07-14
Owning package: WP-07A
Product authority cutover: WP-07B

## Context

The prototype mixes durable project facts, runtime resources, UI interaction,
diagnostics, storage details, and duplicated active-layer state in one large
application object. Moving those fields directly would preserve accidental
coupling and make the product cutover impossible to review.

WP-07A therefore freezes the small canonical vocabulary and the complete
predecessor-field disposition before any product authority moves.

## Decision

- `mirante4d-domain` owns validated, framework-neutral scientific and view
  values. `LogicalLayerKey` is the scientific logical ordinal; storage channel
  numbers, names, and vector positions are never identity.
- `mirante4d-identity` strictly parses typed SHA-256 identities and object
  descriptors. It does not compute hashes or claim scientific conformance.
- `mirante4d-project-model` owns one validated `ProjectState`: one verified
  dataset reference, one workspace view, ID-keyed layer state and presets, and
  immutable artifact references. It owns neither payloads nor live tasks.
- Artifact references use a closed versioned schema enum whose media type and
  logical object role must match; no generic material or segmentation-capable
  artifact escape hatch exists.
- Dataset locator hints are optional reopening aids and never identity. The
  current product cannot attach/save/open through the canonical model until a
  later verifier supplies a real `ScientificContentId`.
- Project revisions are project-bound values. A persisted project-bound
  high-water allocator prevents sequence reuse after undo; the application
  reducer will own the live current revision, high water, and history in WP-07B.
- Set-like preset entries and artifact source-layer keys are canonicalized by
  logical key, while view layer order remains presentation-semantic. Explicit
  per-collection and aggregate nested limits bound validation and in-memory
  metadata work.
- Channel color is RGB. Channel compositing has one separate opacity value;
  DVR's intensity-to-opacity transfer is a distinct mode-specific value.
- Durable project, transient application, transient UI, runtime snapshot,
  derived diagnostic, and settings facts are separate classes. Only durable
  project facts enter a project generation.
- The project model is persistence-neutral: it exposes a semantic generation
  projection, with no serde, filesystem path, I/O, renderer, runtime, UI, or GPU
  types.
- Only a future typed application reducer may change durable authority. It must
  validate before mutation, reject atomically, advance one revision for one
  effective durable change, leave no-ops and transient changes clean, and admit
  asynchronous results only when typed identity and currentness still match.

Current dependency, side-effect, state-class, and semantic invariants live in
the code and [current architecture](../ARCHITECTURE.md). Git history and the
immutable exit tags preserve the exact cutover signatures and predecessor
disposition.

## Consequences

The three crates were accepted at
`5383cbb93c13c59e6f035bfa551356c75fb426dc`
(`foundation-wp-07a-exit-1`). WP-07B-B made them the sole live product
authorities and deleted predecessor durable mirrors in the same hard cutover;
there is no synchronized second model, compatibility DTO, re-export facade,
or index/name fallback.

Canonical preimage encoding, hashing, scientific-content verification,
project-store wire DTOs, and I/O remain owned by their later packages. Strictly
parsing an identity string is not a T1 conformance claim.

## Enforcement

- Cargo dependency policy keeps all existing product crates independent of the
  three preparatory crates during WP-07A.
- Architecture checks enforce zero external side effects, exact dependency
  allowlists, the frozen contract schema, and exact coverage of all 152 current
  application fields.
- Pure unit and fixed-seed property tests prove value validation, typed identity
  separation, ID-stable selection, project closure, and revision invariants.
- WP-07B-B is the hard cutover. Product-open validation becomes mandatory
  there because that checkpoint changes live state and interaction behavior.

## Owning Documents

- [Current Architecture](../ARCHITECTURE.md)
