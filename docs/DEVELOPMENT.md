# Development

Mirante4D currently develops and packages on Linux x86_64.

## Setup

1. Install Git and Linux build dependencies. On Ubuntu/Debian:

   ```bash
   sudo apt-get update
   sudo apt-get install -y build-essential pkg-config python3 libgtk-3-dev \
     libudev-dev libxcb-render0-dev libxcb-shape0-dev \
     libxcb-xfixes0-dev libxkbcommon-dev
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

Run the current PR profile or one focused leaf:

```bash
cargo xtask verify-pr
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
cargo xtask verify-leaf doctest
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
cargo test --release -p mirante4d-render-wgpu \
  resident_volume_gpu_timing -- --ignored --nocapture
cargo test --release -p mirante4d-render-wgpu \
  payload_buffer_vs_texture_gpu_timing -- --ignored --nocapture
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate \
    target_fixture_resident_navigation_no_readback
```

The two ignored tests are component diagnostics. The product commands use the
normal release application; the resident-navigation scenario avoids
validation capture/readback. None is a final performance qualification without
the owner-bound workload, fidelity floor, metrics, samples, and absolute
thresholds required by [Testing](TESTING.md).

The project-store power-cut check is reserved for changes to its qualified
durability boundary. Do not rerun it for unrelated work:

```bash
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local project-store-lifecycle
```

This lane is local-only and is not a GitHub Actions requirement.

The bounded target-package open and verification scenario is retained as a
small regression check for storage-source changes:

```bash
cargo xtask product-validate target_source_verification
```

Import/preprocessing changes also run the native generated-source scenario:

```bash
cargo xtask product-validate import_preprocessing
```

For local public performance diagnostics or qualification, use the release
`import-performance-t2` command documented in [Testing](TESTING.md). The same
document owns the strict private `import-performance-t5` runner and its
external configuration, real-display, raw-evidence, sanitization, and
finalized-raw report-only publication rules.
Qualification requires the exact owner-pinned local HW-2 host/ext4 profile to
resolve outside the repository; an absent, mismatched, or differently
committed profile produces diagnostic-only evidence.
Generated sources, packages, and evidence stay below ignored
`target/mirante4d/` paths unless an explicit qualified scratch root is selected.

Native project-persistence automation is retained for changes to that product
path, not as a recurring acceptance ritual.

## Working Rules

- Keep generated packages, private microscopy data, logs, and evidence under
  ignored local paths, never in the repository.
- Use focused checks while iterating, then run the checks relevant to the
  affected boundary.
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
