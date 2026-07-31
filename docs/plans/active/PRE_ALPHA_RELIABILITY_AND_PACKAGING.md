# Pre-Alpha Reliability And Packaging Plan

- Status: IMPLEMENTED AND VERIFIED
- Planning requested by owner: 2026-07-31
- Implementation authorized by owner: 2026-07-31
- Last reviewed: 2026-07-31

This document is the implementation and handoff plan for publishing the
completed rendering checkpoint, correcting native clean shutdown, exposing
unsaved-project autosave recovery in the packaged application, and producing
one validated Linux x86_64 pre-alpha package.

The completed rendering checkpoint is commit
`441329d4ad75817b7fd4086dacd45459da32a0f7`. GitHub's protected `main` branch
correctly rejected a direct push. The checkpoint is published at
`origin/agent/pre-alpha-reliability`; the final implementation will continue
on that branch and enter `main` through the repository's pull-request
workflow.

## Outcome

The completed package will provide all of the following:

1. the rendering-performance closeout and this reliability work are preserved
   on a pushed GitHub branch with a draft pull request;
2. a clean native window close exits promptly and successfully through the
   same joined shutdown owner used by signal and application-requested exit;
3. a dirty native close still presents Save, Discard, and Cancel without
   silently losing project state;
4. when an earlier launch left a provisional autosave, opening the same
   dataset makes that recovery visibly available without requiring the user to
   know a state-directory path or project UUID;
5. recovery remains explicit, validated by the project-store actor, opens
   dirty, and requires a later Save/Save As rather than silently changing the
   stored branch;
6. a clean committed Linux x86_64 pre-alpha package is built with its existing
   AppImage, tarball, unpacked directory, manifest, dependency audit, content
   report, and smoke logs; and
7. the unpacked packaged executable passes the bounded mapped product checks
   for rendering, provisional-autosave recovery, and native X11 close.

This is a local pre-alpha release candidate for maintainer and research use.
It does not create a supported public release, compatibility promise,
auto-update channel, signing claim, or non-Linux support claim.

## Diagnosed Current State

### Native close

The current clean-close route has two competing shutdown phases:

1. the window manager's close request is cancelled;
2. the application asks the project-store actor to close;
3. the actor is joined from the ordinary frame/event path; and
4. the application emits a second synthetic viewport close.

`on_exit` is already the composition root's shutdown owner and already closes
and joins the project-store actor and the other application services. The
extra pre-exit path duplicates that authority. A bounded current-binary X11
probe reproduced a clean-close hang: the native close was cancelled and the
process remained alive until a later SIGTERM used the separate graceful
signal route.

### Unsaved autosave recovery

The persistence foundations already provide the hard parts:

- provisional autosaves are ordinary authenticated project-store packages
  under the application recovery root;
- recovery-root discovery is canonical and bounded;
- the application service owns recovery locators;
- the actor validates a selected store and generation;
- provisional recovery opens as a dirty branch; and
- the existing recovery window can dispatch an explicit selected locator.

The missing product boundary is exposure and package evidence. Startup does
not proactively show discovered earlier-launch provisional stores, readiness
is not clearly tied to current-source verification, and no normal packaged
multi-launch check proves that a real provisional autosave can be found and
opened after a process crash.

## Non-Negotiable Invariants

### Shutdown ownership

- `MiranteWorkbenchApp::on_exit` is the sole application-exit shutdown and
  join owner.
- A clean native close is accepted on its first close event. It is not
  cancelled merely to close the project actor before `on_exit`.
- A dirty native close is cancelled only to obtain an explicit Save, Discard,
  or Cancel decision.
- Save-success and Discard authorize exactly one new viewport close; they do
  not join services in the frame loop.
- Cancel leaves the application and all services running.
- SIGINT/SIGTERM remains prompt-free and uses the same accepted viewport-close
  and `on_exit` ownership.
- Dataset replacement remains distinct: because the process continues, its
  project-store handoff may still close and join the old actor before
  installing the new source.
- Every owned worker/service is still shut down and joined. No detached exit,
  force-kill, timeout-as-success, or ignored actor remains.

### Recovery authority and user safety

- `ProjectStoreApplicationService` and its actor remain the sole authority for
  opening and validating a recovery store.
- The application may discover only bounded canonical locator names; it does
  not decode project bytes on the UI thread.
- Recovery is never automatic. Startup may open a notice/panel, but the user
  must explicitly choose a candidate.
- Recovery never repairs, deletes, renames, or advances the original recovery
  package.
- A recovered provisional branch opens dirty and cannot be reported saved.
- Recovery waits for the current dataset's scientific verification. The UI
  must explain that wait instead of presenting an action that is known to be
  rejected.
- A source-identity mismatch fails visibly through the normal application and
  project-store route.
- Discovery failure disables only provisional autosave/recovery exposure and
  remains visible; it does not weaken source or project validation.

### Package and evidence scope

- Packaging runs only from a clean committed checkout.
- The package report remains bound to the exact Git commit and tree.
- Existing dependency policy, AppStream, runtime-dependency, AppImage,
  tarball, and unpacked-directory smoke requirements remain mandatory.
- Product checks launch the packaged release executable, not a debug binary or
  a test-only renderer.
- The reliability scenario uses the promoted small fixture, an isolated
  `XDG_STATE_HOME`, bounded process deadlines, no automatic retries, and no
  linked-S0 or other host-stress workflow.
- A crash-recovery check may use external SIGKILL only after the application
  has durably reported completion of its provisional autosave.
- The native-close check uses a real mapped X11 client and a real window-manager
  close request. A later SIGTERM or force-kill is cleanup on failure, never a
  passing result.
- Internal command completion alone cannot prove the native close; the
  external process exit status and deadline are authoritative.

## Architecture And Hard Cut

### One exit route

The native close policy becomes:

```text
native close event
  ├─ clean or explicitly authorized -> accept -> eframe exits -> on_exit joins
  └─ dirty and not authorized        -> cancel once -> Save / Discard / Cancel

Save succeeds or Discard
  -> mark close authorized
  -> emit one viewport Close
  -> eframe exits
  -> on_exit joins
```

The implementation deletes the exit-only project-store state and its
frame-loop join branch. Project-store close completion remains supported for
in-process dataset replacement and explicit automation, where the process
does not exit.

### Startup recovery exposure

After bounded recovery-root discovery, the application records whether
earlier-launch provisional stores exist. If they do, the normal recovery
window opens at startup and the project status states that unsaved work may be
recovered. The window stays explicit: it lists locator identities and offers
“Inspect and Recover” only after the current source is scientifically
verified and the unbound project-store service can open it.

Selection follows the existing route:

```text
user-selected recovery locator
  -> ApplicationCommand::RequestProjectOpen
  -> ProjectStoreApplicationService::submit_open_recovery_store
  -> actor validates package, projection, and recovery graph
  -> application validates dataset identity
  -> recovered projection opens dirty
```

No UI or automation code reads a generation directly. No second recovery
implementation is introduced.

### Packaged reliability scenario

One focused mapped scenario will launch the same packaged executable in an
isolated state home:

1. open and verify the promoted fixture, attach an unsaved project, make a
   durable edit, wait for the real scheduled provisional autosave, publish a
   checkpoint, and receive external SIGKILL;
2. reopen the fixture, prove the startup recovery panel and locator are
   exposed, explicitly recover the sole provisional autosave through the
   normal application/service route, prove the project is dirty and rendered,
   then exit normally; and
3. start a clean session, publish a ready checkpoint, receive an external X11
   window close, and prove prompt successful exit without fallback
   termination.

This scenario is a narrow lifecycle check. It does not rerun the project
store's full durability matrix or the rendering performance program.

## Deleted Behavior

Implementation removes:

- `exit_after_project_close`;
- `pending_viewport_close`;
- the exit-only branch that joins the project-store actor from
  `handle_project_store_event`;
- cancellation of a clean native close solely to wait for that branch;
- the second synthetic close emitted after the frame-loop join; and
- the effectively hidden earlier-launch recovery state in which locators
  exist but startup gives no direct notice.

No compatibility shutdown path, duplicate recovery opener, or test-only
product behavior remains after the cut.

## Work Packages

### P0 — Checkpoint publication

- Preserve the completed rendering commit on a GitHub branch.
- Respect protected `main`; do not bypass required pull-request checks.
- Continue reliability work on `agent/pre-alpha-reliability`.

### P1 — Native shutdown hard cut

- Replace the two-phase clean-close route with first-event acceptance.
- Route dirty Save/Discard authorization to one viewport close.
- Keep Cancel and in-process dataset replacement semantics unchanged.
- Make `on_exit` the only exit-time project-store close/join owner.
- Delete obsolete exit state and branches.

### P2 — Packaged recovery exposure

- Detect discovered earlier-launch recovery locators during app construction.
- Open the normal recovery panel and publish a clear startup status when they
  exist.
- Gate “Inspect and Recover” on verified-source and project-store readiness.
- Preserve explicit selection, actor validation, dirty-open semantics, and
  source mismatch failure.
- Add focused UI/application checks for exposed, waiting, ready, selected, and
  absent states.

### P3 — Focused real-product lifecycle evidence

- Add only the automation vocabulary needed to observe and select an exposed
  provisional locator.
- Add the bounded multi-launch packaged reliability scenario described above.
- Record external X11 geometry, checkpoint, process status, source
  non-mutation, recovered project state, and package-binary identity.
- Make hangs, missing reports, failed recovery, missing dirty state, fallback
  termination, and nonzero native-close exit fail the scenario.

### P4 — Pre-alpha package

- Run focused checks while iterating.
- Run repository policy, formatting/lint, and the full proportional pull-
  request gate before packaging.
- Commit the final source and documentation so packaging sees a clean tree.
- Build the Linux x86_64 release directory, AppImage, and tarball through
  `cargo xtask package-linux-release`.
- Inspect the generated content report, smoke logs, manifest, dependency
  audit, and artifact digests.
- Run `target_fixture_render_modes` against the unpacked packaged executable.
- Run the packaged reliability scenario against that same executable.

### P5 — Handoff and publication

- Update `CURRENT_STATE.md`, `planning/NOW.md`, `RELEASE.md`, `TESTING.md`, and
  `BACKLOG.md` with only verified facts.
- Mark this plan implemented and verified only after the exact checks pass.
- Commit and push the final branch.
- Open a draft pull request with the checkpoint, checks, package paths, and
  remaining pre-alpha boundary.

## Useful Checks

Focused implementation checks:

```bash
cargo test -p mirante4d-ui-egui project_recovery
cargo test -p mirante4d-app native_close
cargo test -p mirante4d-app provisional_recovery
cargo test -p xtask product_validate
cargo xtask docs-check
```

Repository gate:

```bash
cargo xtask verify-pr
```

Package and mapped product checks:

```bash
MIRANTE4D_APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
  cargo xtask package-linux-release

MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=target/mirante4d/dist/\
mirante4d-0.1.0-linux-x86_64-release/mirante4d-app \
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes

MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=target/mirante4d/dist/\
mirante4d-0.1.0-linux-x86_64-release/mirante4d-app \
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate pre_alpha_reliability
```

## Implementation And Verification Result

The planned hard cut is complete:

- the completed rendering checkpoint is preserved on
  `origin/agent/pre-alpha-reliability`;
- clean native close is accepted immediately and `on_exit` is the sole
  exit-time close/join owner;
- dirty Save, Discard, and Cancel retain their explicit decision route;
- bounded earlier-launch locator discovery proactively exposes unsaved
  provisional recovery after startup;
- recovery waits for source verification, follows the normal application
  service and actor route, and opens the selected provisional branch dirty;
- automation script schema 8 adds only the recovery exposure and selection
  vocabulary needed by the product boundary; and
- `pre_alpha_reliability` implements the fixed three-launch mapped scenario
  with no retries or timeout-as-success behavior.

The full pull-request gate passed before packaging: policy, formatting/lint,
documentation, dependencies, fixtures, workflows, and 1,306 tests passed,
with 33 explicitly skipped and no failures. The clean Linux x86_64 package
command then passed dependency policy, release compilation, AppStream
validation, release-directory/AppImage/tarball construction, and all three
package smoke checks.

Both mapped product checks passed against the unpacked packaged executable.
`target_fixture_render_modes` exercised the promoted fixture and MIP/DVR/ISO
matrix. `pre_alpha_reliability` completed all three required launches with
zero retries: external SIGKILL only after the durable provisional-autosave
checkpoint, explicit dirty recovery on the next launch with a nonblank GPU
capture and successful actor close/join, and a real clean X11 window-manager
close that exited successfully without fallback cleanup. All three source
closures were byte-identical and all bounded stderr logs were panic-free.

The generated contents report under `target/mirante4d/dist/` is the authority
for the exact package commit, tree, artifact digests, dependency result, and
smoke results. The mapped product reports under
`target/mirante4d/product-validation/` are the authority for their exact
binary path and observations. These generated local artifacts are not
committed and do not create a public release.

## Acceptance Criteria

The package is complete only when:

- the clean native X11 close exits inside the bounded deadline with status
  zero and without fallback termination;
- dirty Save, Discard, and Cancel remain covered and retain their intended
  state transitions;
- a real provisional autosave created by one packaged process is visibly
  exposed and explicitly recovered by a later packaged process;
- the recovered project is bound, dirty, rendered, and not silently saved;
- source-package bytes remain unchanged across the reliability launches;
- the full repository gate passes;
- all three built package forms pass their existing smoke checks;
- the unpacked packaged executable passes render-mode and reliability product
  validation; and
- the final branch, commits, and draft pull request are published.

## Out Of Scope

- Public release publication or end-user support commitments.
- Signing, notarization, auto-update, installer, Windows, or macOS work.
- Project-store format changes, recovery auto-selection, repair, deletion, or
  maintenance UI.
- Smooth-linear rendering optimization or any other rendering refactor.
- The quarantined linked-S0 diagnostic or any high-load performance
  qualification.
- A compatibility reader or migration promise for experimental datasets or
  project stores.
