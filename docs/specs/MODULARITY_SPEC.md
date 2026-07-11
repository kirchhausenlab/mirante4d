# Modularity Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define the expected isolation, modularity, and file-organization discipline for Mirante4D.

## Scope

This spec covers:

- module boundaries
- file responsibility
- dependency direction
- extraction triggers
- anti-patterns to avoid

## Non-Goals

- Splitting code into tiny files for its own sake.
- Abstracting before a real boundary exists.
- Creating generic frameworks inside the app.
- Preserving old code through compatibility layers.

## Requirements

- Each module should have one clear reason to change.
- Public interfaces should be typed and narrow.
- Side effects should be isolated from pure policy/math where practical.
- Cross-subsystem communication should go through explicit contracts.
- File names should describe domain responsibility, not vague utility buckets.
- Tests should be able to exercise pure logic without initializing the full app.

## Dependency Direction

Preferred direction:

```text
app shell
  depends on viewer runtime and UI contracts

viewer runtime
  depends on renderer contracts and data-engine handles

renderer
  depends on core math/types and renderer resource contracts

data engine
  depends on format and core types

preprocessing
  depends on format, core types, and storage/write contracts

format
  depends on core types and serialization primitives

core
  depends on minimal external crates only
```

Avoid circular dependencies. If two modules need each other, extract the shared contract or pure type into a lower-level module.

## File Size And Responsibility Guidance

There is no strict line limit, but large files require scrutiny. A file is suspect when it:

- cannot be summarized in one sentence
- contains multiple unrelated state machines
- mixes UI, I/O, parsing, and rendering logic
- requires broad integration setup to test simple rules
- attracts unrelated edits from multiple feature areas

When this happens, split by domain stage or responsibility.

## Acceptable Abstractions

Add an abstraction when it:

- expresses a stable domain concept
- isolates side effects
- allows independent testing
- prevents cross-subsystem coupling
- removes meaningful duplication

Do not add an abstraction just to make code look abstract.

## Anti-Patterns

- `utils` or `helpers` modules with unrelated functions.
- Monolithic app state objects passed everywhere.
- Renderer code opening files directly.
- UI code parsing binary dataset structures.
- Data engine code importing UI types.
- Hidden global mutable state.
- Broad trait objects where concrete types would be clearer.
- Compatibility/fallback branches kept without explicit user request.
- Dead code kept for possible future use.

## Testing Requirements

- Pure policy/math modules should have direct unit tests.
- Subsystem boundaries should have integration tests.
- Dependency direction should eventually be checked by architecture tests or lint rules.
- Large refactors should preserve or improve test granularity.

## Open Questions

- Whether to define warning thresholds for file length or module complexity.
