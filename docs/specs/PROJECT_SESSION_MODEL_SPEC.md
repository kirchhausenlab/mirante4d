# Project And Session Model Specification

Status: ACCEPTED
Last updated: 2026-06-30

## Purpose

Define where mutable user work lives and how it relates to immutable native datasets.


## Scope

This spec covers:

- native dataset references
- project package layout
- saved session state
- analysis artifacts
- autosave/recovery
- dirty state and atomic writes

## Non-Goals

- Modifying source data silently.
- Storing normal viewer data in app-private OPFS-like storage.
- Supporting multi-dataset projects in the first implementation.
- Building a user-facing headless project format tool.

## Core Model

Mirante4D has two durable concepts:

- native dataset package: `.m4d`
- project package: `.m4dproj`

The `.m4d` dataset is the immutable viewing source produced by preprocessing.

The `.m4dproj` project stores user work, display settings, annotations,
measurements, analysis results, and workspace state.

Opening a `.m4d` dataset without a project creates an unsaved in-memory session. Saving creates a `.m4dproj` package.

Current implementation status:

- the app writes `.m4dproj/` directory packages with authoritative `project.json`
- the current project format is `mirante4d-project-v14`
- project packages create `artifacts/rois`, `artifacts/tracks`,
  `artifacts/measurements`, `artifacts/tables`, `artifacts/plots`, `autosave`,
  and `logs`
- project state persists a structured dataset reference, active layer/timepoint,
  per-channel display/render state, viewer layout, cross-section navigation,
  scene artifacts, and analysis tables/plots
- the dataset reference records dataset path, dataset ID, native dataset format, native schema version, and a BLAKE3 fingerprint of the canonical native manifest
- project and autosave manifests write the dataset path relative to the `.m4dproj` package root when that can be represented on the same filesystem root; otherwise they write the absolute path
- project and autosave reads resolve relative dataset paths against the `.m4dproj` package root before opening and validating the dataset
- opening a project validates the opened dataset identity, format, schema
  version, and manifest fingerprint before restoring layer, scene, or analysis
  state
- saving a project with analysis tables or plots writes typed payload files under `artifacts/tables/*.m4dtable.json` and `artifacts/plots/*.m4dplot.json`; `project.json` stores references, not embedded row/series payloads
- opening a project restores analysis tables and plots only after validating artifact format, referenced path, and artifact ID against `project.json`
- autosave recovery snapshots are written under `autosave/recovery.project.json`
- autosave analysis payloads live under `autosave/tables` and `autosave/plots`
- autosave read/write/recovery helpers exist in library and test paths, but the
  normal packaged runtime does not yet schedule snapshots or offer recovery
- writing an autosave snapshot does not create or update authoritative
  `project.json` and does not write into authoritative `artifacts/`
- opening autosave recovery uses the same strict project/session validation, then marks the restored state as recovery state in the workflow message
- file-based `.m4dproj` JSON sessions are rejected by the hard-cutover reader
- project and artifact JSON writes use temporary files, file flushes, JSON validation before commit, backup/restore replacement, and regression tests that force commit failure after the old file has moved to backup
- the workbench tracks the current project path and a clean project snapshot;
  viewer/session, scene, and analysis changes make the project dirty until a
  project save succeeds
- native close requests with dirty project state are cancelled and replaced by an in-app save/discard/cancel prompt; save uses the current project path when present and otherwise asks for a `.m4dproj` path
- if a project dataset path is missing, the workbench can ask the user to locate the referenced `.m4d`; the selected package must pass the same dataset ID, format, schema version, and manifest-fingerprint checks before project state is restored

## Project Package Layout

Initial package shape:

```text
experiment.m4dproj/
  project.json
  artifacts/
    rois/
    tracks/
    measurements/
    tables/
    plots/
  autosave/
  logs/
```

`project.json` is authoritative project metadata. Large derived artifacts may live under `artifacts/`.

## Dataset Reference

The project records:

- dataset path, preferably relative to the project when possible
- absolute path fallback
- dataset ID
- root manifest fingerprint
- format string
- schema version

If the dataset moves, the app may ask the user to locate it. It must validate the located dataset against the saved identity before using it.

Current implementation records a relative dataset path in project/autosave manifests when possible, resolves it on open, and validates identity against the resolved dataset before restoring state. If the referenced path is missing, project open offers a strict relocation flow: the selected replacement dataset must match the saved identity before restoration. Relocated projects are treated as dirty until saved because the authoritative `project.json` still references the old path.

## Saved State

Saved durable state:

- opened dataset reference
- layer visibility/color/window/opacity settings
- per-channel render mode and typed mode parameters
- volumetric render sampling policy
  saved as a nearest/linear policy
- projection mode
- ISO display level and ISO light state
- camera bookmarks
- current timepoint
- viewer layout: `single3d` or `four_panel`
- shared cross-section navigation state: `center_world`, normalized
  `orientation_xyzw`, `scale_world_per_screen_point`, and `depth_world`
- saved scene objects: ROIs, annotations, tracks, measurements
- analysis artifact records
- table/plot definitions
- workspace layout at a coarse level

Ephemeral state not saved as authoritative scientific state:

- hover
- active drag operation
- temporary text input
- transient progress
- current GPU residency
- current CPU cache contents
- transient error popovers after dismissal
- active cross-section panel
- panel viewport sizes and render-target sizes
- GPU texture IDs and display-frame identities
- scheduler generations, displayed generations, pending tickets, stream
  priorities, and product-automation diagnostics

## Undo, Redo, And Command History

Use typed commands for mutable project operations.

Undo/redo applies to:

- ROI edits
- annotation edits
- measurement object edits
- display-setting edits where practical

Long-running analysis jobs record provenance and result state, not every internal compute step.

## Autosave And Atomic Writes

All project writes must be atomic:

- write to temporary path
- flush where practical
- validate written metadata
- rename into place

Autosave policy:

- unsaved recovery snapshots may be written under `autosave/`
- autosave is for crash recovery, not a second source of truth
- restored autosave state must be clearly presented as recovery state

Source `.m4d` datasets and raw source data are not modified by project autosave.

## Dirty State And Close Handling

Dirty state is computed from a clean project snapshot rather than ad hoc widget flags.
The snapshot includes the saved `AppSession` representation. This keeps normal
viewer controls, scene edits, and analysis artifacts tied to the same state
that project save persists.

Opening a native `.m4d` creates an unsaved session with no current project path and a
clean baseline. Opening a `.m4dproj` records that project path as current and marks the
restored state clean. Successful project save records the target path as current and
marks the written state clean.

When the OS/window manager requests app close and the project is dirty, the workbench
sends `CancelClose` and shows an in-app prompt with exactly these choices:

- save
- discard
- cancel

Discard allows the close to proceed without writing. Cancel keeps the app open. Save
writes the current project path when available, otherwise asks for a `.m4dproj` package
path; close proceeds only after the save succeeds.

## Single-Dataset Initial Scope

The first project model supports one primary dataset.

Multi-dataset comparison/registration is a future feature and should not complicate the first project format.

## Invariants

- `.m4d` is immutable during normal viewing/analysis.
- `.m4dproj` stores mutable user work.
- Runtime caches are not durable project data.
- Project artifacts record source dataset identity.
- Atomic writes are required for project metadata and artifacts.
- Autosave does not silently become authoritative output.

## Failure Modes

- referenced dataset missing
- referenced dataset fingerprint mismatch
- project metadata corrupt
- artifact source identity mismatch
- save fails mid-write
- autosave recovery conflicts with manually saved project

## Testing Requirements

- project create/open/save tests
- dataset identity validation tests
- fingerprint mismatch rejection tests
- atomic write failure tests
- dirty-state tests
- autosave recovery tests
- command undo/redo tests

## Open Questions

- Exact artifact binary layouts.
- Whether project packages should later support optional single-file archives.
