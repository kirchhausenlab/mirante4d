# Application Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the top-level behavior expected from the Mirante4D desktop application.

## Scope

This spec covers app-level responsibilities:

- native application shell
- project/session lifecycle
- dataset open and validation handoff
- viewer workspace orchestration
- preprocessing job orchestration
- analysis job orchestration
- project/session analysis artifacts
- high-level module boundaries

## Non-Goals

- Browser runtime.
- Static website deployment.
- VR/WebXR.
- Generic OME-Zarr viewer behavior.
- Backward compatibility with `llsm_viewer`.
- User-facing CLI tools.
- Headless, server, or batch-cluster product modes.

## Requirements

- The app must be installable and runnable as a native desktop application.
- The app must open only strict Mirante4D native datasets in the core runtime.
- The app must provide a clear path to preprocess/import raw data into the native format.
- The app must handle datasets larger than system memory by design.
- The app must surface hardware/GPU diagnostics clearly.
- The app must keep long-running preprocessing and loading work cancellable.
- The app must never overwrite source data without explicit user action.
- The app must distinguish user-facing errors from developer diagnostics.
- The app must use one canonical viewer data path for native datasets.
- The app must not offer public AWS experiment loading.
- The app must not expose OPFS-style app-private dataset loading as a product concept.
- The app must adapt to low-resource and high-resource machines through budgets and policies, not separate viewer implementations.
- The app must support viewer-centered analysis as a first-class product direction.
- The app must distinguish view-local previews, approximate results, and exact final analysis results.
- The app must not present partial analysis over currently loaded bricks as complete full-data output.
- The app must preserve analysis provenance for final derived artifacts.
- The app must model tracks, ROIs, annotations, measurements, hover, screen
  text, and reference visuals as typed scene layers or transient interaction
  state rather than ad hoc renderer overlays.

## Module Responsibilities

- App shell: window lifecycle, menus, global commands, session state.
- Viewer runtime: cameras, interaction state, playback state, tool state, scene-layer extraction inputs.
- Renderer: GPU resources, render passes, shaders, scene-layer draw lists, diagnostics.
- Data engine: dataset validation, chunk/shard reads, cache, prefetch, decompression.
- Preprocessing: raw import, spatial geometry metadata, normalization/display metadata, multiscales, acceleration metadata, output validation.
- Format: manifest and binary layout parsing/writing, schema validation.
- Analysis: inspection, annotation, measurements, plotting, statistics,
  exports, and provenance.
- Scene layers: typed tracks, ROIs, annotations, measurement visuals, interaction visuals, reference visuals, extraction, picking/query contracts.

The first concrete app workflow is defined in `FIRST_APP_WORKFLOW_SPEC.md`. Durable project/session state is defined in `PROJECT_SESSION_MODEL_SPEC.md`. Source import UX is defined in `IMPORT_PREPROCESSING_WORKFLOW_SPEC.md`.

## Invariants

- Unsupported datasets fail before entering the viewer.
- UI state must not bypass dataset validation.
- Renderer code must not read arbitrary files directly.
- Data engine code must not depend on UI widgets.
- Preprocessing output must be validated before it is offered for viewing.
- Small and huge datasets must use the same data-engine contracts.
- Analysis tools must use data-engine contracts rather than incidental renderer residency.
- Final analysis outputs must record scope and provenance.
- Preview and exact analysis outputs must not be conflated.
- Spatial data must not silently assume unit voxel spacing.
- UI panels must not directly create renderer resources for scene-layer objects.
- Scene-layer picking results must be typed and explicit about completeness.

## Failure Modes

- No compatible GPU adapter.
- Dataset format/version mismatch.
- Dataset validation failure.
- Missing or corrupt shard/index.
- Insufficient memory or VRAM for current view.
- Preprocessing cancellation.
- Preprocessing source data inconsistency.
- Analysis job cancellation.
- Analysis operation requests scope too large for current budgets.
- Analysis result cannot be completed because required data is missing or corrupt.

## Testing Requirements

- Smoke test: application initializes GPU and opens an empty diagnostic workspace.
- Dataset open tests: valid tiny native dataset opens; invalid format fails clearly.
- Cancellation tests: preprocessing and loading jobs can be cancelled without corrupting output.
- State tests: viewer cannot start from an invalid dataset handle.
- Analysis state tests: final analysis results cannot be produced from incomplete data.
- Analysis provenance tests once analysis artifacts exist.

## Open Questions

- Release cadence and non-Linux package order after the Linux first release path.
