# Current Work

Last updated: 2026-07-14

## Current Checkpoint

WP-09B is accepted on protected main at
`b73dd86fed8cc3ac7b34f75f20dcd8bb8ac85672`, tagged
`foundation-wp-09b-exit-1`. `mirante4d-render-wgpu` is the viewer's only
renderer; the predecessor renderer, complete-residency gate, old display
identity, and CPU placeholder route are deleted.

## WP-09C Candidate

The visible workbench now enters `mirante4d-ui-egui` once with an immutable
application snapshot and small projected facts. The UI owns layout and
interaction, then returns typed commands, requests, and opaque presentation
paints. The native app pumps services and resolves that output; it no longer
draws or merges individual workbench regions.

Focused UI/app tests and strict linting pass. The normal release viewer also
opened the bounded target fixture on the mapped Vulkan display, exercised the
render modes and four-panel layout at 1280x720, and remained usable at
1920x1080. Earlier project durability, import, analysis, runtime, and GPU
qualification is inherited rather than repeated. The candidate still needs the
ordinary PR checks, protected-main acceptance, and its create-once exit tag;
WP-14 follows.
