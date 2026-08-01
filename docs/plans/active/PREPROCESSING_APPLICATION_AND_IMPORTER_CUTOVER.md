# Preprocessing Application And Importer Cutover Plan

- Status: COMPLETE; AUTOMATED-VERIFIED; OWNER PRODUCT-VALIDATED
- Planning requested by owner: 2026-08-01
- Target approved by owner: 2026-08-01
- Implementation authorized by owner: 2026-08-01
- Owner product validation accepted: 2026-08-01
- Last reviewed: 2026-08-01

This is the implementation handoff for turning preprocessing into a normal,
explicit product workflow. It replaces dataset-before-window startup,
layout-guessing TIFF import, the import-specific working-memory selector, and
the fixed five-percent import-memory partition with one application shell,
one user-authored source manifest, and one process-wide bounded resource
authority.

The owner subsequently authorized full implementation. This document remains
the handoff authority and now records the implemented cutover and its honest
verification boundary.

## Post-Closeout Supersession

The application shell, explicit source manifest, process resource broker,
no-data path, and source/publication correctness established here remain
current. Later large-dataset investigation showed that the complete canonical
base cache, separate dataset-scale encoded spool, fixture-calibrated aggregate
admission, and two-copy encoded free-space bound do not scale acceptably with
long `T/C`. The approved
[large-dataset preprocessing and storage cutover](LARGE_DATASET_PREPROCESSING_AND_STORAGE_CUTOVER.md)
is the target authority for replacing those mechanisms. This document remains
the historical implementation record of its still-retained product and
scientific boundaries.

## Outcome

Mirante4D always opens its native window, even when no dataset has been
selected. An empty workbench presents a centered launcher with at least:

- `Preprocess a new dataset`; and
- `Load a preprocessed dataset`.

Loading uses the existing strict `.m4d` package path. Preprocessing opens a
wizard in which the user declares each logical channel and its source
interpretation explicitly. The application does not infer channel structure,
time structure, or channel correspondence from directory names or filename
tokens.

The wizard has no import-specific working-memory control. Mirante4D derives
safe concurrency from its process resource policy, exact task costs, current
resource use, and interactive priority. Temporary lack of memory applies
backpressure; it is not a fatal import error while admitted work can finish
and release capacity.

Preprocessing remains decode-once and throughput-oriented. The importer uses
the largest safe phase-specific concurrency, fuses content work into existing
data passes, and retains no redundant raw-source scan or payload-sized copy
without a proven correctness or durability need.

Successful preprocessing retains the current staged, validated, create-only,
atomic publication contract and then opens the new package through the normal
dataset-session boundary.

## Owner Decisions Captured Here

- The normal application must be useful before a preprocessed dataset exists.
- Channel count is explicit and defaults to one.
- Every channel has a user-authored name and an independently selected source.
- Each channel source is explicitly one of three supported TIFF forms.
- Immediate TIFF children are ordered alphabetically. Filenames carry no
  semantic tokens and are never matched across channels.
- The row summary reports count, logical geometry, dtype, and total source
  size. It does **not** show the first or last filename. Filename-range and
  reorder UI are deferred.
- Cross-channel validation requires equal logical `T/Z/Y/X` geometry, unique
  channel names, and—under the current physical package representation—one
  common dtype.
- Float16 is a genuine desired future capability, but it is explicitly outside
  this cutover. No float16 conversion or admission work belongs here.
- The existing uint8, uint16, and finite float32 no-data behavior remains the
  scientific authority.
- The application, not the user, derives preprocessing byte admission and
  phase-specific parallelism.
- Setup remains strict about user-controlled source and scientific choices.
  Once preprocessing starts, internal memory, queue, descriptor, scheduling,
  and viewer-contention conditions are the importer's responsibility and do
  not become user-facing failures.

## Predecessor Failure And Root Cause

### Startup is a dataset session disguised as an application

The native entry point currently opens a folder dialog before `run_native`.
Cancelling that dialog exits the process. `MiranteWorkbenchApp` is then
constructed by opening a dataset, so application services and preprocessing
are downstream of a dataset that a new user cannot yet possess.

This is an ownership error, not merely a missing welcome screen. Import also
borrows `self.dataset.cpu_ledger_arc()`, making a loaded dataset the accidental
owner of preprocessing capacity.

### Source discovery guesses a narrow hierarchy

The current `TiffSource`/`SourceLayout` route accepts an automatic root and
tries to infer multipage stacks or channel folders of planes. A perfectly
valid user arrangement can therefore become “ambiguous” even though the user
already knows exactly which files constitute each channel and what each file
means.

The monolithic `TiffInspection` also assumes one dataset-wide layout and
dtype. It cannot faithfully represent independently chosen per-channel source
kinds.

### “Working memory” and the global ledger contradict each other

The current wizard offers fixed local memory choices. That value sizes the
ordered-worker policy, but import tasks also acquire from a renderer/runtime
CPU ledger whose `ImportWorkingSet` category is hard-capped at two fortieths,
or five percent, of the total dataset CPU budget. Raising the local selector
therefore cannot raise the actual global ceiling.

A representative `108 x 600 x 810` volume contains 52,488,000 voxels. The
current automatic-mask preflight reserves five bytes per voxel—262,440,000
bytes—before its other resident work, producing a 291,535,800-byte minimum
that already exceeds the 256 MiB UI choice.

In the later base-production failure, the process CPU budget was
6,360,942,182 bytes but the hard import category cap was only 318,047,109
bytes. Each task required 13,286,432 bytes. Twenty-three tasks plus
6,967,640 resident bytes consumed 312,555,576 bytes, leaving 5,491,533; the
twenty-fourth task failed. Selecting either 512 MiB or 1 GiB locally could not
change that global five-percent cap.

The failure is therefore deterministic policy conflict, not evidence that the
source is too large or that the machine lacks memory.

### Setup inspection performs material source work

Inspection currently computes a whole-file SHA-256 for every admitted TIFF.
That makes a supposedly brief source summary proportional to all source bytes
before the user has even validated the channel setup. Raw-file hashing is also
used as a temporary checkpoint binding even though the importer already
derives a canonical scientific content address while producing the dataset.

Structural review, source-currentness protection, and durable scientific
identity must be separated instead of making the setup UI perform a full data
pass.

## Non-Negotiable Invariants

### Product and scientific behavior

- Source TIFFs are read-only and never repaired, renamed, reordered, or
  rewritten.
- The user explicitly supplies channel membership and source interpretation;
  Mirante4D never guesses them from names.
- The logical source order is deterministic and binds preprocessing,
  checkpoint resume, scientific content, and output.
- Channel labels survive publication and reopen.
- Spatial and temporal calibration remain explicit, positive, and finite.
- No-data resolution still inspects only channel zero at timepoint zero and
  applies the resulting fixed geometry and plane set dataset-wide.
- Publication remains staged, validated, create-only, and atomic. A partial
  package never appears complete.
- The normal viewer, importer, analysis services, and project services retain
  one authority each; an empty shell creates no dummy dataset or fake catalog.

### Resource behavior

- Memory, worker count, queues, descriptors, scratch storage, and publication
  objects remain explicitly bounded and observable.
- A temporary capacity shortage stops admission and drains useful work. It
  does not convert ordinary backpressure into a terminal error.
- A run starts only after the broker grants one non-borrowable progress lane
  sufficient for every phase's unavoidable resident state plus its smallest
  complete task/result/commit path. A started run therefore cannot later fail
  merely because borrowed parallel capacity disappears.
- Interactive viewing has priority when preprocessing runs beside an open
  dataset. Preprocessing may use substantially more capacity when the shell
  has no dataset session.
- No supported stage requires a full dataset in RAM.
- Every admitted TIFF has a proven bounded decode path. A native TIFF
  strip/tile whose irreducible decoder workspace exceeds the process safety
  envelope is rejected during inspection with that exact cause; the product
  does not pretend a different UI memory choice could fix it.
- Cancellation, source generation change, insufficient scratch space, and
  output publication failure leave the source unchanged and no public partial
  package.

### Cutover behavior

- There is one explicit source-manifest path after the cut. The automatic
  layout parser is deleted from the product.
- There is one application-level CPU resource coordinator after the cut. A
  loaded dataset is not the owner of import memory.
- There is no hidden legacy wizard, compatibility import entry point, or
  dataset-required startup path.

## Product Workflow

### 1. Application shell and dataset session

The native process initializes the window, selected WGPU adapter, renderer
device services, settings, process resource coordinator, project/recovery
services, source-open service, and preprocessing service before any dataset is
required.

The shell has explicit states:

1. `Welcome`: no dataset session; the render stage is empty and the centered
   launcher is visible.
2. `Opening`: an asynchronous package open is in progress; cancellation or
   failure returns to `Welcome` without closing the app.
3. `Viewing`: one `DatasetSession` owns catalog/source/runtime/render
   presentation state for the open package.

Dataset-only controls are hidden or disabled outside `Viewing`. Settings,
logs, recovery notices, loading a package, and preprocessing remain shell
services. The existing development environment variable may enqueue a package
open after the window starts, but normal startup never displays a pre-window
folder dialog.

Preprocessing is available both from the welcome launcher and from the normal
File menu while a dataset is open. When a new package publishes successfully,
the shell opens it through the same source-open transaction used by `Load a
preprocessed dataset`. Existing dirty-project confirmation remains
authoritative when replacing an open dataset.

### 2. Channel setup

The preprocessing wizard begins with a bounded channel-count selector from 1
through the current 64-layer product ceiling. It defaults to one. Increasing
the count creates rows named `channel 1`, `channel 2`, and so on; decreasing
it removes the trailing draft rows after ordinary UI confirmation when those
rows contain selections.

Each row contains:

- an editable channel name;
- an explicit source-kind selector;
- a `Choose` button appropriate to that kind;
- an asynchronous inspection state; and
- a compact structural summary or actionable error.

The three source kinds are:

1. **Single 3D TIFF** — one selected `.tif`/`.tiff`; `T = 1`, and its pages
   form Z.
2. **Folder of 3D TIFFs** — immediate TIFF children only; files in
   deterministic lexicographic filename order form T, and each file's pages
   form Z.
3. **Folder of 2D TIFFs** — immediate TIFF children only; `T = 1`, and
   single-page files in deterministic lexicographic filename order form Z.

Extension matching is ASCII-case-insensitive. Non-TIFF children are ignored;
subdirectories are not traversed. An empty TIFF inventory is an error. A
single-page TIFF is allowed as a depth-one volume in the explicitly 3D source
kinds, while every file in the 2D-folder kind must have exactly one page.

No filename grammar, numeric-token parser, recursion, channel-folder parser,
or cross-channel filename comparison participates. Different channels may use
different source kinds if their final logical geometry and dtype agree.

The summary reports facts such as:

- `1 3D volume · [X,Y,Z] = [...] · uint16 · 400 MiB`;
- `56 3D volumes · [X,Y,Z] = [...] · float32 · 40 GiB`; or
- `5000 2D planes · [X,Y] = [...] · uint8 · 10 GiB`.

It does not display first/last filenames or a filename range. Ordering
visualization and manual reorder controls require a separate future product
decision.

### 3. Structural inspection

Inspection is asynchronous, cancellable, generation-tagged, and bounded to a
small descriptor/read window. It reads file metadata, TIFF headers, primary
IFDs, page geometry, grayscale/sample format, compression admission facts,
OME calibration hints when present, and filesystem size/generation facts. It
does not decode pixels or hash all source bytes.

Within one channel, inspection verifies:

- every page declares an admitted grayscale uint8, uint16, or float32 storage
  type; finite float32 sample validation remains part of bounded ingest;
- all pages/files have the geometry required by the selected source kind;
- all files have the same dtype;
- all 3D files have the same page count, width, and height; and
- all 2D files have the same width and height.

The first mismatch terminates that row's inspection immediately. The error
names the offending file and the expected versus observed metadata. A fully
consistent folder necessarily visits every immediate TIFF child, but never
reads its pixel payload merely to build the UI summary. Progress reports files
inspected over files found so a large valid series does not look frozen.

Changing a row's name, kind, or source invalidates the prior cross-channel
validation token. A newer inspection result cannot overwrite a later row
selection.

### 4. Cross-channel validation

`Validate channels` is enabled only when every row has a successful current
inspection. It validates exactly:

- every normalized name is nonempty and satisfies the existing bounded layer
  label contract;
- normalized names are unique;
- every channel has the same logical `T/Z/Y/X` shape; and
- every channel has the same dtype under the current writer representation.

Name normalization trims surrounding whitespace and applies the existing NFC
text authority; it does not invent channel names from paths. Filename sets,
basenames, numeric tokens, source kinds, and directory structures are not
compared across channels.

Successful validation creates one immutable, generation-bound source manifest
and unlocks the downstream preprocessing controls. Any channel edit or source
generation change revokes it and locks those controls again.

### 5. Preprocessing options

The unlocked section retains the useful current controls:

- spatial calibration, prefilled from unambiguous metadata when available but
  explicitly confirmed by the user;
- temporal interval when `T > 1`;
- automatic no-data detection for every currently admitted dtype;
- manual no-data entry for uint8 only;
- independent `Hide constant Z planes`;
- create-only destination/package name;
- bounded output and scratch-space estimate; and
- `Start preprocessing`.

There is no `working memory` selector and no hidden advanced equivalent in the
normal product. The overall resource policy in application settings remains
the single optional user override for process capacity.

## Source And Package Authorities

### Explicit source manifest

Replace the monolithic automatic `TiffInspection` with a model equivalent to:

```text
PreprocessingDraft
  -> ChannelDraft { label, selected source, latest inspection }
  -> ValidatedSourceManifest
       -> ChannelManifest { label, source kind, ordered files, T/Z/Y/X, dtype }
       -> exact logical (channel, timepoint, z) mapping
```

The immutable validated manifest is the only input to import planning. Base
production, no-data detection, canonical-cache ordinals, pyramid generation,
scientific hashing, recipe metadata, and publication consume its logical
mapping directly. No later stage rediscovers the directory layout.

### Channel labels

The editable name must not be a disposable wizard label. Storage adds one
canonical optional NFC layer label to the display-layer metadata. New imports
must populate it for every logical channel. `LocalDatasetSource` uses that
field as the `DatasetLayer` label; a genuinely absent label retains the
canonical ordinal-derived display name.

Absence is a defined optional-metadata state, not a version-specific reader or
migration branch. Canonical encoding omits an absent value, so existing
complete packages remain byte-for-byte unchanged and semantically unnamed;
the importer emits the field on all new packages. The format lifecycle checks
must independently round-trip labelled and unlabelled packages.

### Dtype boundary

The domain catalog and scientific descriptor can describe dtype per logical
layer, but the current importer writes all channels into one physical image
array with one dtype. This cutover therefore rejects mixed channel dtypes at
`Validate channels` with an exact explanation.

Supporting mixed dtypes later requires a separate package-layout plan, most
likely mapping dtype-compatible logical layers to separate physical images.
It must not be smuggled into this UI refactor.

Float16 remains unsupported by `IntensityDType`, TIFF admission, storage,
rendering, and analysis. The inspector reports it as an unsupported future
dtype and performs no silent float32 conversion. Float16 work is deferred in
full.

### Structural currentness versus scientific content

Setup inspection records a compact deterministic manifest of paths relative
to each selected source, file type, device/inode, byte length, timestamps,
IFD/page facts, and logical ordering. A digest over that small manifest may
bind review generations and temporary checkpoints; it is not represented as a
hash of the microscopy pixels.

Import revalidates those guarded filesystem generations before and during
use. The source-native ingest decodes every admitted native TIFF chunk exactly
once into the canonical raw cache. A canonical decoded-source digest is fused
into that stream before no-data canonicalization or pyramid reduction. The
normal package scientific-content hasher then consumes the resulting base
production stream without causing another TIFF decode.

That decoded-source identity, not a redundant pre-review list of whole-file
hashes, supplies durable path-free source identity for the new import recipe.
The distinction is explicit: filesystem generations prove that the selected
local files did not change during this operation, while the decoded-source
digest identifies the canonical values actually admitted. Neither claim
authenticates biological identity or authorship.

The predecessor per-file full-hash setup pass and final duplicate raw-source
hash pass are deleted as a separately tested identity cut within this plan.
Source mutation still fails closed through descriptor generation checks, and
publication still validates the exact package and scientific content.
Checkpoint binding uses the small structural-manifest generation plus the
decoded-source identity once it becomes available. Existing complete packages
and their historical source records are not reinterpreted.

## Process-Wide Resource Cutover

### One small byte broker

Move the existing byte-ledger authority above an optional dataset session and
extend it into one deliberately small process-level broker. This is not a new
general task scheduler, operating-system simulator, or preemptive runtime. It
owns only:

- one configured hard ceiling for managed CPU allocations;
- exact byte leases acquired before allocation;
- current use and peak diagnostics by purpose;
- one foreground reserve while a dataset session exists; and
- generation-based wakeups when leases or the reserve change.

Dataset runtime, source opening, analysis, preprocessing, and publication use
that same broker for their already-accounted substantial buffers. Untracked
Rust/runtime/driver memory remains outside the managed ceiling, so the default
global resource policy retains its existing host/OS safety headroom. Linux
`MemAvailable` or another current-free-memory observation may conservatively
throttle new background admission, but it is advisory and never replaces
reservation accounting or creates a false hard-memory guarantee.

Categories remain labels for accounting and for distinguishing pinned,
transient, and reclaimable work. Their fixed percentage caps are removed as
hard allocation walls. The total managed ceiling remains hard. Reclaimable
caches may release ordinary least-recently-used entries through their existing
owners; the broker itself does not reach into or mutate another subsystem's
state.

While a dataset is open, one non-borrowable foreground reserve covers the
maximum admitted interactive decode cohort plus its bounded queue/result and
staging requirements. Its size comes from those declared execution bounds,
not an arbitrary percentage and not the full theoretical viewer budget.
Already resident viewer memory remains normally charged. Import may borrow all
other currently free managed capacity but never consumes the foreground
reserve.

With no dataset session the foreground reserve is zero, apart from explicitly
accounted shell/import overhead, so preprocessing can use most of the managed
budget. With a dataset open, import stops admitting new work when only the
reserve remains. Already-running tasks are not abruptly preempted merely to
reclaim bytes; they finish or observe their normal bounded cancellation
checkpoint. This keeps the policy predictable and avoids building a second
priority scheduler.

Before changing the workflow state from setup to running, preprocessing
preflights every phase and acquires one import progress reservation equal to
the maximum phase-local minimum:

```text
unavoidable phase resident bytes
  + one smallest valid task workspace
  + that task's retained result
  + deterministic commit working bytes
```

The reservation is capacity ownership that converts into the phase's normal
leases; it is not eagerly allocated memory and is not double-charged. If a
smaller audited streaming task can satisfy the phase, that path defines the
minimum. Only a source or configured process ceiling that cannot support any
valid minimum path prevents the run from starting.

The progress lane remains owned until completion or cancellation. Additional
capacity is borrowed to expand the sliding window for speed. Viewer demand,
cache growth, queue pressure, or another background service may remove only
that borrowed concurrency, never the one-task progress path. Consequently a
started run may become temporarily less parallel, but ordinary process
contention cannot strand or terminate it.

### Automatic worker policy

The application already derives a default overall CPU budget from installed
memory and permits one explicit global settings override. Preprocessing uses
that policy directly. For each phase it computes:

- unavoidable resident bytes;
- exact per-task working and result-retention bytes;
- CPU-core and codec concurrency ceilings;
- queue/result bounds; and
- currently available process capacity after the interactive reserve.

Worker count is bounded by cores and the maximum phase policy, while actual
in-flight work uses one contiguous ordered sliding window. The earliest
uncommitted task is always admitted before later tasks. Each task's lease
covers its input retention, workspace, queued result, and deterministic-order
wait until commit. This prevents an out-of-order completed result from hiding
memory that the missing earlier task needs.

The owner fills that window only while the next task's complete lease fits. On
ordinary refusal it stops filling, drains and commits already admitted work,
releases leases, and retries after the broker generation changes. It does not
enqueue a fixed 32-task cohort and then treat the twenty-fourth lease refusal
as fatal. It also does not fall back to fully serial work while multiple tasks
fit: the window continuously expands to the largest safe useful concurrency
for the current phase.

A terminal memory error is valid only when the minimum one-task execution path
plus unavoidable resident state cannot fit the hard process ceiling and no
smaller audited streaming path exists. The error reports those exact
components and the global ceiling; it never recommends changing a removed UI
selector.

### Automatic-mask memory

Replace the automatic-mask resolver's dense byte-per-voxel state and
preallocated four-byte-per-voxel frontier. The current six-connected semantics
do not change.

The normal resolver uses:

- bit-packed exact-value, seed, visited, and final-mask state;
- rolling plane/row state for uniform-cube detection;
- run-length rows and a bounded active-component/union structure rather than
  one queued `u32` per voxel;
- a deterministic checkpoint-owned run spool when finalized component facts
  must outlive the bounded active Z frontier; and
- exact scratch-space preflight, cancellation, and source-generation checks.

For the representative 52,488,000-voxel geometry, one bitset is about 6.3 MiB
rather than 50 MiB, and no unconditional 200 MiB frontier exists. This is a
streaming connected-component labeller over exact-value runs, not a voxel BFS
that becomes an external-memory queue after RAM is exhausted. The final
row-packed mask remains the one immutable geometry reused across channels and
timepoints. The bounded run spool is temporary, checksummed/bound to the
import plan, and removed on successful publication or explicit checkpoint
reset.

The first logical volume is ingested into the canonical raw cache before mask
reconstruction. Uniform-block and constant-plane facts are accumulated during
that one TIFF decode. Once the value is known, complete component
reconstruction rereads the local canonical cache sequentially instead of
decoding the TIFF again. Remaining source ingest may proceed through the
bounded pipeline while that CPU work runs, provided the broker admits both and
the overlap improves rather than competes destructively for the same storage.

### Disk and progress

Preflight distinguishes destination bytes, canonical cache, encoded spool,
automatic-mask spill, publication stage, and safety margin. Insufficient disk
space is typed before substantial work when knowable; a later filesystem-full
error remains fail-closed and retains only the valid private checkpoint state.

Progress names real work: channel inspection, planning/preflight, optional
no-data detection, base ingest, pyramid levels, content finalization, shard
publication, validation, and commit. It reports completed/total units when the
total is known and never fabricates an ETA.

## Throughput Architecture

### Performance is a product requirement

The importer must be as fast as the admitted source, CPU, memory, codec, and
destination storage permit without weakening scientific semantics, source
safety, bounded resource use, resumability, or atomic publication. “Bounded”
must not become an excuse for artificial serialization, and “parallel” must
not mean oversubscribing memory or making several workers fight over one
storage device.

The implementation therefore has these structural performance invariants:

- every admitted native TIFF strip/tile is decoded exactly once;
- setup reads metadata only and performs zero full-source hash bytes;
- the import performs no separate raw-source hash pass;
- the decoded-source digest is fused with canonical raw-cache production;
- automatic no-data work rereads the local canonical cache, not the TIFF;
- each logical base/pyramid chunk is numerically produced and encoded once;
- encoded checkpoint payload enters the package writer without decode and
  re-encode;
- substantial buffers come from bounded reusable pools or direct writable
  spans rather than payload-sized clone/copy chains;
- file descriptors and positional readers are reused within their declared
  bounds; and
- durability remains batched at the existing checkpoint/stage authority, not
  expanded into per-file or per-chunk synchronous flushes.

Any proposed optimization that violates one of those invariants needs a
separate owner-approved plan. Any redundant pass or payload-sized copy found
during profiling is a defect unless an independent correctness or durability
boundary proves it necessary.

### Decode-once bounded pipeline

The explicit manifest gives the importer a complete deterministic work graph
before pixels move. Production uses a small fixed set of bounded stages rather
than a generic task-graph framework:

1. Ingest channel zero/timepoint zero first into the canonical raw cache,
   accumulating its uniform-block, constant-plane, decoded-source-digest, and
   source-generation facts during the same source decode.
2. Resolve automatic no-data connectivity from sequential canonical-cache
   reads. Once its immutable policy is known, it no longer blocks unrelated
   source ingest that fits the broker.
3. Continue source-native ingestion in manifest order. Independent single-
   volume files may decode concurrently; a retained multipage decoder remains
   sequential inside that file while downstream chunk work can proceed.
4. Run normalization/base production, pyramid reduction, scientific hashing,
   and inner encoding in phase-appropriate parallel windows as soon as their
   exact inputs and no-data policy are available.
5. Let one deterministic owner commit canonical ordinals, checkpoint batches,
   shards, validation, and publication. Compute may finish out of order, but
   output identity never depends on scheduling.

Every queue between those stages is byte-leased and small. Backpressure moves
to the earliest full boundary; it does not allow decoded planes, encoded
results, or out-of-order completions to accumulate invisibly. When source and
destination share one device, concurrency remains bounded enough to preserve
large sequential I/O. When independent files and CPU codecs are the bottleneck,
the sliding windows expand automatically up to core, codec, descriptor, and
memory limits.

Phase policies are specific and simple: metadata inspection, native decode,
numeric reduction, encoding, and validation do not inherit one arbitrary
global worker count. Each declares its task bytes and useful concurrency cap,
then uses the common broker and ordered-window mechanism. This gets full safe
parallelism without a persistent self-tuning subsystem or user-exposed knobs.

### Performance evidence and retention rule

Before implementation changes the importer, record release-build baselines on
the same clean revision, machine, filesystem, source, destination, and stated
cache condition. At minimum use:

- the existing bounded public T2 workload;
- a generated many-small-2D-files inspection/ingest workload;
- a generated multipage-3D time-series workload;
- a generated multichannel volume-series workload; and
- the representative automatic no-data geometry that currently fails
  capacity admission.

Record end-to-end wall time and the first blocking component, plus source bytes
read, native decode operations/bytes/time, canonical-cache bytes/time, pyramid
and encoding time, writer/validation time, synchronization time, worker busy
time, admission-wait time, peak managed bytes, queue depth, descriptor peak,
and scratch bytes. These are diagnostic counters, not a new receipt or
provenance framework.

For a workload already supported by the predecessor, five independent release
runs under the same declared cache condition compare medians. The cutover must
not regress median end-to-end time by more than five percent without an exact
diagnosed reason and explicit owner acceptance; the target is improvement,
not merely staying below an old broad qualification ceiling. A speculative
optimization is retained only when it removes a structurally redundant
pass/copy or produces a repeatable material improvement—normally at least five
percent in its affected component or end-to-end workload. Otherwise it is
deleted.

New-layout performance claims name source geometry/file count, dtype, codec,
hardware, filesystem, cache condition, metric, and sampling method. High CPU
utilization by itself is not success: the relevant result is shorter verified
end-to-end preprocessing with the same scientific output and bounded resource
facts.

## Failure Semantics

### Setup and validation

Errors remain strict and prominent while the user is constructing the source
manifest and can correct the input:

- Cancelling a chooser changes nothing.
- A row inspection failure affects only that row and leaves its actionable
  error visible.
- A source edit invalidates the manifest; it cannot be silently accepted
  because filenames still match.
- Duplicate names, cross-channel shape/dtype mismatch, unsupported TIFF
  structure, missing calibration, invalid destination, and insufficient
  preflight disk remain explicit setup blockers.
- Automatic no-data no-match remains successful and produces no validity
  arrays unless constant planes were found.
- Cancelling or failing package open returns to the empty shell or preserves
  the previously open dataset.

These messages protect interpretation and consistency. They are not hidden in
the name of convenience, and correcting one field does not discard unrelated
valid setup state.

### Runtime conditions that are not failures

After the workflow reports `running`, all of the following are ordinary
importer mechanics and must be absorbed automatically:

- temporary managed-memory shortage outside the progress lane;
- viewer or analysis demand reclaiming borrowed background capacity;
- a full task or result queue;
- descriptor pressure within the declared descriptor authority;
- out-of-order worker completion;
- a temporarily unavailable byte lease;
- excessive phase concurrency selected by the importer; and
- a transient interrupt or retryable local I/O condition.

The importer reduces its sliding window, stops queue admission, drains and
commits admitted work, releases leases/descriptors, applies bounded retry where
the operating-system error is genuinely transient, and resumes on a broker or
I/O generation change. The UI may truthfully show a neutral state such as
`waiting for resources` or `reducing parallelism`; it does not show a red
failure, request user action, or reset progress.

Raw implementation messages such as `CPU capacity in ImportWorkingSet cannot
satisfy ...` are never normal product outcomes after Start. If the declared
progress lane cannot execute a phase it already preflighted, that is an
internal accounting/implementation defect, not a source-selection problem.

### Legitimate mid-run interruptions

A running import pauses or fails only when continuing could change scientific
input/output, violate publication safety, or is physically impossible despite
the guaranteed execution path. Examples are:

- a source file changing, disappearing, becoming unreadable, or revealing
  corrupt/non-finite pixel data that metadata inspection could not observe;
- the destination filesystem becoming full, read-only, detached, or otherwise
  unavailable after preflight;
- another actor creating/replacing the selected destination;
- a persistent hardware or operating-system I/O failure;
- an internal invariant, codec, or publication-validation failure for which
  continuing could publish incorrect data; or
- explicit user cancellation or process shutdown.

The UI distinguishes `needs user action` from `internal failure`, reports one
plain actionable cause rather than a cascade of allocator details, and retains
the latest valid checkpoint/private stage whenever its binding remains safe.
Freeing space, restoring an unchanged source, or selecting a new destination
must resume from valid work rather than unnecessarily restarting decode.
Internal failures are labelled as such and do not blame the TIFF selection.

Cancellation joins workers and preserves only valid resumable state.
Publication failure never opens or exposes the staged directory. Auto-open
failure leaves the newly published package intact and reports that exact open
failure; it does not rerun preprocessing.

## Predecessor Deletions

The cutover is incomplete until all of the following product behavior is gone:

- the pre-`run_native` package chooser and cancel-to-exit startup;
- `MiranteWorkbenchApp` construction that requires a dataset;
- dataset-session ownership of the import CPU ledger;
- automatic `SourceLayout::Auto` product discovery;
- channel-folder and filename-token inference;
- the one-root/one-layout monolithic TIFF inspection path;
- import destination selection before an explicit channel manifest exists;
- `working_memory_choices`, `working_memory_bytes`, and their egui/application
  draft controls;
- the hard five-percent `ImportWorkingSet` allocation wall;
- fixed-cohort lease failure in ordered workers;
- user-facing mid-run failures for ordinary memory, queue, descriptor, worker
  ordering, or viewer-contention backpressure;
- dense automatic-mask frontier reservation;
- whole-file source hashing in the setup/inspection path;
- generic “choose another TIFF” guidance for capacity, consistency, or
  execution failures; and
- tests that validate only the predecessor inferred layouts or memory-choice
  UI.

Test-only fixture builders may construct explicit manifests directly. They do
not keep the deleted product parser alive.

## Implementation Packages

### P0 — Documentation and authority freeze

- Keep this plan as the sole target authority.
- Mark current-state documents honestly as planned until implementation.
- Record float16, mixed-dtype packaging, and filename-order UI as deferred.

### P1 — Application shell cut

- Split shell services from optional `DatasetSession` state.
- Launch the normal native window without a dataset.
- Add welcome, asynchronous opening, failure/cancel return, and viewing states.
- Route launcher and File-menu package opens through one source-open service.

### P2 — Explicit source model and inspection

- Add per-channel source-kind drafts and immutable inspection generations.
- Implement the three nonrecursive source inventories and deterministic order.
- Replace pixel/hash-heavy setup with bounded metadata inspection.
- Add precise first-mismatch diagnostics and cancellable progress.

### P3 — Wizard, validation, and labels

- Build the channel-count/row workflow and validation token.
- Lock downstream options on every relevant edit.
- Persist canonical optional layer labels and load them into the dataset
  catalog.
- Reject mixed dtype and float16 at their exact boundaries.

### P4 — Process resource authority

- Move the existing CPU byte ledger above an optional dataset session and keep
  it as a small admission broker rather than a second task scheduler.
- Replace hard category partitions with one hard managed total, one declared
  foreground reserve, background borrowing outside that reserve, and
  generation-based wakeups.
- Keep purpose categories as diagnostic/reclamation labels and leave cache
  eviction with each existing subsystem owner.
- Delete the import working-memory UI and fixed local selector.
- Make contiguous ordered-window admission drain-and-retry under ordinary
  contention without preempting admitted tasks.

### P5 — Import engine and no-data cut

- Route planning, decode, cache, pyramid, recipe, and publication through the
  explicit channel manifest.
- Replace dense automatic-mask reconstruction with bit-packed run-based
  connected-component work and one bounded deterministic run spool.
- Separate structural source currentness from canonical scientific content
  identity and delete redundant raw-file hash passes.
- Decode channel zero/timepoint zero once, reuse its canonical-cache bytes for
  automatic-mask reconstruction, and continue the bounded source pipeline as
  soon as dependencies permit.

### P6 — Throughput closeout

- Record release baselines and useful component counters before changing the
  execution path.
- Prove exact-once native decode, zero setup/full-raw-hash passes, encode-once
  package production, and bounded reusable buffer/descriptor behavior.
- Profile the first blocking component on each declared workload, change one
  mechanism at a time, and delete speculative optimizations that do not meet
  the retention rule.
- Meet the predecessor no-regression boundary and report any remaining
  workload-specific bottleneck honestly.

### P7 — Publication, auto-open, and hard deletion

- Preserve checkpoint, validation, create-only rename, and source-nonmutation
  guarantees.
- Open the published package through the ordinary shell transaction.
- Delete every predecessor entry point, model, UI field, parser, and test
  helper named above.
- Synchronize architecture, data-format, current-state, testing, and user
  documentation with implemented facts only.

## Verification

### Focused model and source checks

- Each of the three source kinds maps independently expected files/pages to
  logical `(t,z)` coordinates.
- Nonsense filenames with no numeric relationship still import in exact
  lexicographic order.
- Non-TIFF files and subdirectories are ignored; no recursive discovery or
  naming parser is called.
- Shape, page-count, and dtype mismatch stop at the first offending file and
  do not inspect later files.
- Stale inspection completions cannot replace newer row state.
- Duplicate/empty labels and cross-channel shape/dtype mismatches keep options
  locked.
- Float16 is rejected explicitly and never converted.

### Memory and scheduling checks

- Reproduce the representative geometry and task cost that fail under the
  current five-percent cap; prove ordered admission backs off and completes
  within the hard managed total.
- Prove Start atomically obtains the maximum phase-minimum progress lane and
  that its capacity converts into ordinary leases without double accounting.
- After Start, withdraw every byte of borrowed import capacity while injecting
  foreground viewer demand; prove the import continues through its one-task
  lane, later expands again, and emits no terminal capacity problem.
- Force queue saturation, descriptor pressure, reverse worker completion, and
  transient lease refusal; prove each becomes bounded drain/retry rather than
  a user-facing failure.
- Prove changing a removed local memory choice is no longer part of behavior.
- Prove a foreground dataset request obtains its exact reserve while an import
  is active and that import resumes after capacity release.
- Compare bit-packed reconstruction with an independent six-connected oracle,
  including geometry that exercises the bounded run spool.
- Measure peak coordinator bytes, task count, queue depth, descriptors, and
  scratch bytes against declared ceilings.
- Prove cancellation and source mutation wake blocked admission and join all
  workers without publishing.

### Performance checks

- Prove setup inspection reads no pixel payload and hashes no complete source
  file.
- Prove each admitted native TIFF chunk is decoded once, including automatic
  no-data mode and checkpoint resume.
- Prove the canonical source digest, base/pyramid production, and encoding add
  no duplicate source decode or decode/re-encode publication loop.
- Compare five-run release medians with the clean predecessor for every
  predecessor-supported declared workload; investigate any regression above
  five percent rather than averaging it away.
- Confirm the importer expands to useful multicore concurrency when CPU-bound,
  remains sequential enough for large same-device I/O, and records why it is
  waiting when neither occurs.
- Attribute wall time among inspection, native decode, cache I/O, mask,
  pyramid, encoding, validation, synchronization, and capacity wait so a fast
  synthetic case cannot hide a slow real phase.

### End-to-end package checks

- Launch the normal executable with no dataset and observe the usable welcome
  shell.
- Cancel Load and remain in the app; then open a strict existing package.
- Preprocess generated fixtures for all three source kinds through the real
  wizard path.
- Preprocess a multichannel, multi-timepoint folder-of-3D-files fixture with
  unrelated channel filenames.
- Reopen the package and independently verify exact `T/C/Z/Y/X`, dtype,
  calibration, channel labels, representative values, no-data validity, and
  deterministic time/Z order.
- Hash test fixture source bytes before and after the workflow solely as an
  independent nonmutation oracle; product setup does not perform that test
  hash.
- Exercise cancellation/resume, source change, insufficient scratch space,
  destination-exists, and publication failure; no case exposes a partial
  package.
- Run import once from the empty shell and once while a dataset is open;
  confirm the UI remains responsive and foreground rendering retains priority.

Automation must observe actual shell, inspection, validation, import,
publication, and reopen state plus independently checked output. A screenshot
of a wizard or a self-reported success field is insufficient. Final product
validation uses the normal native application on a real display; it does not
reuse the quarantined linked-rendering stress workflow.

Use focused package checks while iterating, then the changed-boundary
format/import/product checks from `docs/TESTING.md`, `cargo xtask docs-check`,
and the public PR gate. A package metadata change also requires the format
lifecycle lane.

## Risks And Mitigations

- **Optional dataset assumptions:** many app fields currently assume a live
  catalog/runtime. The shell/session type split must make invalid access
  unrepresentable rather than adding repeated `if dataset` patches.
- **Cross-channel mapping mistakes:** one immutable manifest and independent
  coordinate oracle prevent later rediscovery or filename coupling.
- **Long metadata scans:** asynchronous bounded inspection, first-mismatch
  termination, real progress, and stale-result suppression preserve UX.
- **Resource starvation or deadlock:** one coordinator, strict priority order,
  declared foreground reserve, contiguous ordered admission, generation
  wakeups, and no nested lease inversion are required before parallel import
  is enabled.
- **A run starts but cannot progress:** one pre-acquired maximum phase-minimum
  progress lane is held for the run and converted, not double-charged, into
  task/result/commit leases.
- **Overengineered resource management:** the broker owns bytes, a reserve,
  and wakeups only. Existing worker owners retain scheduling and cancellation.
- **Mask component explosion:** bit-packed state, bounded active run labels,
  and the deterministic run spool keep RAM finite without changing
  connectivity semantics.
- **Parallel I/O becoming slower:** phase-specific concurrency, small leased
  queues, sequential manifest order, and component timings expose and prevent
  source/cache/destination thrashing.
- **Performance mythology:** exact decode/read/copy counters and same-host
  release medians are required; thread count or CPU utilization alone is not
  accepted as improvement.
- **Source mutation:** descriptor generation checks bind inspection, ingest,
  checkpoint, and final publication even without a setup-time full hash.
- **Channel-label drift:** package metadata is the sole durable authority;
  wizard drafts cease to matter after publication.
- **Format accidental compatibility paths:** optional label absence has one
  defined semantic meaning; no version switch, migration, or alternate reader
  is added.
- **Over-testing:** retain a small set of independent source, memory, package,
  and normal-product checks. Test count is not an acceptance metric.

## Explicitly Out Of Scope

- Float16 TIFF admission, storage, rendering, analysis, or conversion.
- Mixed-dtype channel publication.
- First/last filename display, filename-range preview, manual sequence
  reordering, or natural-sort controls.
- Recursive directory import or arbitrary loose-plane `T x Z` hierarchy.
- Filename naming conventions or automatic channel/time inference.
- Non-TIFF source formats.
- New TIFF compression codecs or relaxed native decoder workspace bounds.
- Rendering, LOD, playback, or GPU-residency changes.
- Compression-ratio or storage-layout optimization.
- Segmentation or learned no-data detection.
- Rewriting or migrating existing complete packages.

## Completion Standard

This cutover is complete only when the normal app starts without a dataset;
all three explicit source kinds work in multichannel workflows; channel
validation and labels survive reopen; setup performs no full source-data pass;
the working-memory control and five-percent import cap are gone; the diagnosed
representative import completes through bounded backpressure; automatic-mask
memory is bit-packed and bounded; publication remains atomic and
source-preserving; native source chunks decode once; setup and import perform
no redundant raw-source pass; predecessor-supported release workloads remain
within the five-percent median no-regression boundary unless the owner
explicitly accepts an exact tradeoff; the new package auto-opens; predecessor
paths are deleted; an active run produces no user-facing failure for ordinary
memory, queue, descriptor, scheduling, or viewer contention; focused and
repository checks pass; and the normal mapped product workflow is exercised
successfully.

## Implementation Closeout

The source cutover is implemented in the current working tree:

- normal startup creates `MiranteApplicationShell` and no longer performs a
  pre-window package selection;
- the welcome shell supports package open and preprocessing without a dummy
  dataset session;
- all product and automation import routes construct one of the three explicit
  per-channel source kinds, and the predecessor automatic layout model and
  working-memory fields are absent;
- inspection is asynchronous, metadata-only, reports real file progress, and
  drops stale row-generation results;
- user-authored channel labels round-trip through canonical optional display
  metadata and runtime catalog construction;
- one process CPU broker owns the total, foreground reserve, background
  borrowing, and non-stealable run-scoped progress lane;
- ordered worker admission drains and retries transient capacity refusal;
- canonical ingest decodes native chunks once and accumulates the decoded-
  source identity without a raw-file hash pass;
- automatic reconstruction uses row-packed bits, fixed-size spill I/O, a
  bounded in-memory run window, and worst-case spill-space preflight; and
- successful shell preprocessing consumes the validated publication transfer
  and opens the resulting package.

Automated verification passed for the focused import pipeline, no-data,
runtime-ledger, application, egui, native-app, storage lifecycle, and `xtask`
suites. Product automation script schema 10 records the explicit TIFF source
kind and contains no removed working-memory command field. On 2026-08-01 the
owner reported that the completed normal preprocessing workflows worked
correctly and explicitly accepted the result as validated. That owner
attestation closes the welcome/wizard product requirement without exposing
private source facts or creating a broader release-performance claim.
