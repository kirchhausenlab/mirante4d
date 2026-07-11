# UI/UX Specification

Status: ACCEPTED
Last updated: 2026-07-10

## Purpose

Define the expected native user experience for Mirante4D.

## Scope

This spec covers:

- high-level app workflows
- viewer workspace
- dataset open/import
- analysis tools and results
- diagnostics and errors
- control density and interaction expectations
- layout model
- app state versus UI state
- UI verification expectations

## Non-Goals

- Marketing website.
- Browser UI.
- VR-specific UI.
- Decorative landing page.
- Ad hoc debug UI.
- A direct copy of the legacy web viewer layout.
- UI that looks temporary, generated, or debug-only.

## Requirements

- The first screen should be useful, not promotional.
- Users should be able to open a native dataset or start preprocessing/import.
- Long-running jobs must show progress and remain cancellable.
- Viewer controls should be dense, organized, and suitable for repeated scientific work.
- Errors should be specific and actionable.
- Technical diagnostics should be available without overwhelming normal users.
- The main workbench must always show the currently displayed fidelity, including shown LOD, target LOD when different, completeness, backend, viewport pixels, render timing, and display freshness when known.
- When presentation size and render target size differ, the workbench must not
  collapse them into one ambiguous viewport label. It should report the
  displayed render target pixels and, where useful for diagnostics, the current
  presentation size separately.
- The shown LOD label must be literal: `shown sN` is the LOD currently visible in the viewport, while `target sM` is the pending/loading goal. If no complete frame is displayed yet, the UI must say so rather than pretending the pending scale is shown.
- The 2D/3D viewer viewport is the primary surface and should dominate the workspace.
- UI layout must be stable: changing one panel or control must not unpredictably misalign the rest of the app.
- Controls must come from a shared design system rather than one-off styling.
- UI state must be separated from durable application state and renderer/resource state.
- The interface must look professional, intentional, and internally consistent.
- Every important workflow should be reachable without requiring users to understand implementation details.

## Primary Workflows

- Open native dataset.
- Inspect dataset metadata before launch.
- Launch viewer.
- Adjust layer visibility, color, brightness/contrast, and per-channel render mode.
- Navigate timepoints and play time series.
- Inspect voxel/label values.
- View tracks and labels.
- Draw and inspect ROIs and measurement artifacts.
- Run analysis tools with clear preview/final states.
- View measurement tables, plots, and statistics.
- Export analysis results.
- Start preprocessing from source data.

## Workbench Layout

Mirante4D should use a viewer-first scientific workbench layout:

- center: large 2D/3D viewport
- left: dataset, layers, channels, and visibility controls
- right: selected object, render settings, metadata, and inspector panels
- bottom: time controls, playback, frame scrubber, and progress/status for temporal data
- top: compact toolbar, tool mode controls, project/app status, and diagnostics entry points

This is a starting layout model, not a pixel-locked mockup. The invariant is that the viewport remains the main work surface and all secondary panels have clear ownership.

The first concrete app workflow and default viewer state are defined in `FIRST_APP_WORKFLOW_SPEC.md`.

## UI Direction

Initial UI should prioritize:

- utility
- clarity
- predictable controls
- compact scientific panels
- clear status and diagnostics

Avoid:

- marketing-style hero layouts
- decorative UI
- excessive empty space
- inconsistent button/control styling
- giant headings inside tool panels
- arbitrary floating card layouts
- nested panels without clear purpose
- one-off spacing, colors, and widget styles

The product tone should be calm, precise, technical, and professional. Visual hierarchy should come from layout, spacing, typography, and grouping, not decoration.

## Fidelity Status UI

The fidelity status strip is required product UI.

It must be visible during normal viewing without opening diagnostics. It should be concise enough for the top or bottom workbench status area, and it must remain stable across high-DPI scaling and narrow-but-supported windows.

Required concise fields:

- shown LOD, formatted as `shown sN`
- target LOD when different, formatted as `/ target sM`
- completeness or budget/loading reason
- backend, such as `GPU resident`
- viewport physical pixels
- recent render time
- display freshness, such as `display current` or `display stale`, when enough
  metadata is available

Allowed examples:

- `shown s0 | complete | GPU resident | 1920x1080 | render 8.2 ms | display current`
- `shown s2 / target s0 | budget-limited: GPU | GPU resident | 1920x1080 | render 31.0 ms`
- `shown s1 / target s0 | loading target | GPU resident | 1280x720 | render pending | display stale`

Disallowed patterns:

- showing only `s0` when the current frame is rendered from `s2`
- labeling a coarse frame as exact because the target is `s0`
- hiding incomplete or budget-limited state in a diagnostics panel only
- implying missing occupied bricks are empty data
- reporting render time as actual interaction or presentation FPS unless that
  is what was measured

Detailed diagnostics may expand this with visible/resident/missing brick counts, CPU/GPU cache bytes, upload queue depth, adapter limits, and the last capacity error.

## UI Toolkit Direction

The current stack direction uses `egui`/`egui-wgpu` because it integrates well with the Rust + `wgpu` renderer and supports dense native scientific tools.

This does not mean accepting the default quick-debug UI appearance. Mirante4D should build an internal UI kit on top of the chosen toolkit:

- shared theme
- shared spacing and typography scale
- reusable controls
- consistent icons
- stable panel primitives
- standardized disabled/loading/error states
- screenshot and layout regression tests

Alternative UI toolkits may be reconsidered only through a decision record that evaluates renderer integration, cross-platform behavior, testing, accessibility, maintenance cost, and visual quality.

## State Model

UI code should distinguish:

- Durable app/project state: opened dataset, channels, render settings, timepoint, selected entities, saved annotations, analysis artifacts.
- Ephemeral UI state: panel open/closed state, hover, drag state, search text, temporary text input.
- Renderer state: GPU resources, resident bricks, pipelines, frame diagnostics.

Widgets should emit typed commands/actions instead of freely mutating unrelated global state. This keeps UI behavior testable and prevents layout code from becoming entangled with renderer/data-engine internals.

Scene-layer objects must follow `SCENE_LAYER_OVERLAY_SPEC.md`. UI panels may inspect, select, filter, style, create, or edit tracks, ROIs, annotations, measurements, labels, and reference visuals through typed commands, but panels must not directly create renderer resources. Hover, active tool previews, and selection handles are transient viewer state unless explicitly committed.

## Analysis Tool UX

Analysis tools should use a dedicated tool model:

- active tool: inspect, pan, draw ROI, brush, threshold, measure, etc.
- tool options panel
- selected object inspector
- undo/redo stack
- command history
- preview/finalize controls
- job progress and cancellation
- result tables and plots as first-class views

The UI must clearly distinguish:

- view-local preview
- approximate result
- exact final result
- partial/incomplete result
- failed or cancelled result

Users must not be led to believe that a quick preview over visible/resident data is a final full-dataset measurement.

## Layout Discipline

UI layout must use stable regions and shared primitives.

Required practices:

- panels have explicit ownership and bounded responsibilities
- repeated controls are components
- viewport resizing is deliberate and testable
- labels, values, and controls survive narrow windows and high-DPI scaling
- long labels and platform font differences do not overlap controls
- controls expose loading/disabled/error states consistently
- layout code is not mixed deeply into renderer logic

Avoid:

- huge `ViewerUI` or `AppUI` monoliths
- catch-all UI utility files
- layout hacks that depend on incidental widget sizes
- one-off hand-aligned controls
- UI code that changes durable state as a side effect of painting

## Design System Dependency

Detailed design-system rules live in `DESIGN_SYSTEM_SPEC.md`. UI implementation work should satisfy both this workflow spec and the design-system spec.

## Invariants

- Invalid datasets do not enter the viewer.
- Long tasks must expose cancellation.
- UI must not hide hard validation errors behind warnings.
- Controls should reflect current state truthfully.
- Analysis controls must reflect preview, approximate, exact, partial, failed, and cancelled states truthfully.
- The app must not look or behave like generated throwaway UI.
- Visual consistency is a quality requirement, not polish.
- The viewport is the primary surface.
- Panels and controls must be modular and reusable.
- UI changes must not regress unrelated layouts silently.

## Failure Modes

- user selects unsupported data
- preprocessing fails
- GPU unavailable
- dataset partially corrupt
- insufficient memory/VRAM
- controls overlap, clip, or resize unpredictably
- panel tweaks break unrelated layouts
- inconsistent spacing, typography, icons, or widget styles
- state changes are hidden inside widget rendering
- panel code directly creates or owns scene-layer renderer resources
- UI tests pass but screenshots reveal visual slop
- analysis preview appears indistinguishable from final output
- long analysis job blocks the UI thread
- cancelled analysis job leaves a result that appears complete

## Testing Requirements

- UI smoke test for app launch.
- Dataset open flow tests.
- Error display tests.
- Preprocessing cancellation UI test.
- Screenshot checks for major workspaces once UI stabilizes.
- Screenshot regression tests for the main workbench at representative window sizes.
- Layout overflow tests for long labels, high-DPI scale factors, and narrow panels.
- Interaction tests for scene-layer selection, hover, and edit command flow.
- Interaction tests for analysis preview/finalize/cancel workflows.
- Viewport-size stability tests for UI panel changes.
- Component-level tests or snapshot checks for repeated controls where practical.

## Open Questions

- Whether to use document/project tabs.
- How much of preprocessing configuration appears in advanced panels.
- Long-term track import and metadata editing workflow.
- Exact icon set and typography.
- Exact visual theme and color tokens.
- Accessibility target beyond basic keyboard/focus/readability support.

Concrete UI testing tooling is defined in `TESTING_TOOLING_SPEC.md`.
