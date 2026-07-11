# GPU Resident Display Renderer Specification

Status: ACCEPTED
Implementation: implemented, automated-verified, and product-validated for the
accepted current scope
Last updated: 2026-06-26

## Purpose

Define the current product renderer boundary: normal eligible interactive
display renders to renderer-owned GPU display textures and presents those
textures through the app, instead of rebuilding full-frame CPU `ColorImage`
products as the product path.

## Current Contract

- The normal product path for complete supported resident frames is
  GPU-resident display.
- CPU-visible renderer products are allowed for reference, diagnostics, export,
  benchmarks, tests, and explicit fallback/error investigation only.
- The product eframe/WGPU device must request the renderer-required resource
  limit envelope before device creation.
- Existing devices that cannot satisfy renderer requirements must fail with
  typed backend-limit diagnostics.
- Presented GPU frames are swapped atomically only after candidate render and
  egui-wgpu registration succeed.
- The last compatible presented frame may be preserved while a replacement
  resident request is loading; status must remain truthful about freshness and
  completeness.
- GPU resource accounting must include display textures, atlas residency,
  upload bytes, cache hits/misses, and relevant renderer counters.

## Supported Display Scope

The current GPU-resident product path covers:

- single-channel `MIP`, `DVR`, and `ISO`
- mixed visible channels through the display graph
- same-ray multi-channel `DVR` cohorts
- depth-sorted multi-channel `ISO` cohorts
- `uint8`, `uint16`, and supported `float32` resident intensity paths
- label and scene overlays composited over renderer-owned display textures

## Non-Goals

- Legacy CPU-visible presentation as the normal product path.
- Silent dense fallback when resident rendering is incomplete or unsupported.
- Compatibility branches for old renderer/session behavior.
- Claiming interaction smoothness from render time alone.

## Verification Requirements

Renderer changes that touch this boundary require focused renderer/app tests and
the applicable product-open validation gate from `docs/TESTING.md`.

Evidence should distinguish:

- standalone renderer GPU tests
- product-like existing-device tests
- UI/e2e/product automation reports
- real-display product-open validation
- benchmark timing and resource reports

Do not call GPU/display work complete from automated evidence alone unless the
user explicitly waives product-open validation.
