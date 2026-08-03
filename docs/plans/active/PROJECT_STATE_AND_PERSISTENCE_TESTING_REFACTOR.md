# Project State And Persistence Testing Refactor

- Status: IMPLEMENTED — PORTABLE VERIFIED; TRUSTED EXT4/VM REFRESH PENDING
- Audit requested by owner: 2026-08-03
- Target approved by owner: 2026-08-03
- Last reviewed: 2026-08-03
- Scope: canonical project-model validation, production project-store
  conformance, hosted process/fault evidence, local power-cut selection,
  application persistence handoff, and developer test reliability.

This plan is the implementation authority for the project-state and
persistence testing corrections accepted after the 2026-08-03 read-only
audit. The product format, immutable-generation architecture, actor ownership,
accepted Linux ext4 durability tuple, and application-service authority remain
unchanged.

The work strengthens the evidence for those contracts. It does not add a
compatibility reader, automatic repair, a second store, a general maintenance
UI, a broader filesystem qualification, or a power-loss claim for tests that
only kill a process.

## Outcome

The refactor must make every green persistence result mean what its name says.
In particular:

1. the local lifecycle lane must execute every host-side test whose evidence
   it parses while leaving the VM guest driver exclusively inside the VM;
2. injected transition failures must prove the expected public failure and
   safe fresh-process state, not merely prove that an injection marker was
   reached;
3. a named successful create/save/analyse/reopen workflow may not return green
   after an unsupported-filesystem branch skips that workflow;
4. every promoted independent project-store corruption must be exercised
   through production Rust and checked against its declared rejection class;
5. the ordinary parallel developer test command must not fail because test
   subprocesses transiently inherit unrelated lease descriptors;
6. public project-model rejection branches and boundary behavior must have
   exact, compact coverage; and
7. representative capacity and permission failures must cross the public
   actor/service boundary and prove that prior authority and source data stay
   intact.

## Contracts That Must Remain

- `mirante4d-project-model` remains the canonical durable model and
  persistence-neutral projection authority.
- `mirante4d-project-store` remains the sole project storage authority and one
  actor remains the sole filesystem mutation owner.
- The application project-store service remains the sole product route for
  Create, Open, Save, Save As, autosave, recovery, dirty close, and joined
  shutdown.
- Objects and generations remain immutable; refs remain small and atomic;
  complete generation publication remains generation-last.
- Direct and paged closure, revisions, provenance, leases, ref continuity,
  recovery selection, cancellation, and bounded namespace work remain
  fail-closed.
- Unqualified new destinations fail before mutation. Existing stores on an
  unsupported filesystem remain read-only where the product contract permits.
- Save As leaves the source untouched and installs only an authenticated,
  complete destination closure.
- Indeterminate durability suspends writes until reopen; visible files alone
  do not establish a successful commit.
- Process termination evidence is labelled process-crash evidence. Only the
  rootless ext4 VM cut harness may support the plan's power-cut evidence.
- The independent fixture remains producer/reader-independent. Production
  writer/reader agreement is not substituted for independent expected facts.
- Tests and validation never mutate microscopy source data or silently repair
  a project.

## Current Inventory And Diagnosed Defects

At the planning baseline, `mirante4d-project-model` has 17 routine tests.
`mirante4d-project-store` has 136 registered tests: 132 routine and four
ignored. The ignored cases are one VM guest driver and three host-side
exhaustive/process matrices.

The routine store suite passes under process-isolating Nextest, but ordinary
parallel libtest runs intermittently fail with writer-lease `ReadOnly` results
or completion timeouts. Serial execution passes but is materially slower. The
test support code already identifies a parallel fork/exec descriptor window;
the protection is not used consistently.

The implemented lifecycle runner invokes only the nonignored Nextest suite,
then requires hosted transition, Trash, and Purge markers emitted only by
ignored tests. Its hosted phase therefore cannot satisfy its own parser. The
fourth ignored test is the VM guest driver and must not be selected on the
host.

The independent project fixture contains four stores, twelve generations, and
sixteen promoted negative mutations. Production Rust consumes the positive
stores, but the promoted mutations currently prove only that the independent
Python validator rejects its own generated corruptions.

Several application success-path tests return successfully when the temporary
directory is on an unsupported filesystem. At least one test named for the
complete import/analyse/save/reopen workflow passes without analysing, saving,
or reopening under that condition.

## Required Changes

### 1. Exact lifecycle ownership and selection

Replace the lifecycle hosted phase with explicit, fail-closed ownership:

1. run the exact routine `mirante4d-project-store` inventory with ignored
   cases excluded;
2. run exactly these host-side ignored cases with `--run-ignored only`:
   `exhaustive_hosted_and_process_transition_matrix`,
   `trash_fresh_process_kill_and_retry_matrix`, and
   `purge_fresh_process_kill_and_retry_matrix`;
3. keep `project_store_vm_guest_driver` selected only by the VM guest entry
   point; and
4. parse evidence only after native process success and exact selected,
   executed, passed, failed, skipped, and ignored inventories reconcile.

The runner and registry must reject a renamed, newly ignored, unexpectedly
unignored, missing, duplicated, or host/guest-misassigned case. A portable
self-test must prove the generated commands and selectors without requiring a
VM. The existing VM manifest self-test continues to prove its 11 flows and 59
transition cuts, but it is not relabelled as a power-cut execution.

No compatibility alias retains the broken mixed selector. The local lane
remains opt-in, clean-revision guarded, bounded by its existing aggregate
deadline, and absent from GitHub Actions.

### 2. Meaningful hosted fault-injection oracles

Every newly injected hosted transition row must verify more than marker
reachability. The child protocol must report a bounded canonical terminal
outcome containing the operation, transition, edge, injected fault, public
result class, and whether mutation authority was acquired.

The parent must require:

- the exact requested transition and edge were hit once;
- the operation returned the expected typed injected failure or terminated in
  the exact declared process-fault mode;
- no child panic, success-after-injection, timeout, malformed report, or extra
  transition marker occurred;
- a new process can inspect or open the resulting store under the expected
  recovery classification;
- active/manual/autosave/recovery/pin authority matches the transition
  family's declared before-or-after state;
- full structural verification succeeds whenever that state is meant to be
  openable; and
- retry is successful and idempotent for transition families whose contract
  permits retry.

Shared transition-family oracles should cover the 338 injected rows; the
implementation must not create hundreds of hand-coded case branches. Existing
SIGKILL reopen/retry and pure before/after tree comparisons remain separate
evidence classes and retain their stronger checks.

### 3. Honest filesystem capability results

Split qualified-filesystem success and unsupported-filesystem behavior:

- A test named for a successful create/save/reopen or complete product
  workflow must require the accepted writable filesystem capability and must
  complete every named phase.
- Unsupported-filesystem behavior receives separately named exact tests that
  require no destination mutation, retained dirty state where applicable,
  visible typed unavailability, and clean joined shutdown.
- A missing writable capability may be an explicit reported skip in a runner
  that supports non-green skips, or the success case may live in the local
  qualified lane. It may not be a return from a green success-path test.
- Common helpers return a typed capability/result to the caller; they do not
  hide the branch or decide that an incomplete workflow passed.

At least one portable regression must deliberately use an unsupported local
filesystem when available and prove the unsupported contract. The qualified
ext4 success path remains part of the relevant local persistence boundary.

### 4. Production consumption of promoted corruptions

Add a manifest-driven Rust conformance runner over every promoted independent
mutation. The fixture tooling may emit bounded mutated stores into ignored
temporary output, or Rust may apply the manifest's exact mutation recipe, but
the resulting bytes and expected identity must remain independently defined.

For every mutation, production inspection/open must assert:

- the exact mutation identifier is individually reported;
- the public rejection class matches the manifest contract;
- malformed envelope, ref, generation, object/page, closure, provenance, and
  scientific-rebinding families remain distinguishable where the public API
  distinguishes them;
- no repair, ref advance, object publication, or other source mutation
  occurs; and
- the runner continues through all cases and reports every failure.

The Python fixture `--self-test` remains useful producer/validator evidence,
but it no longer stands in for production negative conformance.

### 5. Reliable ordinary parallel tests

Remove the shared-process fork/lease race without serializing the complete
suite or weakening production lease behavior. Use the smallest test-only
mechanism that closes the actual window, such as:

- isolating subprocess-owning cases in registered Nextest groups;
- a parent/child exec-readiness handshake before lease reacquisition; or
- one centralized bounded test-only eventual reacquisition helper used by
  every affected case.

The chosen mechanism must retain immediate production lease semantics. A
focused regression must run affected subprocess tests concurrently and prove
they do not return a spurious read-only result. The complete ordinary
parallel package command must pass repeatedly with zero retries; a serial-only
green result is insufficient.

### 6. Compact canonical project-model coverage

Add table-driven exact-error coverage for:

- empty, invalid-character, and overlong channel-preset identifiers;
- empty, whitespace-only, control-containing, byte-overlong, and exact-limit
  labels, including multibyte Unicode boundaries;
- empty views, duplicate logical layers, and a missing active layer;
- duplicate preset entries and IDs;
- duplicate artifact source layers and artifact handles;
- missing and half-present regenerable-artifact provenance; and
- accepted and rejected exact collection/aggregate boundaries.

Negative constructors assert the exact `ProjectModelError`, not only
`is_err()`. Strengthen the revision high-water property to begin from arbitrary
valid current/high-water pairs, and make the reorder property exercise an
actual generated permutation rather than reversal alone. Retain fixed seeds,
bounded case counts, and the suite's subsecond intent.

### 7. Representative public full-flow faults

Retain the existing narrow helper-level fault tests, but add a compact public
actor/service matrix for:

- capacity exhaustion before publication;
- permission denial before publication; and
- one partial-write or commit-indeterminate path where the existing fault
  boundary supports it.

Each case must assert the exact public fault, previous ref/authority and source
closure unchanged, no complete partial destination, no leaked writable
authority, successful fresh inspection/reopen of the prior state, and finite
joined shutdown. Real filesystem capacity/permission behavior belongs in the
qualified local lane when deterministic portable injection cannot represent
the public flow honestly. Do not build a combinatorial filesystem matrix.

### 8. Runtime and fixture discipline

Correctness takes priority over runtime. After the required oracles pass,
reduce redundant extraction of the 16.8 MiB independent fixture only through
a safe immutable template plus copy-on-write/reflink copy where supported.
Tests that mutate a store may not share hardlinked writable files. A missing
optimization is not a correctness failure; unsafe fixture sharing is.

Routine model/store tests should remain practical for ordinary development.
Exhaustive hosted matrices and the power-cut VM stay explicit
changed-boundary checks.

## Verification Topology After Cutover

| Evidence | Command class | Claim |
| --- | --- | --- |
| Model and routine store behavior | Ordinary PR/Nextest groups | Portable deterministic correctness |
| Independent fixture validation | Python validator plus Rust conformance | Independent positive and negative format evidence |
| Hosted transition/process matrices | Explicit local host selector | Process-fault, fresh-reopen, and retry evidence |
| Qualified filesystem integration | Explicit local accepted-ext4 selector | Writable actor/service behavior on the accepted tuple |
| Rootless ext4 VM lifecycle | `verify-local project-store-lifecycle` VM phase | Named transition power-cut evidence only |
| Normal application persistence | Focused app/service plus mapped scenario when affected | Product handoff; mapped use still required for visible behavior |

No host-only process test is described as power-loss evidence. No VM
self-test is described as an executed VM campaign. The native exit status is
authoritative; parsed evidence cannot override a failed or incomplete process.

## Implementation Order

1. Register this plan and freeze the exact test/fixture inventory.
2. Repair lifecycle ownership and exact selector self-tests.
3. Strengthen hosted injected-failure outcomes and fresh-process oracles.
4. Remove unsupported-filesystem false greens.
5. Connect every promoted independent mutation to production Rust.
6. Close the parallel developer-harness race.
7. Add compact project-model and public actor fault coverage.
8. Run focused, routine, hosted, fixture, qualified-filesystem, VM, policy,
   documentation, and mapped-product checks required by the changed boundary.
9. Update current authorities and mark only actually executed evidence as
   automated-verified or product-validated.

## Deletion Gate

The refactor is incomplete while any of these remain:

- a lifecycle host command that skips a required evidence producer;
- a host command that accidentally selects the VM guest driver;
- marker-only acceptance of injected transition failures;
- a success-path test that returns green on `UnsupportedFilesystem` before its
  named workflow completes;
- promoted independent mutations checked only by the independent validator;
- documented reliance on serial `cargo test` to hide the fork/lease race;
- broad `.is_err()` assertions where an exact public model error is part of
  the contract; or
- helper-only ENOSPC/permission tests described as complete actor/service
  persistence evidence.

No compatibility selector, duplicate capability helper, or alternate store
path survives the cutover. Git history is the archive.

## Risks And Controls

- **Exhaustive checks become routine overhead.** Keep hosted matrices and VM
  disruption local and changed-boundary only; retain fast routine invariants.
- **Fault injection proves its own hook.** Require typed public outcome, fresh
  process inspection, authority invariants, and retry where applicable.
- **A filesystem skip becomes a pass.** Use a distinct test/result identity;
  never return from a success case as green.
- **Fixture independence is lost.** Keep mutation recipes and expected facts
  in the independent corpus; production only consumes and classifies them.
- **Test-only retry leaks into product behavior.** Confine fork/exec handling
  to test orchestration; production lease acquisition remains immediate.
- **A process crash is called a power cut.** Reports name the exact disruption
  mechanism and only the VM phase carries power-cut language.
- **A real ENOSPC test damages unrelated data.** Use a bounded owned image or
  deterministic actor-boundary injector; never fill a general filesystem.

## Explicit Non-Goals

- A project-store format or durability-policy redesign.
- A compatibility reader, migration, repair, or fallback store.
- Broadening writable qualification beyond the accepted Linux ext4 tuple.
- Adding project maintenance, Trash, or Purge UI.
- Authorizing deletion of non-regenerable artifacts.
- Claiming arbitrary-filesystem, arbitrary-process-point, or universal
  power-loss safety.
- Moving the local lifecycle lane to GitHub or adding a self-hosted runner.
- Large random fuzzing infrastructure or a combinatorial fault matrix.
- Performance qualification for project-store throughput.

## Implementation Record

The repository cutover completed on 2026-08-03. The lifecycle registry now
separates host selectors, accepted-filesystem integration, and the VM guest
driver; hosted rows prove typed public outcomes plus fresh-process recovery;
unsupported filesystems cannot make named success tests green; all sixteen
independent mutation recipes cross production Rust; fork/lease orchestration
is reliable under repeated parallel Nextest runs; compact model/property
coverage exercises the public validation surface; and ENOSPC, permission, and
short-write faults cross the actor/application-service boundary while
preserving prior authority.

Portable model, store, application-service, hosted matrix, independent
fixture, registry, and parallel-repeat evidence is part of the implementation
closeout. The accepted-ext4 and rootless VM phases still require a clean
trusted-local revision and are not replaced by the hosted process tests. No
new power-cut or broader-filesystem claim is made from leaving those local
phases pending.

The final portable working-tree closeout passed the complete policy group,
including the four-store, sixteen-mutation, four-recovery-state project
fixture and the VM harness self-test. Clippy, exact discovery ownership, and
all 1,453 routine unit/contract/UI cases then passed with zero retries. The 42
registered ignored cases were discovered and audited but not executed; that
set includes the deliberately deferred trusted filesystem and GPU work.

## Completion Standard

This plan is complete only when:

1. exact host, guest, routine, and qualified-filesystem ownership reconciles;
2. the hosted and VM lifecycle phases can execute their required inventories
   and the native process result remains authoritative;
3. every injected hosted row proves its declared safe outcome;
4. no named success workflow can pass without completing its behavior;
5. all promoted positive stores and negative mutations cross production Rust;
6. repeated ordinary parallel project-store runs pass with zero retries;
7. compact exact project-model validation and property coverage passes;
8. representative public actor/service capacity and permission faults pass;
9. relevant focused suites, fixture validators, hosted matrices,
   `project-store-lifecycle`, `cargo xtask verify-pr`, registry, and
   documentation checks pass for the final revision; and
10. authority documents report important local, VM, filesystem, or mapped
    skips honestly.

A passing serial suite, a Python-only mutation self-test, a VM-manifest
self-test, a transition marker, or a green unsupported-filesystem early return
does not complete this plan.
