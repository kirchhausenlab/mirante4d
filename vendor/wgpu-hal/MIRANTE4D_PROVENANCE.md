# Vendored `wgpu-hal`

Mirante4D carries `wgpu-hal` 29.0.3 as a local path patch because that release
does not expose the required present-ID, present-wait, or first-pixel-out
timing feedback at its native Vulkan swapchain boundary. Mirante4D's trusted
local GPU campaign needs that feedback to establish mapped-product identity,
maximum visible stalls, settlement, and exact scanout cadence on the qualified
workstation. Ordinary product presentation does not activate the patch.

- Upstream project: <https://github.com/gfx-rs/wgpu>
- License: MIT or Apache-2.0 (`LICENSE.MIT`, `LICENSE.APACHE`)
- Source archive:
  <https://crates.io/api/v1/crates/wgpu-hal/29.0.3/download>
- Source archive SHA-256:
  `31f8e1a9e7a8512f276f7c62e018c7fa8d60954303fed2e5750114332049193f`
- Release source commit recorded by the archive:
  `4cbe6232b2d7c289b6e1a38416a6ae1461a22e81`

The crates.io archive was extracted in full. A bytewise tree comparison leaves
only the local provenance file, Cargo's extraction marker, and these Vulkan
changes:

- opt-in instance and device enablement for
  `VK_KHR_get_surface_capabilities2`, `VK_KHR_present_id`,
  `VK_KHR_present_wait`, `VK_KHR_present_id2`,
  `VK_KHR_calibrated_timestamps`, and `VK_EXT_present_timing` when a live
  local observer is installed;
- one weak, process-local observer hook around the existing native Vulkan
  `vkQueuePresentKHR` call;
- strictly increasing present IDs supplied through both present-ID structures,
  paired with first-pixel-out timing requests;
- bounded surface-capability, timing-queue, time-domain, and past-presentation
  queries;
- read-only access to the already-owned raw Vulkan instance for the bounded
  off-thread waiter; and
- a pre-destruction observer callback that lets bounded outstanding host waits
  finish before the corresponding swapchain handle is destroyed.

There is no alternate swapchain, renderer, queue, present call, or ordinary
runtime behavior. Remove this path patch when a reviewed upstream `wgpu-hal`
release exposes equivalent final-present feedback.

To recapture the source archive:

```bash
curl -fsSL \
  https://crates.io/api/v1/crates/wgpu-hal/29.0.3/download \
  -o wgpu-hal-29.0.3.crate
printf '%s  %s\n' \
  31f8e1a9e7a8512f276f7c62e018c7fa8d60954303fed2e5750114332049193f \
  wgpu-hal-29.0.3.crate | sha256sum --check
```
