# LOD Scheduling Stability Specification

Status: ACCEPTED
Last updated: 2026-07-07

## Purpose

Define stable, truthful LOD scheduling for large 3D/4D display.

## Scope

This contract applies to dense intensity `MIP`, `DVR`, and `ISO` runtime LOD
selection, pending loads, promotion, fallback selection, user-visible status,
and interaction responsiveness.

## State Model

- `target_scale`: finest desired scale for the current view under policy.
- `fallback_scale`: deterministic coarser scale used when the target is hard
  impossible under current budgets.
- `pending_scale`: target or fallback currently being loaded for promotion.
- `displayed_scale`: complete scale currently presented.

## Contract

- Target/fallback selection must be deterministic from view geometry, budgets,
  residency limits, and hard feasibility.
- During active 3D movie playback, if the normal target for the current view is
  `s0` and the dataset provides `s1`, the effective runtime target is `s1`.
  Normal `s1` or coarser views are not made coarser by playback. Single-scale
  datasets keep their normal target. This playback target is runtime-only and
  is removed immediately when playback stops.
- A pending target/fallback must not masquerade as displayed data before it is
  complete enough for the active mode.
- Promotion is monotonic toward the selected target/fallback and uses complete
  frames, not arbitrary partial results.
- A coarser complete frame must not replace a better complete displayed frame
  unless it is the selected fallback for a new hard-feasibility state.
- Hard failures choose one stable fallback and do not loop through scales.
- Hidden channels do not affect current-frame LOD scheduling or residency work.
- Status must report displayed, target, pending/fallback, completeness, backend,
  and hard fallback reason truthfully.
- Playback downshift must be reported truthfully as playback LOD. When playback
  stops, status must immediately return to the normal target policy; if `s0` is
  pending while a complete `s1` frame remains displayed, the UI must report
  shown `s1` / target `s0` rather than blocking or blanking.

## Failure Modes

- displayed scale churn during orbit/pan/zoom
- recent frame timing changes semantic target selection nondeterministically
- incomplete target frames appear as complete display
- fallback loops or repeated retry storms
- hidden channels trigger decode/upload/render work
- status describes target scale as displayed scale
- playback leaves a persisted/session LOD override after stopping
- playback stop blocks the UI while source scale reloads

## Testing Requirements

Coverage must include deterministic target/fallback selection, complete-frame
promotion, scale churn prevention, missing data, hard fallback reporting,
hidden-channel exclusion, playback downshift start/stop behavior, scale-specific
playback request keys and prefetches, and real-sample interaction evidence for
`MIP`, `DVR`, and `ISO` when LOD behavior changes.
