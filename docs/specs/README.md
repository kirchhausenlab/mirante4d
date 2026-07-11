# Specifications

Active specs define current product, architecture, data, renderer, workflow,
and verification contracts.

## How To Read

Start with `../CURRENT_STATE.md`, then read only the minimum domain specs
needed. For target foundation implementation, also follow the
[foundation refactor handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
and its work-package entry order.

Status meaning:

- `ACCEPTED`: binding current contract unless superseded by the milestone
  contract or explicit user request.
- `DRAFT`: provisional or reference guidance. Do not treat draft-only text as
  binding current scope.
- `SUPERSEDED`: historical only; active docs must point to the replacement.

## Domain Index

Core product/workflow:

- `APPLICATION_SPEC.md`
- `FIRST_APP_WORKFLOW_SPEC.md`
- `VIEWER_BEHAVIOR_SPEC.md`
- `PROJECT_SESSION_MODEL_SPEC.md`
- `UI_UX_SPEC.md`
- `DESIGN_SYSTEM_SPEC.md`
- `RELEASE_PACKAGING_SPEC.md`

Format, preprocessing, and data:

- `DATASET_FORMAT_SPEC.md`
- `NATIVE_DATASET_V1_SCHEMA_SPEC.md`
- `DATASET_V1_STORAGE_POLICY_SPEC.md`
- `INTENSITY_DTYPE_CHANNEL_SPEC.md`
- `NO_DATA_MASK_POLICY_SPEC.md`
- `INTENSITY_VALIDITY_RANGE_HIERARCHY_SPEC.md`
- `SPATIAL_GEOMETRY_RESAMPLING_SPEC.md`
- `IMPORT_PREPROCESSING_WORKFLOW_SPEC.md`
- `PREPROCESSING_SPEC.md`
- `DATA_ENGINE_SPEC.md`
- `DATA_ENGINE_RUNTIME_POLICY_SPEC.md`

Renderer and display:

- `RENDERER_SPEC.md`
- `RENDERER_PIPELINE_SPEC.md`
- `GPU_RESIDENT_DISPLAY_RENDERER_SPEC.md`
- `MIXED_CHANNEL_RENDER_MODE_REFACTOR_SPEC.md`
- `DVR_TRANSFER_RGBA_RENDERING_SPEC.md`
- `DVR_OPACITY_TRANSFER_FUNCTION_SPEC.md`
- `ISO_DISPLAY_SPACE_SURFACE_RENDERING_SPEC.md`
- `ISO_SURFACE_LIGHT_CONTROL_SPEC.md`
- `OUTPUT_COVERAGE_DISPLAY_TRANSFER_SPEC.md`
- `LOD_SCHEDULING_STABILITY_SPEC.md`
- `CAMERA_VIEW_SPEC.md`

Analysis and scene:

- `ANALYSIS_SPEC.md`
- `SCENE_LAYER_OVERLAY_SPEC.md`

Quality, operations, and governance:

- `PRODUCT_VALIDATION_TESTING_REFACTOR_SPEC.md`
- `TESTING_TOOLING_SPEC.md`
- `QUALITY_ASSURANCE_SPEC.md`
- `PERFORMANCE_TARGETS_SPEC.md`
- `WORKSPACE_SPEC.md`
- `MODULARITY_SPEC.md`
- `ARCHITECTURE_ENFORCEMENT_SPEC.md`
- `DEPENDENCY_POLICY_SPEC.md`
- `CI_CD_SPEC.md`
- `ARTIFACTS_POLICY_SPEC.md`
- `VERSIONING_SPEC.md`
- `CONFIGURATION_SPEC.md`
- `OBSERVABILITY_SPEC.md`
- `ERROR_HANDLING_SPEC.md`
- `ERROR_LOGGING_DIAGNOSTICS_SPEC.md`
- `DATA_SAFETY_SPEC.md`
- `UNSAFE_FFI_POLICY_SPEC.md`
- `CONCURRENCY_SPEC.md`

## Draft Specs

The current draft specs are active reference only:

- `ARCHITECTURE_ENFORCEMENT_SPEC.md`
- `ARTIFACTS_POLICY_SPEC.md`
- `CI_CD_SPEC.md`
- `CONCURRENCY_SPEC.md`
- `CONFIGURATION_SPEC.md`
- `DATA_SAFETY_SPEC.md`
- `DESIGN_SYSTEM_SPEC.md`
- `ERROR_HANDLING_SPEC.md`
- `ERROR_LOGGING_DIAGNOSTICS_SPEC.md`
- `MODULARITY_SPEC.md`
- `OBSERVABILITY_SPEC.md`
- `QUALITY_ASSURANCE_SPEC.md`
- `SPATIAL_GEOMETRY_RESAMPLING_SPEC.md`
- `UNSAFE_FFI_POLICY_SPEC.md`
- `VERSIONING_SPEC.md`

If a draft describes implemented behavior, keep it synchronized with code. If it
describes future direction, keep that status explicit and put actionable work in
`../BACKLOG.md`.

## New Spec Template

```md
# Title

<status line: DRAFT or ACCEPTED>
Last updated: YYYY-MM-DD

## Purpose
## Scope
## Non-Goals
## Contract
## Invariants
## Failure Modes
## Testing Requirements
## Deferred Work
```

Keep active specs concise and testable. Execution logs and private closure
evidence stay outside the public source tree.
