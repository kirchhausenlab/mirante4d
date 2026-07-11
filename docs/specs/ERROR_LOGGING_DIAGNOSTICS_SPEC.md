# Error, Logging, And Diagnostics Specification

Status: DRAFT
Last updated: 2026-06-12

## Purpose

Define the Rust error-handling, panic, logging, and diagnostics stack policy.

## Scope

This spec covers library errors, app boundary errors, panics, logs, structured diagnostics, and user-facing reporting.

## Non-Goals

- Swallowing errors to keep workflows moving.
- Panic-driven normal control flow.
- Network telemetry by default.
- Logging raw data payloads.

## Requirements

- Library crates should expose structured typed errors.
- App and developer-tooling boundaries may use broader error wrappers for context.
- User-facing errors should be concise and actionable.
- Developer diagnostics should preserve structured context.
- Logs should use structured spans for long-running operations.
- Startup/runtime diagnostics should include OS and GPU adapter/backend details.

## Recommended Stack

Current provisional direction:

- `thiserror` for library error enums.
- `anyhow` or equivalent only at app, developer-tooling, or test orchestration boundaries.
- `tracing` for structured logging and spans.
- `tracing-subscriber` or equivalent for local log output.

These are governed by `DEPENDENCY_POLICY_SPEC.md`.

Current implementation should use typed errors for format, data-engine, renderer, import, and analysis failures, with broad wrappers only at app and `xtask` boundaries.

## Panic Policy

- No `unwrap()` or `expect()` in runtime paths unless proving a local invariant with a useful message.
- Panics are acceptable in tests.
- Panics are acceptable for impossible internal invariants only when returning a normal error would hide a bug.
- Unsupported datasets are validation errors, not panics.

## Invariants

- Cancellation is not an error.
- Unsupported format is a hard validation error.
- Missing required metadata is a hard validation error.
- Corrupt data is a hard validation error.
- User-facing error text must not require reading logs to understand the next action.

## Failure Modes

- error loses path/context
- panic in user workflow
- logs too verbose to use
- logs expose unnecessary user data
- diagnostic object missing hardware context

## Testing Requirements

- Error conversion tests for key layers.
- User-facing message snapshot tests.
- Panic-free invalid dataset tests.
- Log/diagnostics schema tests.

## Current Implementation Status

Implemented startup and runtime diagnostics:

- App startup emits a structured `startup diagnostics` log event with app version, target OS, target architecture, and target family.
- `AppState` carries typed startup diagnostics for UI/runtime inspection.
- Startup diagnostics declare the current diagnostics format identity, `mirante4d-diagnostics-v1`.
- The runtime diagnostics panel shows diagnostics format, app version, platform, selected backend, and GPU adapter summary.
- GPU adapter summaries include backend, device type, adapter name, driver, and driver info when a GPU renderer is available.
- GPU initialization failures are recorded in the same diagnostics surface instead of disappearing into logs.

Tests:

- `cargo test -p mirante4d-app startup_diagnostics -- --nocapture`

## Open Questions

- Whether to collect crash reports locally.
- Whether to persist user-exportable diagnostics bundles separately from logs.
