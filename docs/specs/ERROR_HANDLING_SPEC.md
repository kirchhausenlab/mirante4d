# Error Handling Specification

Status: DRAFT
Last updated: 2026-06-12

## Purpose

Define how Mirante4D reports and handles failures.

## Scope

This spec covers user-facing errors, developer diagnostics, validation failures, I/O failures, GPU failures, and job cancellation.

## Non-Goals

- Silent compatibility fallbacks.
- Hiding validation failures to keep the app running.
- Panic-driven normal error handling.

## Requirements

- Errors must identify the failing operation.
- Dataset validation errors must identify the invalid field/path where practical.
- I/O errors must include the relevant path or shard identifier.
- GPU errors must include adapter/backend context where available.
- User-facing text should be concise and actionable.
- Developer diagnostics should preserve structured details.
- Cancellation is not a failure and must be represented distinctly.

## Error Categories

- configuration error
- dataset validation error
- source data validation error
- I/O error
- decode/decompression error
- preprocessing error
- GPU initialization error
- GPU resource error
- renderer pipeline error
- cancellation
- internal invariant violation

## Invariants

- Unsupported format is a hard error.
- Missing required metadata is a hard error.
- Corrupt data is a hard error.
- Incomplete residency is a visible runtime state, not false empty data.
- Internal invariant violations should be loud during development.

## Failure Modes

- repeated read failure
- invalid cache state
- partial preprocessing output
- adapter lost/device lost
- out-of-memory
- unsupported hardware feature

## Testing Requirements

- Error construction tests for validation paths.
- User-facing message snapshot tests.
- Corrupt fixture tests.
- Cancellation propagation tests.
- Device/adapter failure simulation where practical.

## Open Questions

- How much structured error data is persisted in logs.
- Whether user-facing messages are localized.
