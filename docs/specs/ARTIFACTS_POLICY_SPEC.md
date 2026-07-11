# Artifacts Policy Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define repository hygiene for generated files, sample data, benchmark outputs, and large artifacts.

## Scope

This spec covers `.gitignore` expectations, generated native datasets, local sample data, benchmark outputs, screenshots, logs, and curated fixtures.

## Non-Goals

- Committing large real microscopy datasets to the repo.
- Hiding important golden fixtures outside version control.
- Treating benchmark noise as source truth.

## Requirements

- Large generated data must be ignored by default.
- Tiny deterministic golden fixtures may be committed.
- Local sample data lives outside the repo and is addressed through `MIRANTE4D_SAMPLE_DATA`.
- Benchmark outputs should be ignored unless intentionally curated.
- Failure artifacts may be stored temporarily but should not be committed accidentally.
- `.gitignore` should be updated before generating new artifact classes.

## Artifact Classes

- Source code and docs: committed.
- Tiny golden fixtures: committed when stable and useful.
- Generated native datasets: ignored by default.
- Local real sample data: external.
- Benchmark raw outputs: ignored by default.
- Curated benchmark summaries: committed only when useful and small.
- Screenshots/render artifacts: ignored by default, committed only as stable visual fixtures.
- Logs: ignored.
- Build outputs: ignored.

## Invariants

- Do not commit `MIRANTE4D_SAMPLE_DATA` contents.
- Do not commit large generated datasets accidentally.
- Do not rely on ignored local artifacts for normal fast tests.
- CI fixtures must be explicit and reproducible.

## Failure Modes

- huge binary committed
- test depends on unavailable local file
- benchmark report committed without context
- generated output overwrites fixture

## Testing Requirements

- `xtask` or CI should eventually check for oversized files and forbidden artifact paths.
- Fixture tests should fail clearly when required fixture generation is missing.
- Benchmark reports should include schema/version/context before being curated.

## Open Questions

- Size threshold for committed fixtures.
- Curated benchmark output directory.
- Visual fixture update workflow.
