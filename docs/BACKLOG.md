# Unresolved Backlog

The current checkpoint lives in [planning/NOW.md](planning/NOW.md). This file
contains unresolved work only.

## Deferred

- Complete the owner-authorized
  [viewer rendering correctness, recovery, and numerical cutover](plans/active/VIEWER_RENDERING_CORRECTNESS_RECOVERY_AND_NUMERICAL_CUTOVER.md)
  after its implemented P1–P6 correctness cut. Its pre-edit P0 evidence was
  not captured and remains unclaimed; the designated private A/B/C P7
  matching-work campaign and any measurement-selected correction remain open.
  The mapped render-mode and native-navigation scenarios pass, while a
  suitable non-clipped temporal workload, qualifying clean-revision trusted
  lane, and owner acceptance remain P8 work. No separate speculative
  performance branch is active.
- Add float16 TIFF/storage/rendering/analysis support only through a separate
  dtype capability plan.
- Support mixed-dtype channel publication only through a separate physical
  package-layout plan.
- Consider filename-order preview, first/last filename display, or manual
  source-sequence reordering only after the explicit-source import cutover.
- Select and publish full microscopy datasets through the separate
  [open-data handoff](plans/deferred/OPEN_DATA_FOLLOW_ON.md).
- Reconsider [segmentation](plans/deferred/SEGMENTATION.md) only through a new
  approved capability plan.
- Consider non-Linux targets or 4K qualification only after a concrete need
  and separately approved scope.
