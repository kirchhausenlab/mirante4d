# Configuration Specification

Status: DRAFT
Last updated: 2026-06-11

## Purpose

Define configuration surfaces for Mirante4D.

## Scope

Configuration includes:

- app preferences
- dataset/session settings
- renderer/runtime budgets
- preprocessing options
- diagnostics flags

## Non-Goals

- Hidden compatibility flags.
- Environment-variable-only configuration for normal users.
- Silent feature downgrades.

## Requirements

- Defaults must be safe and documented.
- Performance budgets must be visible in diagnostics.
- Dataset/session settings must be separate from global app preferences.
- Developer/benchmark flags must be clearly separated from user settings.
- Configuration that changes output format must be recorded in preprocessing metadata.

## Candidate Configuration Areas

- CPU memory cache budget.
- GPU memory/resource budget.
- preprocessing worker count.
- compression codec/level.
- chunk/brick policy.
- log level.
- diagnostics capture.
- default viewer controls.

## Current Implementation Status

Implemented runtime configuration and preferences:

- `mirante4d-data` has a typed `DataRuntimeConfig`.
- Dataset opening uses safe documented default data-runtime budgets.
- Tests pin the default budget values and custom runtime-config plumbing.
- Runtime diagnostics expose configured data budgets and current cache/request counters in the app.
- Brick worker count and queue capacity are visible in app runtime diagnostics.
- The desktop app persists user preferences as JSON with format tag `mirante4d-preferences-v1`.
- Default preference paths are OS-specific:
  - Linux: `$XDG_CONFIG_HOME/mirante4d/preferences.json`, or `$HOME/.config/mirante4d/preferences.json`.
  - macOS: `$HOME/Library/Application Support/Mirante4D/preferences.json`.
  - Windows: `%APPDATA%\Mirante4D\preferences.json`.
- The app exposes runtime volume-cache and decoded-brick-cache budgets in the Settings section.
- Saved runtime budgets apply to datasets opened after the settings file is saved.
- When no preference file exists, app startup uses the system-RAM policy for the decoded-brick-cache budget where RAM can be detected; otherwise it uses the deterministic unknown-RAM fallback.
- Preprocessing options that affect native output are recorded in import/native dataset metadata, not in global runtime preferences.

Runtime-only configuration must remain runtime-only. It must not mutate dataset contents or preprocessing metadata.

## Invariants

- No hidden legacy mode.
- No config flag that silently opens unsupported old datasets.
- Preprocessing options that affect data must be stored with the output.
- Runtime-only settings must not mutate dataset content.

## Failure Modes

- invalid config file
- unsupported option value
- budget below minimum viable unit
- incompatible preprocessing option combination

## Testing Requirements

- Config parse tests.
- Default value tests.
- Invalid value tests.
- OS preference-path tests.
- Settings UI exposure and persistence tests.
- Preprocessing metadata recording tests for import options that affect native output.

## Open Questions

- Which GPU/VRAM limits should become user preferences rather than diagnostics-only values.
- Whether advanced preprocessing defaults deserve global presets after the importer workflow stabilizes.
