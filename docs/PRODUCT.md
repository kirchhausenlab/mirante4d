# Product

Mirante4D is a native desktop application for exploring and analysing large 4D
microscopy datasets: three-dimensional volumes over time, often with multiple
channels and derived scientific results.

## Users

The primary users are microscopy researchers and biologists. Routine viewing
should not require knowledge of GPU APIs, storage layouts, or command-line
tools.

## Intended Outcomes

- Open local microscopy datasets reliably.
- Navigate space, time, and channels interactively.
- Render intensity data with scientifically explicit controls.
- Work with datasets larger than system memory through streaming and
  multiscale storage.
- Record analysis inputs, outputs, and provenance clearly.
- Preserve user data and report incomplete or unsupported states honestly.

## Product Principles

- Native desktop application, not a browser service.
- Strict validated inputs rather than permissive guessing.
- One runtime path for small and large datasets.
- Explicit CPU, memory, storage, and GPU budgets.
- Reproducible scientific operations and typed persisted state.
- Hard cutovers while the project remains pre-alpha.

## Current Scope

The current application includes import/preprocessing, streaming native data,
MIP/DVR/ISO rendering, multichannel display, project state, analysis tools, and
Linux packaging. See [CURRENT_STATE.md](CURRENT_STATE.md) for exact current
facts.

A previously attempted derived-label capability has been removed. Any future
return requires a new, separately approved design after the foundation work;
the lessons are preserved in a deferred capability record.

## Non-Goals

- Browser or server deployment.
- Compatibility with `llsm_viewer` data or architecture.
- A generic viewer for arbitrary OME-Zarr datasets.
- Silent repair of malformed or underspecified inputs.
- Stable persisted-format compatibility during pre-alpha development.
- Paid hosted CI or a public self-hosted runner.
- Platform support claims beyond verified Linux x86_64 builds.

## Project Lineage

Mirante4D carries forward useful product ideas from
[llsm_viewer](https://github.com/kirchhausenlab/llsm_viewer) while replacing
the browser architecture with a native implementation. Related research code
is available in [SpatialDINO](https://github.com/kirchhausenlab/spatialdino).
