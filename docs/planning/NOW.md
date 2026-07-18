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
the active checkpoint. Its strict external v5 profile, ten-scenario scripts,
independent fidelity/oracle bundle, correctly placed GPU timing controls,
per-view generation/scale/coverage facts, structural and unique-work counters,
source-preservation inventory, and exact import receipt gate are implemented.
One earlier fresh normal-product IP run passed as focused predecessor evidence;
focused evidence also records the predecessor's perspective-distance, four-
panel LOD, temporal handoff, and prepared-layout failures without averaging
them away.

The first predecessor sampling launch was stopped before acceptance when a
read-only audit found that the EP-01 selection authority did not yet freeze
byte-exact candidate identity, package, checkpoint, ordered-trace, GPU-lookup,
gate-operand, and receipt contracts. A second exact launch was intentionally
stopped after both RZ roles proved that v3 terminal acceptance waits collapsed
an attributable product miss into an ambiguous automation failure and skipped
the remaining required checkpoints. Both partial roots are retained as
interrupted evidence and are never reused. The v4 repair then separated
complete evidence from typed product outcomes, but its first baseline launch
was owner-rejected because serial long-deadline observations left the visible
application static for minutes after valid scripted actions. That partial root
is also preserved, never reused, and establishes no complete sample or
performance claim.

The version-5 hard cut replaces those serial observations with one grouped
concurrent observation batch per contiguous acceptance checkpoint. All members
use a shared declared origin and validated profile, oracle, or protocol
deadlines, so checkpoint wall time is bounded by
the maximum deadline rather than their sum. The exact development population
is three ordered samples of all ten scenarios with a fresh instrumented and
matched control process for each scenario and sample: 60 role attempts, zero
retries, and balanced role order. Distinct exact hard-safety caps still stop a
runaway attempt and fail evidence integrity. Typed product misses continue
through the complete population, but the first fatal setup, process, hard-
safety, or evidence-integrity failure stops immediately and preserves the
partial lineage. All non-import prerequisite waits are fixed by the validated
profile or protocol and are at most 30 seconds; no script-authored multi-minute
viewer prerequisite remains.
The runner derives each role-process bound from that validated schedule plus
bounded launch/closeout grace, counting the IP import-primary wall once; there
is no caller-authored process-timeout override.
The immediate next action is to freeze the v5 repair on one clean revision,
rotate the opaque profile binding, and run the complete predecessor protocol
against its exact release build. EP-01 may begin only after the resulting
receipt proves that every scenario is attributable and preserves every failure
rather than hiding it behind a timeout or missing metric. The integrated
first-overhaul source and its supporting evidence remain current implemented
facts in
[Current state](../CURRENT_STATE.md) until the atomic successor hard cutover
passes its ordered gates.

No absolute performance, orders-of-magnitude improvement, or comparison-viewer
claim is accepted. Final qualification still requires owner-bound workload,
fidelity, hardware, thresholds, clean immutable revision, and externally
inspected real-display evidence under [testing and evidence](../TESTING.md).

Other unresolved maintenance remains in [the backlog](../BACKLOG.md). Deferred
public-data and segmentation outcomes remain separate from this work.
