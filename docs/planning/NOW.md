# Current Work

Last updated: 2026-07-14

## Current Checkpoint

WP-09B is accepted on protected main at
`b73dd86fed8cc3ac7b34f75f20dcd8bb8ac85672`, tagged
`foundation-wp-09b-exit-1`. `mirante4d-render-wgpu` is the viewer's only
renderer; the predecessor renderer, complete-residency gate, old display
identity, and CPU placeholder route are deleted.

## WP-09C Active

WP-09C makes egui the visible shell rather than an application runtime. UI code
will read immutable application snapshots, emit typed commands, and request
paint through opaque presentation tokens. Import, analysis, project I/O,
render coordination, worker lifetime, and WGPU resources will remain behind the
application/composition boundary.

Native presentation ownership, the UI-only crate, the command/snapshot surface,
and import/render coordination are in place. The temporary UI/import/render
owners and shell bridge are deleted. The remaining cutover moves the visible
workbench to one snapshot-in, typed-output-out UI entry point and shrinks the
native app to process composition. Acceptance is focused boundary testing plus
one bounded native small-fixture scenario at 1280x720 with a short 1920x1080
check. Earlier durability, science, runtime, and GPU qualification is inherited
rather than repeated.
