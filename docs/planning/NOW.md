# Current Work

Last updated: 2026-07-18

## Current Status

The foundation implementation and deletion audit through WP-15, the
import/preprocessing hard cutover, and the corrective `uint8` sentinel no-data
restoration are complete. [Current state](../CURRENT_STATE.md) and [testing and
evidence](../TESTING.md) own their implemented facts, revision-bound evidence,
and accepted remaining risks.

## Active Implementation Checkpoint

The [viewer performance overhaul](../plans/active/VIEWER_PERFORMANCE_OVERHAUL.md)
remains active, but its earlier VP-00 through VP-05 implementation is now the
current baseline rather than the accepted performance outcome. Owner product
testing found a material improvement but rejected completion: wheel zoom can
lag or freeze, the four-panel workflow can select an unjustifiably coarse LOD,
compound-angle movement can expose arriving bricks, and overall performance
remains below the required standard.

The replacement EP-00 through EP-07 plan redesigns preprocessing, the
experimental package layout, streaming, caching, GPU residency, arbitrary-
plane rendering, coordinated frame submission, low-level MIP/DVR/ISO and
arbitrary-plane kernels, LOD, and presentation as one pipeline. It fixes one
shared cubic-brick identity across storage, scheduling, decoded caching, and
GPU residency while separating hot view control from persistent GPU state.
Anisotropic or orientation-specific payloads and runtime strategy selectors
are excluded.

The owner granted implementation authorization for the replacement plan on
2026-07-17. EP-00 workload, fidelity, cost-truth, and absolute-gate binding is
the active checkpoint. Its strict external v3 profile, ten-scenario scripts,
independent fidelity/oracle bundle, correctly placed GPU timing controls,
per-view generation/scale/coverage facts, structural and unique-work counters,
source-preservation inventory, and exact import receipt gate are implemented
and pass schema/parity checks. One fresh normal-product IP run passed; focused
evidence also records the predecessor's perspective-distance, four-panel LOD,
temporal handoff, and prepared-layout failures without averaging them away.

The immediate next action is to freeze the worktree as one clean immutable
baseline revision and run the complete predecessor protocol against that exact
release build. EP-01 may begin only after the resulting receipt proves that
every scenario is attributable and preserves every failure rather than hiding
it behind a timeout or missing metric. The integrated first-overhaul source
and its supporting evidence remain current implemented facts in
[Current state](../CURRENT_STATE.md) until the atomic successor hard cutover
passes its ordered gates.

No absolute performance, orders-of-magnitude improvement, or comparison-viewer
claim is accepted. Final qualification still requires owner-bound workload,
fidelity, hardware, thresholds, clean immutable revision, and externally
inspected real-display evidence under [testing and evidence](../TESTING.md).

Other unresolved maintenance remains in [the backlog](../BACKLOG.md). Deferred
public-data and segmentation outcomes remain separate from this work.
