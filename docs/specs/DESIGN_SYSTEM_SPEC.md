# Design System Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define the visual and component standards that keep Mirante4D's native UI professional, consistent, and maintainable.

## Scope

This spec covers:

- visual quality bar
- theme tokens
- component library expectations
- layout primitives
- icon and control usage
- anti-slop rules
- design verification

## Non-Goals

- Marketing website design.
- Browser UI compatibility.
- Decorative UI for its own sake.
- Preserving the legacy web viewer's look and layout.
- Throwaway debug panels as the final product UI.

## Requirements

- Mirante4D must look like a professional scientific visualization application.
- The UI must not look temporary, generated, or debug-only: no arbitrary decoration, inconsistent spacing, random component styles, unstable alignment, or filler layouts.
- A shared design system must exist before substantial UI feature work.
- Repeated controls must be implemented as reusable components.
- Visual tokens must be centralized instead of duplicated across panels.
- UI elements must remain readable and aligned across common window sizes, high-DPI settings, and platform font differences.
- Design quality must be verified with screenshots and layout checks, not judged only by code review.

## Visual Direction

The app should feel:

- calm
- precise
- technical
- dense but readable
- professional
- built for repeated scientific work

The app should not feel:

- decorative
- promotional
- toy-like
- improvised
- debug-only
- generated from unrelated generic app templates

Use restrained visual hierarchy:

- typography
- spacing
- alignment
- grouping
- state color
- meaningful icons

Do not use:

- decorative gradients or background blobs
- oversized hero text
- marketing cards
- random accent colors
- inconsistent rounded rectangles
- nested cards inside cards
- explanatory clutter that describes obvious UI behavior

## Token System

The design system should define tokens for:

- colors
- typography
- spacing
- radii
- borders
- shadows, if any
- icon sizes
- control heights
- panel widths
- animation timing, if used
- status colors

Tokens should be named semantically, for example:

- `panel.background`
- `panel.border`
- `text.primary`
- `text.muted`
- `accent.active`
- `status.warning`
- `status.error`
- `channel.swatch.border`
- `viewport.background`

Avoid hard-coded colors, sizes, and spacing in feature panels.

## Layout Primitives

The UI should be built from shared layout primitives:

- app shell
- top toolbar
- left side panel
- right inspector panel
- bottom timeline/status region
- section header
- property row
- layer row
- compact toolbar group
- viewport overlay group
- modal/dialog
- progress row

Panels should have explicit ownership and stable sizing rules. A panel should not become a dumping ground for unrelated controls.

## Component Library

Expected reusable components include:

- icon button
- icon/text button for clear commands
- segmented control
- toggle/checkbox
- slider with numeric value
- numeric input/stepper
- dropdown/menu
- color swatch
- channel/layer row
- visibility/lock controls
- time scrubber
- playback controls
- progress indicator
- status badge
- tooltip
- collapsible section
- error panel
- diagnostics table

Repeated controls should not be reimplemented by copying local layout code.

## Control Rules

- Use icons for common tool actions where the meaning is standard.
- Use tooltips for icons whose meaning may not be obvious.
- Use segmented controls for mutually exclusive modes.
- Use toggles or checkboxes for binary states.
- Use sliders, steppers, or numeric inputs for numeric values.
- Use menus for option sets.
- Use color swatches for channel and label colors.
- Use compact, stable dimensions for toolbar controls.
- Avoid visible instructional text for standard controls.

## Viewport And Overlays

The viewport is the primary product surface.

Viewport overlays should be:

- minimal
- readable over data
- dismissible or unobtrusive where appropriate
- tied to real state
- stable during playback and navigation

Do not cover important image data with decorative panels or oversized labels.

## Accessibility And Readability

Minimum expectations:

- readable text contrast
- keyboard-reachable core controls where practical
- visible focus state
- no tiny text for primary controls
- no text clipping in common window sizes
- no color-only meaning for critical errors
- high-DPI friendly sizing

Scientific density is allowed, but unreadable density is not.

## State And Component Boundaries

Components should receive typed state and emit typed commands/actions.

Avoid:

- widgets that mutate broad global state directly
- renderer resource logic inside UI component code
- data-engine logic inside panel code
- layout components that know dataset file paths
- feature panels that directly manage unrelated workflow state

## Testing Requirements

Design-system verification should include:

- screenshot baselines for major app shells
- component snapshot/screenshot tests where practical
- layout tests for narrow windows
- high-DPI screenshot tests
- long-label and long-value tests
- no-overlap/no-clipping checks where practical
- viewport-size stability tests
- visual review artifacts for major UI changes

A UI feature should not be considered done if it only works functionally but visibly breaks alignment, spacing, overflow, or consistency.

## Invariants

- Visual polish is part of correctness for UI work.
- One-off UI is technical debt unless explicitly temporary and tracked.
- Repeated UI patterns must become reusable components.
- The design system is the default source of truth for spacing, color, typography, and controls.
- UI implementation must be modular, not monolithic.

## Failure Modes

- panels use different spacing or typography
- icons and text buttons are mixed inconsistently
- debug controls leak into product UI
- changes to one control misalign unrelated controls
- the viewport is treated as secondary to panels
- screenshots reveal overlap, clipping, or generated-looking filler UI
- agent-generated UI satisfies behavior but fails professional visual quality

## Open Questions

- Exact theme colors.
- Exact typography stack.
- Exact icon library.
Concrete screenshot/UI testing tooling is defined in `TESTING_TOOLING_SPEC.md`.
- Minimum supported window size.
- Whether to support user-selectable light/dark themes in v1.
