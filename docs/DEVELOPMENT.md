# Development

Mirante4D currently develops and packages on Linux x86_64.

## Setup

1. Install Git and Linux build dependencies. On Ubuntu/Debian:

   ```bash
   sudo apt-get update
   sudo apt-get install -y build-essential pkg-config python3 libgtk-3-dev \
     libudev-dev libxcb-render0-dev libxcb-shape0-dev \
     libxcb-xfixes0-dev libxkbcommon-dev libx11-dev
   ```

2. Install Rust through `rustup` and clone the repository. The checkout selects
   the pinned `rust-toolchain.toml` toolchain.
3. Install the tools pinned by the verification registry:

   ```bash
   cargo install cargo-nextest --version 0.9.138 --locked
   cargo install rumdl --version 0.2.30 --locked
   cargo install cargo-deny --version 0.20.2 --locked
   ```

Running the application also requires a working Vulkan-capable graphics
driver. TIFF import and create-only dataset publication require Linux kernel
5.8 or newer; older kernels are rejected before a private publication stage is
created because their `syncfs` result does not reliably report writeback
failures.

## Commands

Run the generated development dataset:

```bash
cargo xtask run-dev
```

Run the ordinary application without requiring a dataset at startup:

```bash
cargo run --release -p mirante4d-app
```

Run the current PR profile or one focused leaf:

```bash
cargo xtask verify-pr
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
```

Check generated verification files or documentation only:

```bash
cargo xtask verification-sync --check
cargo xtask docs-check
```

Discover the complete current command surface from the executable authority:

```bash
cargo xtask --help
```

`verify-pr policy` and `verify-pr rust` run one public group. The protected
repository requires the matching `PR / policy` and `PR / rust` checks.

Trusted GPU verification is separate and requires the designated Vulkan
workstation:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu
```

For viewer/rendering work, the focused release diagnostics and normal-product
scenarios are:

```bash
cargo test -p mirante4d-app \
  selected_vulkan_adapter_reports_typed_device_local_memory \
  -- --ignored --nocapture --test-threads=1
cargo test -p mirante4d-render-wgpu \
  segmented_payload_upload_and_sampling_cross_the_first_binding \
  -- --ignored --nocapture --test-threads=1
cargo test -p mirante4d-render-wgpu \
  coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame \
  -- --ignored --nocapture --test-threads=1
cargo test -p mirante4d-render-wgpu \
  volume_page_traversal_ \
  -- --ignored --nocapture --test-threads=1
cargo test --release -p mirante4d-render-wgpu \
  resident_coordinated_volume_gpu_timing -- --ignored --nocapture
cargo test --release -p mirante4d-render-wgpu \
  native_1080p_terminal_navigation_gpu_timing \
  -- --ignored --nocapture --test-threads=1
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    /absolute/path/to/dataset.m4d representative_native_navigation
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    /absolute/path/to/time-series.m4d representative_temporal_playback
cargo xtask viewer-oblique-continuity \
  --workflow linked-lod-diagnostic \
  --dataset /absolute/path/to/dataset.m4d \
  --duration-secs 30 --runs 1 \
  --allow-host-stress
cargo xtask viewer-oblique-continuity \
  --workflow zoom \
  --dataset /absolute/path/to/dataset.m4d \
  --duration-secs 30 --runs 3 --skip-build
cargo xtask viewer-oblique-continuity \
  --workflow combined \
  --dataset /absolute/path/to/dataset.m4d \
  --duration-secs 300 --runs 1 --skip-build
```

The ignored timing tests are component diagnostics of resident sparse and
single-resource terminal rendering. The terminal fixture measures
voxel-exact and smooth-linear MIP/DVR/ISO at 1920×1080. The render-modes
command uses the normal release application for independent pixel and mode
facts. The former representative three-session and command-driven
resident-navigation cadence scenarios were deleted because they did not
establish visible continuity.

The `volume_page_traversal_` filter runs the four registered trusted-GPU
binary32 boundary cases. The full private A/B/C static-recovery campaign is
documented in [Testing](TESTING.md); it requires three clean overlay commits,
the designated workload and workstation, and
`MIRANTE4D_XTASK_ALLOW_STATIC_RECOVERY=1`. Do not substitute an ordinary dirty
working tree or a smaller public fixture for that evidence gate.

`representative_temporal_playback` requires a multi-timepoint package. It
uses 70 normal-product commands to exercise direct selection, fixed-scale
standalone and coordinated four-panel playback, seven paired stationary versus
two-second held-input phases across 3D and linked controls, two FPS settings,
and retained-front Pause/Stop teardown. It requires received material input,
temporal commits in both halves of every gesture, bounded maximum gaps,
coherent target groups, and exact Stop scale traces. It also redirects the
application runtime log into the bounded scenario output and fails on
renderer/runtime errors; a green automation report beside a dirty runtime log
is not accepted.

`linked-lod-diagnostic` drives real bounded Shift-drag at exact S3, S1, and S0
and records causal boundaries, but it does not observe monitor visibility or
apply a performance threshold. Its first S0 attempt froze the desktop, so it
is quarantined by default. Do not run it unattended; the shown
`--allow-host-stress` flag is an explicit acknowledgement for a later
owner-approved controlled run, not a safety guarantee. `zoom` requires both a
feasible finer 3D display and an aggregate-capacity adaptive boundary under
real wheel input; `combined` adds real resize and orbit round trips. Omit
`--skip-build` for the first relevant workflow after changing product source.

The project-store power-cut check is reserved for changes to its qualified
durability boundary. Do not rerun it for unrelated work:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local project-store-lifecycle
```

This lane is local-only and is not a GitHub Actions requirement.

The exhaustive project-store matrices and redundant imported-publication
variants are explicit developer-local checks:

```bash
cargo test -p mirante4d-project-store matrix -- --ignored
cargo test -p mirante4d-app imported_ -- --ignored
```

Run them only after changing those boundaries.

The bounded target-package admission and explicit-audit scenario is retained
as a small regression check for storage-source changes. It proves ordinary
idle starts no audit, then exercises user-requested cancellation and
self-consistency completion:

```bash
cargo xtask product-validate target_package_integrity_audit
```

Import/preprocessing changes also run the native generated-source scenario:

```bash
cargo xtask product-validate import_preprocessing
```

For deliberate local import diagnostics or a current claim, use the release
`import-performance-t2` or private `import-performance-t5` command documented
in [Testing](TESTING.md). These are changed-boundary or qualification tools,
not ordinary edit-loop checks.
The predecessor private T5 configuration pin is intentionally absent after the
compositional storage/checkpoint cutover. Do not treat a diagnostic run as a
current qualification or repin it without explicit owner review.
Generated sources, packages, and evidence stay below ignored
`target/mirante4d/` paths unless an explicit qualified scratch root is selected.

Native project-persistence automation is retained for changes to that product
path, not as a recurring acceptance ritual.

## Working Rules

- Keep generated packages, private microscopy data, logs, and evidence under
  ignored local paths, never in the repository.
- Use focused checks while iterating, then run the checks relevant to the
  affected boundary.
- Reserve exhaustive process, crash, and durability matrices for changes to
  the boundary they own.
- Add a dependency only for a clear current need. Run
  `cargo xtask verify-deps`; exact exceptions live only in the
  [exception ledger](DEPENDENCY_EXCEPTIONS.md).
- Run `cargo fmt --all` for Rust changes and `cargo xtask docs-check` for
  documentation changes.
- Rendering, loading, GPU, interaction, and large-data changes require the
  real-product validation described in [testing](TESTING.md).
- Follow the high-risk entry workflow in [the agent guide](AGENTS.md) for
  architectural or broad corrective work.

Current packaging status and the local release-candidate command are in
[release](RELEASE.md).
