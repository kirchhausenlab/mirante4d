# First App Workflow Specification

Status: ACCEPTED
Last updated: 2026-06-12

## Purpose

Define the first coherent user workflow for the native Mirante4D app.

This spec describes the first coherent native app workflow. `CURRENT_STATE.md`
defines the overall implementation status.

## Scope

This spec covers:

- launch behavior
- open/import entry points
- first viewer state
- default projection/render mode
- core controls
- save/project behavior
- error handling

## Non-Goals

- Marketing or landing-page UI.
- Public AWS experiment loading.
- Browser OPFS behavior.
- Full Fiji/Napari-scale analysis parity in this first workflow spec.
- Legacy dataset loading.

## Launch Behavior

The app launches into the workbench shell.

Product empty-state actions:

- open native dataset/project
- import source data

This is not a marketing landing page. It is the empty state of the actual workbench.

Developer automation:

- `cargo xtask run-dev` launches the app with `basic-u16-16cube.m4d`
- this is developer automation, not user-facing product behavior

Current implementation includes native dataset open, `.m4dproj` project open/save,
autosave recovery, and a strict TIFF/OME-TIFF import workflow.

## Open Native Dataset

Flow:

1. user chooses a `.m4d` dataset or `.m4dproj` project
2. app runs quick validation
3. app shows concise metadata summary
4. app opens viewer if validation succeeds
5. app shows actionable errors if validation fails

Invalid datasets do not enter the viewer.

## Import Source Data

The import action opens the workflow defined in `IMPORT_PREPROCESSING_WORKFLOW_SPEC.md`.

Successful import opens the generated `.m4d` dataset in the viewer.

Current implementation presents strict TIFF/OME-TIFF file and directory import setup steps for source, output, detected source dtype, OME voxel-spacing metadata status, editable file grouping, dataset name, voxel spacing, channel names, and channel colors, then runs import as a background app task with progress/status display and cancellation. Complete OME `PhysicalSizeX/Y/Z` metadata with explicit convertible units pre-fills the reviewed voxel-spacing fields, but the app still requires explicit voxel-spacing review before import can start.

## First Viewer Defaults

For 3D intensity datasets:

- render mode: `MIP`
- projection: `Orthographic`
- active timepoint: first timepoint
- visible layers: first intensity channel visible; additional channels visible only when metadata/default policy says they should be
- display window: robust percentile-based default from dataset metadata

The current app implementation stores the active layer `LayerDisplay` in app state and applies its visibility, display window, and opacity when converting the rendered `u16` frame into the GUI texture. Per-frame auto-normalization is not the app display contract.

For 2D datasets:

- projection: `Orthographic`

Current implementation treats single-z-plane datasets as 2D for first-view defaults:
in `MIP` with orthographic projection.

Rationale: orthographic avoids scientific distortion by default, while MIP gives fast first inspection for 3D fluorescence data. Perspective and DVR remain important user-selectable modes.

## Workbench Regions

Top toolbar:

- open/import/save
- import runs as a background task; conflicting open/save/import actions are disabled while it is active
- active import exposes a cancellation command
- dataset/project name
- render mode
- projection mode
- reset view
- diagnostics indicator

Left panel:

- dataset tree
- layers/channels
- visibility
- color
- display window summary

Right panel:

- render settings
- selected object inspector
- dataset metadata
- GPU/data-engine diagnostics

Bottom strip:

- time slider
- playback controls
- progress/status
- warnings/errors summary

Center:

- primary viewport

## Basic Interactions

First viewer milestone should support:

- pan
- zoom
- rotate for 3D
- reset view
- switch projection
- change timepoint when multiple timepoints exist
- toggle layer visibility
- inspect voxel coordinates and scientific intensity/label values through typed picking/readout

Controls must be typed actions, not direct widget mutation of renderer/data state.

Current implementation routes core viewer/workbench controls through typed
`WorkbenchCommand` actions before state changes are applied. Covered controls
include render mode, projection, reset view, layer selection, timepoint changes,
and viewport orbit/pan/zoom gestures. Lower-level editing panels may still own
local draft widget state, but committed viewer behavior should continue moving
through typed domain commands.

## Project Save Behavior

Opening a `.m4d` alone creates an unsaved session.

Current implementation tracks project dirtiness from the state that would be
written to `.m4dproj`. Opening a native `.m4d` starts with no current project
path and a clean in-memory baseline; viewer, display, scene, or analysis changes
make the project dirty. Opening a `.m4dproj` or saving a project sets the current
project path and marks the written state clean.

User can save a `.m4dproj` project to persist:

- dataset reference
- display settings
- camera/projection/render mode
- timepoint
- future ROIs/measurements/analysis artifacts

Closing with dirty project state prompts the user to save, discard, or cancel close.
The close prompt cancels the native close request until the user chooses one of those
three outcomes. Saving writes to the current project path when one exists, otherwise it
asks for a project package path.

## Error Handling

Errors must be concise and actionable.

Examples:

- invalid format: "This is not a Mirante4D v1 dataset."
- missing required metadata: name the missing field and dataset path
- corrupt payload: identify layer/timepoint/scale/chunk when available
- GPU unavailable: show adapter/backend diagnostics and what feature failed
- budget too small: show required minimum work-unit size and current budget

Detailed diagnostics should be available without overwhelming the normal workflow.

## Invariants

- First screen is the workbench, not a website-like landing page.
- Invalid datasets do not enter the viewer.
- Orthographic projection is the default inspection projection.
- MIP is the default 3D first-view render mode.
- Empty/missing data is not rendered as valid empty signal.
- Project save state is separate from dataset package state.
- UI controls issue typed commands.

## Failure Modes

- empty state grows into marketing UI
- invalid dataset opens partially
- first frame blocks indefinitely
- projection switch warps orthographic view
- panel changes resize viewport unpredictably
- dirty project state is lost silently
- diagnostics are either hidden or overwhelming

## Testing Requirements

- app launch smoke test
- empty workbench screenshot/layout test
- open valid fixture flow
- reject invalid dataset flow
- first viewer defaults test
- projection switch interaction test
- timepoint control test
- dirty project close prompt test
- error message snapshot tests

## Open Questions

- Exact visual theme tokens.
- Exact keyboard shortcuts.
