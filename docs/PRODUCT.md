# Product

Mirante4D is a native desktop application for exploring and analysing large 4D
microscopy datasets: three-dimensional volumes over time, often with multiple
intensity channels and derived scientific results.

## Users And Outcomes

The primary users are microscopy researchers and biologists. Routine viewing
should not require knowledge of GPU APIs, storage layouts, or command-line
tools.

Mirante4D aims to:

- open local microscopy data reliably;
- navigate space, time, and channels interactively;
- render intensity data with scientifically explicit controls;
- work beyond RAM and VRAM through bounded streaming and multiscale storage;
- record analysis inputs, outputs, and provenance; and
- preserve source data and report incomplete or unsupported states honestly.

## Product Principles

- Native local desktop software, not a browser or server service.
- One bounded product path for small and large datasets.
- Strict validated inputs rather than permissive guessing.
- Explicit CPU, memory, storage, GPU, and I/O budgets.
- Reproducible scientific operations and typed persisted state.
- Hard cutovers while the project remains pre-alpha.

The source is public, MIT-licensed academic software. Full microscopy-data
publication and external dataset contribution are separate future decisions.

## Current Scope

The prototype includes import/preprocessing, streaming native data,
MIP/DVR/ISO intensity rendering, multichannel display, project state, exact
whole-layer and numeric box intensity analysis, a table/plot results workspace,
and Linux packaging.
[Current state](CURRENT_STATE.md) is the sole authority for exact implemented
behavior and limitations.

Segmentation is not part of the current product or foundation program. Any
future return requires a separately approved post-foundation design.

## Non-Goals

- Browser, cloud, or server deployment.
- Compatibility with `llsm_viewer` architecture or formats.
- A generic reader for arbitrary OME-Zarr datasets.
- Silent repair of malformed or underspecified inputs.
- Stable persisted-format compatibility during pre-alpha development.
- Paid hosted CI or a public self-hosted runner.
- Platform claims beyond qualified Linux x86_64/Vulkan work.
- 4K foundation qualification.

Mirante4D carries forward useful product ideas from
[llsm_viewer](https://github.com/kirchhausenlab/llsm_viewer) while replacing
its browser architecture. Related research code is available in
[SpatialDINO](https://github.com/kirchhausenlab/spatialdino).
