# Current State

Last reviewed: 2026-08-04

Mirante4D is public pre-alpha research software. It has no supported public
release and no public full microscopy dataset. Persisted formats and APIs can
change through explicit hard cuts.

## Product

- The product is a native Rust desktop application for Linux x86_64.
- The normal process can start without a dataset.
- The welcome window can open a strict `.m4d` package or start TIFF
  preprocessing.
- The viewer supports MIP, DVR, and ISO intensity rendering.
- Rendering supports multiple visible channels, orthographic and perspective
  projection, voxel-exact and smooth-linear sampling, and flat or lit ISO.
- The workbench provides linked XY, XZ, and YZ views with a separate 3D view.
- Time-series playback uses a bounded fixed-quality session.
- Exact analysis provides whole-layer time traces and numeric box statistics.
- Tables and plots are durable project artifacts and support CSV copy.
- The product can build a local Linux release directory, tarball, and
  AppImage.
- Segmentation and derived labels are absent.

## Data And Import

The product opens only the experimental `m4d-science-1.0` semantic schema in
the `m4d-zarr3-local-1.0` OME-NGFF 0.5.2 and Zarr v3 sharded storage profile.
The active intensity dtypes are `uint8`, `uint16`, and finite `float32`.

Import accepts grayscale TIFF and OME-TIFF data. Each named channel has one
explicit source kind:

- one 3D TIFF;
- a folder whose immediate 3D TIFF children are timepoints; or
- a folder whose immediate single-page TIFF children are Z planes.

Traversal is non-recursive. Lexical child order defines time or Z order.
Filenames carry no channel, time, or Z meaning. All channels must have the
same logical shape and dtype.

The admitted TIFF compression families are uncompressed, LZW, current or old
Deflate, and PackBits. JPEG, WebP, Zstd-in-TIFF, fax, and other unaudited
decoder workspaces fail as unsupported input.

Import is bounded, cancellable, restartable, and create-only. One canonical
owner produces and commits the current `(timepoint, channel)` unit. At most
one surplus-resource worker can decode the next unit into its ordinary cache.
The future worker has no hash, spool, shard, journal, or publication
authority. A constrained machine uses the same width-one path automatically.

One process CPU broker protects managed memory and the active import progress
lane. The product has no import-specific memory or worker selector. Disk
admission reserves only bounded unfinished work and finalization headroom.
Package-size estimates are guidance, not whole-package reservations.

An incomplete import keeps one resumable final-layout stage. A capacity pause
keeps the checkpoint and offers Resume. Corrupt or mismatched state uses a
separate confirmed Reset and Restart action. Validation completes before one
create-only publication makes the destination visible.

Automatic no-data detection reads channel zero at timepoint zero. It can find
an exact typed value from a uniform `5 x 5 x 5` seed and reconstruct each
face-connected equal-value component that contains a seed. Disconnected
equal-valued data remains valid. Constant Z-plane hiding is independent and
plane-local. The resolved mask and planes apply to every channel and
timepoint. A normal no-match produces an all-valid package.

Imported spatial pyramids halve spatial dimensions until the coarsest shape
has a maximum dimension of 64 or less and at most 262,144 voxels. Time is not
reduced. Validity-aware levels use valid-only means and centered transforms.

## Storage And Runtime

`mirante4d-storage` owns package admission, bounded reads, explicit package
audits, and create-only publication. Ordinary open validates the package
structure. It does not scan every payload before the user can work. Each
consumed object still receives checksum and mutation checks. A user can start
a separate cancellable full package-consistency audit.

The dataset runtime owns one byte-accounted scheduler for viewer, playback,
histogram, readout, and analysis requests. It deduplicates shared work,
supports scoped cancellation, suppresses stale results, and preempts lower
priority work for admitted foreground demand. Capacity and source faults are
typed. They do not select an unreported dense, CPU, or legacy path.

## Viewer And Renderer

`mirante4d-render-wgpu` is the only product renderer. The unpublished CPU
reference renderer is test-only. The GPU renderer owns one global bounded
residency system, segmented storage-buffer arena, exact resident map, page
directory, staging pool, pin counts, eviction, and compaction.

GPU payload capacity is separate from physical startup commitment. Startup
commits a small bounded arena and grows it only after exact placement
preflight. A placement failure reports aggregate free bytes, the largest
contiguous span, and segment state. The renderer can compact within its
bounded scratch allowance and retry once. A remaining refusal returns to the
normal generic LOD selector.

The application builds semantic demand. The renderer alone owns GPU reuse,
submission, completion, target fronts, texture swaps, picks, captures, and
presented currentness. Complete old pixels remain visible while a hidden
replacement is incomplete.

Static 3D navigation selects the finest complete resident native-resolution
body that fits the interaction work limit. A deterministic terminal-to-fine
ladder supplies coarser choices. One gesture keeps one selected body. It does
not change resolution or LOD because timing or resource arrival changes.

Linked 2D planes can use complete coarser coverage while selected target
bricks arrive. Voxel-exact sampling uses one scale. Smooth-linear sampling
uses one complete interpolation footprint and retries the whole footprint at
a coarser scale when needed. Invalid fine data remains invalid.

One composed-presentation scheduler owns semantic transactions across time,
3D position, linked position, and retained quality. It requires all affected
active targets before publication. The renderer derives the physical delta
and submits the coordinated color work once. Pause and Stop retain the last
complete playback front until a stationary replacement is ready.

Initial color pipelines compile asynchronously. Plane, MIP, DVR, ISO, Mixed,
and Pick have explicit capability state. A local Pick failure cannot disable
ready color. A device-level failure uses one first-cause terminal latch. The
product has no device-recovery epoch or CPU rendering fallback.

The render API owns the binary32 affine, coordinate, camera, ray, page, and
work envelopes shared by planning and shaders. Exact, smooth, color, coverage,
validity, and Pick paths use the same admitted numerical boundary.

## Analysis And Persistence

Analysis computes exact statistics from source voxels, not displayed pixels.
It uses the shared dataset scheduler with bounded lower-priority work. A table
and plot become visible only after one atomic project-store commit.

The project store uses immutable objects and generations, atomic references,
bounded closure, and held leases. One application service owns New, Open,
Save, Save As, autosave, recovery, dirty close, and shutdown.

Earlier provisional autosaves appear as an explicit recovery choice. A
recovered branch opens dirty and still needs Save or Save As. Startup never
silently opens, advances, repairs, or deletes it.

Native window close uses one exit-time cancellation and join path. A dirty
close asks for Save, Discard, or Cancel. SIGINT and SIGTERM request the same
normal joined shutdown without a second guardian or kill path.

## Verification Boundary

Public pull requests require `PR / policy` and `PR / rust`. Hosted work uses
standard free public runners without caches or artifacts. Focused development
uses package tests. GPU, product, package, performance, filesystem, and
power-cut checks remain explicit affected-boundary local work.

The clean trusted GPU correctness lane has one exact 25-case inventory and
passes all 25 cases serially with zero retries. The normal mapped render-mode
and native-navigation scenarios also pass as automated product evidence.
These results are not owner visual acceptance.

The project-store trusted local lane passes its four accepted-filesystem
workflows, three fresh-process matrices, and 60 rootless-KVM ext4 power-cut
cases. The public lane retains portable unsupported-filesystem contracts.

The initial accepted GPU performance baseline is still pending. The current
absolute interaction smoke meets its named gates, but one clean component
activation found the eight-channel voxel-exact ISO case above the 33.3 ms
feasibility limit. No accepted comparison baseline or general performance
improvement claim exists.

[Testing](TESTING.md) owns commands, triggers, thresholds, and claim language.

## Known Limitations

- The dataset and project formats are experimental and have no compatibility
  promise.
- Linux x86_64 is the only package target.
- TIFF import and create-only dataset publication require Linux kernel 5.8 or
  newer.
- Writable project durability is qualified only for the accepted Linux ext4
  boundary.
- There is no supported release, update channel, Windows or macOS package, or
  4K qualification.
- There is no segmentation or derived-label subsystem.
- There is no GPU device recovery or CPU viewer fallback.
- Product rendering is bounded to 1920x1080, 64 active layers, 65,536
  requirement records per presentation, and configured device limits.
- Smooth-linear exact refinement can be much slower than voxel-exact
  refinement on representative large data.
- The linked-S0 host-stress diagnostic is quarantined after one run froze the
  desktop. It provides no valid monitor-continuity or LOD-transition
  performance claim.
- Static multichannel performance recovery, a suitable temporal closeout
  workload, the accepted GPU baseline, and owner viewer acceptance remain
  open.
- Rendered-viewport statistics are absent. Analysis uses exact source voxels.
- Package self-consistency does not prove biological authorship, provenance,
  or external scientific correctness.
- Project maintenance and Purge UI are absent.

[Current work](planning/NOW.md) owns the unfinished viewer outcomes.
