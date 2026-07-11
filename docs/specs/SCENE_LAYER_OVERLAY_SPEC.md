# Scene Layer And Overlay Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define typed scene layers, overlays, picking, hover, tracks, ROIs, labels, and
annotations.

## Scope

This contract covers layer types, coordinate spaces, time semantics,
persistent/transient state, render extraction, pass ordering, occlusion,
picking/hover, data ownership, and testing.

## Core Model

- Scene layers are typed: intensity, ROI, annotation, measurement, track,
  marker, text, and transient interaction overlays.
- Persistent project-owned artifacts are separate from transient UI state.
- Layer identity, visibility, order, time scope, transform, and ownership must
  be explicit.
- Overlay geometry is expressed in documented coordinate spaces and converted
  through typed transforms.

## Rendering Contract

- Overlay extraction must be bounded and view-aware.
- Hidden layers do no current-frame extraction, upload, render, pick, or hover
  work.
- Labels and annotations render after intensity products according to the
  defined display graph.
- Occlusion/depth behavior must be explicit for each overlay kind.
- Text labels must remain legible and must not be the only encoded identity for
  scientific objects.

## Picking And Hover

- Picking returns typed targets with layer/object identity, coordinate, time,
  source/value metadata where relevant, and confidence/coverage state.
- Hover is transient and must not mutate persistent artifacts.
- Picking must distinguish hidden, missing, incomplete, uncovered, and
  non-pickable states.

## Ownership

- Dataset packages own source data and native package metadata.
- Project/session or sidecar data owns user-created scene artifacts unless an
  accepted format spec moves a specific artifact into the native dataset.
- Analysis outputs must carry provenance and stale-state detection.

## Failure Modes

- overlay state stored in the wrong owner
- hidden overlays still consume render/pick work
- screen-space drawing loses world/time identity
- hover mutates persisted state
- pick target lacks layer/object identity

## Testing Requirements

Coverage must include layer visibility/order, coordinate transforms,
time-scoped overlays, hover/picking, occlusion, labels/text, ROI/track geometry,
project persistence, hidden-layer exclusion, and export/provenance for analysis
artifacts.
