# Analysis Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the current viewer-centered analysis workbench contract.

## Scope

This covers inspection, annotation, drawing, measurements, tables, plots,
exports, provenance, execution classes, and CPU/GPU policy for analysis
workflows.

## Non-Goals

- replacing dedicated external image-analysis environments
- treating display color as scientific source data
- forcing full-dataset residency before analysis
- hiding approximate/multiscale results as exact measurements

## Capability Layers

- Inspection: source-value readout, coordinates, timepoint/channel/layer state,
  render-mode context, and completeness.
- Annotation and drawing: ROIs, notes, markers, and project-owned
  analysis artifacts.
- Measurement: view-local, ROI-local, object, and batch measurements with
  explicit units and data scope.
- Tables, plots, and export: typed result records, CSV/SVG exports, and
  reproducible provenance.

## Execution Classes

- View-local interactive work must stay responsive and may use current resident
  data only when the result is clearly scoped.
- ROI-local exact work may issue bounded reads through the data engine.
- Full-scope batch work must run as cancellable/background work with progress.
- Preview-then-finalize workflows must mark preview results as provisional.
- Multiscale approximate results must state scale, method, and limitations.

## Engine Responsibilities

- Use data-engine APIs rather than incidental renderer residency.
- Preserve source dtype semantics and use adequate numeric precision for each
  measurement.
- Carry spatial geometry, units, transform, timepoint, channel, layer, ROI, and
  algorithm provenance.
- Treat missing/incomplete data as incomplete, not zero.
- Keep exports deterministic and self-describing.

## UI Requirements

- Analysis tools live inside the native workbench and remain tied to the active
  dataset/project.
- Long-running work must show progress, cancellation, and result state.
- Results must distinguish exact, approximate, preview, failed, stale, and
  incomplete states.

## Failure Modes

- measurement from display RGB instead of source data
- approximate/multiscale result presented as exact
- missing data silently counted as zero
- analysis depends on renderer cache lifetime
- export lacks units, scope, or provenance

## Testing Requirements

Coverage must include source-value measurements, ROI geometry, spatial units,
object measurements, table/plot/export artifacts, cancellation, missing-data
handling, and deterministic provenance.
