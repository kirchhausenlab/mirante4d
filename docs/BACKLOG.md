# Unresolved Backlog

The current checkpoint lives in [planning/NOW.md](planning/NOW.md). This file
contains unresolved work only.

## Deferred

- Recover static multichannel viewer performance after the correctness-first
  visible-layer cut. Entry requires a controlled same-dataset, same-hardware
  baseline/current comparison covering selected and displayed per-layer maps,
  interaction latency, settlement time, I/O/decode/upload work, and GPU timing.
  Preserve visible-layer authority, scientific semantics, native output, and
  atomic presentation; playback requires separate explicit authorization.
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
