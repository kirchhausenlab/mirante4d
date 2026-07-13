# Foundation Refactor — Project Store And Durability Brief

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-13
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: D-010 project transactions, commit protocol, concurrency, recovery, autosave, garbage collection, and durability evidence

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

## Approved Project Store And Durability Contract

### D-010 Approved Project Store

Use a transparent directory-backed content-addressed store with immutable
objects and complete immutable generations. Do not use mutable artifact paths
as authority, copy the entire project for each save, or introduce a hybrid
SQLite/filesystem two-phase commit.

The initial logical layout is:

```text
experiment.m4dproj/
  project.json                         fixed store envelope + project UUID
  refs/
    head                               atomic {current, previous} generation ref
    recovery                           independently durable prior manual tip
    autosave-head                      atomic {current, previous, base} ref
    autosave-recovery                  independently durable prior autosave tip
    pins/<checkpoint-id>               explicit retained generations
  generations/sha256/ab/<digest>.json  immutable complete snapshots
  objects/sha256/ab/<digest>           immutable exact-byte artifacts
  staging/<transaction-id>/            never authoritative
  locks/                               diagnostic metadata, not lock authority
  trash/                               quarantined explicit-GC output
```

`project.json` identifies the experimental store profile and one stable random
project UUID; mutable viewer state never lives there. A generation contains
the project ID, kind, sequence, parent, captured project revision, the persisted
project-bound revision high-water, D-009 scientific identity, optional
package/release pin and locator hints, one bounded
persistence-owned state DTO, and typed object descriptors with digest, byte
length, schema/media type, logical role/handle, provenance, completeness, and
recoverability class.

Every ref is a bounded canonical record with magic, schema, exact length, and
checksum so truncation/tearing is detected before a digest is followed. Manual
and autosave heads each keep current and previous, while their independently
synced recovery refs preserve the prior committed tip if the corresponding
head itself is lost.

The durable DTO does not import renderer/GPU/worker/cache/scheduler objects,
arbitrary internal paths, or the segmentation model being deleted. Runtime
state is projected explicitly into persistence-owned types and validated as a
closed generation before commit.

#### Commit And Dirty-State Protocol

One background project-store actor owns serialization and filesystem writes.
UI commands submit immutable domain snapshots; no save, autosave, hash, flush,
or directory operation runs on the interaction thread.

Each durable domain command creates a `ProjectRevisionId`; undo/redo moves the
current revision pointer but never rolls the project-bound revision high-water
back. A save captures an exact `(revision, revision_high_water, snapshot)`
tuple.
`saved_revision` changes only after that exact revision commits durably. If the
user edits revision 12 while revision 11 is saving, completion of 11 leaves the
project dirty.

The commit protocol is:

1. Hold the real OS advisory writer lease and verify that `refs/head` still
   names the expected parent generation.
2. Serialize, hash, stage, flush, validate, and immutably publish every changed
   object on the destination filesystem with create-if-absent/no-replace
   semantics; sync the containing directories. An existing object at the same
   digest must validate exactly or the store reports corruption.
3. Serialize the complete canonical generation, hash it, stage it, flush it,
   validate its object closure, publish it with the same no-replace semantics,
   and sync its directory.
4. Atomically set and directory-sync `refs/recovery` to the validated old
   current generation before changing the authoritative head; the first
   generation has no old current and therefore no recovery ref yet.
5. Write and flush a tiny replacement head containing both the new current and
   old current-as-previous generation IDs.
6. Atomically rename that file over `refs/head` without first moving the
   authoritative head away, then sync the refs directory.
7. If ref-directory sync fails, retry it to success. If durability still cannot
   be established, return typed `CommitIndeterminate`, keep the revision dirty,
   suspend further writes, and require close/reopen recovery; rereading a
   visible new head is not proof that the rename survived power loss. Only a
   fully durable outcome advances `saved_revision`.

Before the head switch, the complete old generation remains authoritative.
After the durable switch, the complete new generation is authoritative.
If execution stops after recovery sync but before the head switch,
`recovery.current == head.current` is a recognized interrupted state: the old
head remains authoritative and no ref is repaired automatically.
Staged or published-but-unreferenced objects/generations are harmless orphans;
a ref never points into staging or at a partial closure.

Initial Save and Save As construct a sibling temporary package on the
destination filesystem, fully sync it, and install it with no-clobber rename
only when the destination does not exist, then sync the destination parent.
An existing unrelated/nonempty destination is refused; saving an already open
matching project uses the normal generation protocol. Moving a whole package
keeps its project ID. Save As creates a new project ID with `forked_from`
provenance unless a later explicitly named workflow requires identity-
preserving cloning. Cross-device Save As copies and rehashes the reachable
closure into destination staging rather than relying on hard links or cross-
device rename.

#### Concurrency, Autosave, Recovery, And GC

- Every opener holds a shared OS maintenance lease for its whole session; a
  writable opener additionally holds the writer lease. Lock acquisition order
  is maintenance then writer. Compaction requires the exclusive maintenance
  lease, so it cannot move an object while any read-only or writable process is
  streaming it. Lock-file text may aid diagnosis, but presence, PID, age, or
  hostname never proves ownership. A second process opens read-only when it
  cannot acquire the writer lease.
- Every commit also compares the expected parent; a mismatch returns a typed
  conflict. There is no automatic project merge.
- Autosave uses the same object/generation format but switches only
  `autosave-head` through the same autosave-recovery-before-head protocol and
  coalesces queued requests in the store actor. An established autosave records
  its current manual base; a provisional autosave has no manual base. Its first
  private publication installs a complete caller-located sibling-staged package
  with only a base-less autosave head, and later provisional autosaves advance
  that lane without becoming established. A manual save may clear/advance
  autosave refs only after the manual head is durable. A crash before that
  cleanup leaves a harmless stale autosave suppressed by base/revision
  comparison rather than prompting the user or becoming a permanent live root.
  The application integration owns the 30-second idle and 120-second maximum
  scheduling because it owns live revisions and capture creation; the store
  actor publishes and coalesces submitted captures. The B3 candidate implements
  that exact deadline with an injected monotonic clock in a private application
  service, including edit-during-capture, failure/cancellation, and
  `CommitIndeterminate` handling. The service remains product-unreachable until
  B4.
- The normal recovery prompt is offered only for a newer same-base autosave.
  A changed-base autosave is labeled divergent. Manual previous, manual
  recovery, and manual generations found by bounded scan are labeled manual
  branches. Orphan autosaves retain the classification derived from their own
  base/revision facts. Every explicitly selected recovery projection opens
  dirty as an unsaved branch; it is never silently merged, promoted, or used to
  repair a ref. Unsaved projects use the same store in the application recovery
  area under a provisional ID.
- Open validates the envelope, refs, generation digests/schema, and referenced
  object names/types/lengths eagerly. Bulk object digests are checked during
  streaming or an explicit/background full verify, not through an unbounded
  synchronous open.
- An invalid current generation may lead to an explicit offer of the validated
  previous or independent recovery generation. If a head is corrupt, the
  independent recovery ref is the only automatic fallback. If both are invalid,
  a bounded scan may list validated generations only as user-selected recovery
  candidates; it cannot distinguish every committed old tip from pre-commit
  orphans and never auto-repairs a ref.
- Successful Open and OpenRecovery return the held ProjectStoreSession together
  with the validated ProjectGenerationProjection. Candidate inspection remains
  metadata-only at its public boundary: it may decode projection state for
  validation but does not return or expose it before explicit selection.
- The public actor starts unbound and performs all binding work on its worker.
  First provisional Autosave carries one explicit destination; later autosaves
  carry none. Create installs either a fresh project or the first manual package
  for the same healthy provisional project. A recoverable failed Open retains
  only a recovery session: it permits inspection, explicit selection, verify,
  cancellation, and close; after selection, Save As may fork the exact selected
  generation to a new destination. Failed transfers retain the old session, and
  no handoff deletes, repairs, or mutates the old package.
- Automatic cleanup removes only exact writer-private transaction directories
  after an existing-store opener has acquired the writer lease, completed
  bounded store/recovery validation, and reconfirmed the writer against the
  same root. Read-only sessions never clean. Every staging child must match the
  canonical
  `tx-<nonzero-minimal-u32-pid>-<32-hex>-<16-hex>-<00..7f>` grammar and be
  empty or contain only one bounded, same-filesystem, single-link regular
  `payload`; PID, age, hostname, and lock text never establish ownership.
  Cleanup preflights the whole bounded namespace before mutation, removes in
  bytewise order, syncs the transaction and staging directories, and is
  retryable without markers. An existing empty `staging/` is synced on a
  zero-removal retry; a missing one is not created.
  Cancellation is preflight-only; any failure after removal begins is
  write-suspending and indeterminate. This does not cover sibling package
  stages, immutable orphans, hostile external writers, power loss, or
  filesystem qualification. Explicit compaction roots only the current/
  previous manual head, manual recovery, current/
  previous autosave head, autosave recovery, and pins. `generation.parent` is
  provenance, not a liveness edge, so complete history requires pins. Before
  moving anything, compaction lists orphan generations as recovery candidates.
  Any unknown/unparseable file anywhere, corrupt graph, or active maintenance
  lease blocks deletion. Unreachable content moves into synced `trash`; purge
  is a separate explicit action. Live files are never deleted in place.
- Annotations, ROIs, tracks, measurements, manual edits, imported material,
  and any analysis output without a complete verified deterministic recipe are
  non-regenerable by default. They are never age-pruned or provenance-guessed
  away. Trashing or purging a non-regenerable candidate requires an itemized
  confirmation and the approved verified-backup policy. The current Trash API
  cannot carry that proof, so its implementation must reject such candidates
  with `ConfirmationRequired`; a future proof-bearing API must be approved
  separately.
- Trash uses mirrored generation/object namespaces under `trash`, fresh
  exclusive-maintenance preflight, retained-generation closure subtraction,
  no-replace moves, and bounded durable batches. It never sweeps anonymous
  unrooted objects. Cancellation may stop only between synced batches; any
  other post-mutation failure is write-suspending and indeterminate.
- Purge selects the complete canonical trash snapshot only after strict active-
  plus-trash validation proves every generation regenerable. It removes
  objects in synced bounded batches while retaining their generation records,
  crosses a revalidated empty-object barrier, then removes generation records.
  It retains the directory hierarchy. Cancellation between batches leaves a
  synced idempotent prefix; a post-unlink failure or process kill is
  indeterminate and requires reopen. Object-phase retry recognizes the ordered
  removal prefix; generation-phase retry revalidates each surviving record.
- The first writable tuple is exactly Linux ext4 magic `0xef53`, normalized VFS
  options `[rw,relatime]`, and super options `[rw]`. Select it by the held root
  descriptor's `statx` mount ID and one unambiguous `/proc/self/mountinfo`
  record; missing, erroneous, ambiguous, or different facts are unqualified,
  and a successful capability probe never adds qualification. `ReadOnly`
  remains read-only. `PreferWritable` on an existing unqualified store reports
  a read-only session; Create, Save As, first provisional Autosave, and every
  other new destination fail `UnsupportedFilesystem` before source reads or
  mutation. No production bypass exists. Internal traversal remains
  descriptor-relative and no-follow.

Internal references are digest-derived and survive moving the package.
Dataset paths/URIs are locator hints, never identity. Relinking binds a new
project generation only after D-009 scientific identity is verified; a trusted
release may bind an alternate exact package with the same science. A
self-declared content-ID field alone is not trusted.

WP-02 cut project v13 to the segmentation-free v14 predecessor. WP-07B deleted
v14 and installed the private experimental project-v15 bridge. WP-10B
hard-cuts that bridge to the owner-approved target store: no v13/v14/v15
reader, in-place mutation, fallback branch, or converter remains. Existing
disposable development projects are regenerated; a converter would require a
separate explicit owner request.

### Project Store Alternatives

| Model | Assessment | Recommendation |
| --- | --- | --- |
| Filesystem content-addressed store + immutable generations | Incremental, inspectable, naturally suited to large artifacts and crash-safe pointer commit; requires careful sync/locking tests | Select |
| SQLite WAL containing all content | Strong built-in transactions/locking, but opaque large-blob behavior, WAL/backup/export concerns, and a larger persistence boundary | Reject as authority; a rebuildable index may be considered later |
| SQLite plus external large files | Retains a two-authority/two-phase-commit problem | Reject |
| Copy-on-write generation directories | Simple mental model but duplicates data or depends on non-portable hard-link/reflink semantics | Reject |
| Append-only event log | Valuable for audit/history, but replay, snapshots, schema evolution, and compaction add unnecessary primary-store complexity | Reject as authority |

### Required Acceptance Evidence

D-009/WP-10A must publish canonical bytes and digest vectors produced by an
implementation independent of the production writer. The matrix covers every
dtype; finite float edge patterns; signed zero; validity/sentinel equivalence;
tile/page/fan-out edges; transform/unit projection; channel/layer ordering;
one-bit changes; compression/chunk/shard/path invariance and sensitivity;
recompression/resharding; missing/duplicate/reordered leaves; package mutation,
path traversal, collision, and symlink rejection; recipe parameter types,
algorithm/policy/seed changes; release mutability; and verified/unverified
project rebinding.

Hashing must be streaming, cancellable, resumable, parallel-deterministic, and
bounded by the approved CPU ledger. DS-0 through DS-4 record throughput and
peak RSS. DS-X receives only a structural simulator identity and cannot claim a
scientific ID for nonexistent voxel payloads.

D-010/WP-10B must inject failures before and after every frozen transition,
including every repeated occurrence and both ref lanes. Fresh-process
`SIGKILL`, reopen, and exact retry cover transitions that mutate filesystem or
lease state or can leave writer-private residue. Pure reads, validation,
comparison, root scans, and candidate listing instead require deterministic
fault and byte-identical no-mutation evidence. The matrix must also exercise
`ENOSPC`, short writes, permissions/read-only errors,
corrupt/truncated refs/generations/objects, concurrent writers, stale-parent
conflicts, crash-released locks, autosave races/divergence, GC interruption,
symlink/path attacks, relocation, Save As, and cross-device-copy failure.

Callback mocks and `SIGKILL` establish logic/process-crash behavior but do not
simulate lost kernel/page-cache writes. The rootless VM therefore cuts one
pre-sequence baseline and each distinct post-transition persistence/authority
boundary: object and generation file-sync/publish/directory-sync; manual and
autosave recovery/head file-sync/replace/directory-sync; package tree-sync,
install, and destination-parent sync; pin file-sync/replace/directory-sync;
unpin remove/directory-sync; Trash directory-create, collision-sync, move,
duplicate-remove, source-sync, and trash-sync; and Purge remove/directory-sync.
Equivalent adjacent before states are not duplicated; distinct lanes,
directories, and repeated mutation states remain separate. Private stage
creation/write/copy, staging cleanup, open/validation, comparisons, lease
upgrade/restore, scans, and listings remain hosted/process evidence. The exact
rootless-QEMU, two-disk, 256-MiB guest, 640-MiB working-disk, 900-second,
zero-retry, performance, and sanitized-report requirements remain those frozen
by the entry and correction. The clean aggregate lane passed on protected-main
commit `4a246a1bb7bfe099673ef10d6cb5951729b3ff37` (tree
`af5531d8ffbda0c13b342a0b4df47a894e7f99fb`): all 120 hosted tests and 60 VM
cut cases passed with zero harness retries. The sanitized report SHA-256 is
`ced8c82c75c480810e7ebf81e2c032e579f89bbb28c1f854d1681a3ddad1f9e5`, and
protected-main policy/Rust checks passed in GitHub Actions run 29273392030.
This qualifies only that exact off-product B2 revision and ext4 tuple. The B3
candidate adds actor-authenticated direct/paged reuse, destination-local Save As
copy-and-rehash, and the private autosave service described above, but its
public and real-display evidence is not yet accepted. B4 product activation,
predecessor deletion, and product persistence validation remain pending.

### Identity And Project Resolution

The owner approved D-009 and D-010 together on 2026-07-09 through OD-020:

1. adopt the versioned SHA-256 scientific/package/recipe/derivation-record/
   release/artifact identity split, typed raw-object descriptors, and the
   storage-independent scientific Merkle rules;
2. adopt the directory-backed immutable content-addressed project store rather
   than SQLite or mutable artifact files;
3. adopt the background commit, exact revision dirty state, OS writer and
   maintenance leases, expected-parent conflict, independent recovery refs,
   autosave/recovery, conservative GC, and qualified-filesystem durability
   policies; and
4. after WP-02's completed hard cut from v13 to transitional v14, replace v14
   with the project store without a v13/v14 compatibility reader or converter.

Approval settles the target boundary; it does not bless byte-level
implementation by inspection. WP-10A/WP-10B still must freeze normative
schemas/domain tags and independently verified test vectors before the new
contracts become a candidate or the product cuts over. No current content ID,
project store, persisted contract, or compatibility promise changes merely
because the planning decision is resolved.
