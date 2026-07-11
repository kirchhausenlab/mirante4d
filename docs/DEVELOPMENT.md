# Development

Mirante4D currently develops and packages on Linux x86_64.

## Setup

1. Install Git and the Linux build dependencies. On Ubuntu/Debian:

   ```bash
   sudo apt-get update
   sudo apt-get install -y build-essential pkg-config libgtk-3-dev \
     libudev-dev libxcb-render0-dev libxcb-shape0-dev \
     libxcb-xfixes0-dev libxkbcommon-dev
   ```

   Running the application also requires a working Vulkan-capable graphics
   driver.
2. Install Rust through `rustup`.
3. Clone the repository. Rust automatically selects `rust-toolchain.toml`.
4. Install the two temporary WP-01 verification tools:

   ```bash
   cargo install cargo-nextest --version 0.9.138 --locked
   cargo install rumdl --version 0.2.30 --locked
   ```

Use the checked-in Rust pin. The bridge tool versions are temporary and are
replaced by the final verification bootstrap in WP-06.

## Common Commands

```bash
cargo xtask verify-bootstrap
cargo xtask run-dev
```

`verify-bootstrap` runs formatting, workspace compilation, 169 selected CPU
tests, and active Markdown/link validation. It is intentionally partial and
prints the deeper evidence it does not cover. Use the relevant package and test
filter while iterating; run broader GPU, UI, E2E, packaging, or product checks
only when the change requires them.

List every developer command with:

```bash
cargo xtask --help
```

`cargo xtask verify-fast` remains a known failing legacy gate and must not be
represented as green. WP-06 replaces the temporary bridge and the legacy
verification stack.

## Product Validation

Rendering, GPU, viewport, data-loading, interaction, and large-dataset changes
require the real desktop application to be opened and exercised. Generated
fixtures are suitable for bounded correctness checks; scientific/product claims
must name the real dataset and hardware used.

```bash
cargo xtask product-validate
cargo xtask verify-render
cargo xtask verify-ui
cargo xtask verify-e2e
```

Some scenarios require a real display, GPU, local dataset, or explicit heavy-
test opt-in. Do not commit local datasets or generated `target/` evidence.

## Documentation

The normal bridge validates active Markdown and local links. To run only that
part:

```bash
rumdl check --no-cache --config .rumdl.toml .
```

Documentation-only changes do not require opening the application unless they
make or change product-validation claims.
