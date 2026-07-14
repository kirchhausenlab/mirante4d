# Current Work

Last updated: 2026-07-14

## Current Checkpoint

WP-10C is accepted on protected main at
`b9ac2a5f08101094933f80a0ce98fbdbdbe6c8d6`, tagged
`foundation-wp-10c-exit-1`. The sharded target package and bounded importer are
the sole product storage/import route; the predecessor crates are deleted.

WP-09B is a completed candidate on `wp09b-product-render-cutover`.

## WP-09B Candidate

`mirante4d-render-wgpu` is now the viewer's only renderer. It presents useful
partial/current frames, shares the window WGPU device, and owns the GPU targets.
The predecessor crate, complete-residency gate, old display identity, and CPU
placeholder rendering are deleted.

Focused app, renderer, and policy checks pass. The existing small-package
product scenario passes on a real Vulkan display for MIP, DVR, ISO, linked
panels with a 1280x720 render target, and a current nonblank 1920x1080
render-target resize. It records a useful partial frame and zero WGPU
validation errors. WP-09A qualification is inherited; its trusted-GPU matrix
was not repeated.

Remaining work is the normal review/protected-main acceptance and create-once
`foundation-wp-09b-exit-1` tag. WP-09C follows.
