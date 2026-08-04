# Development

Mirante4D develops and packages on Linux x86_64.

## Setup

On Ubuntu or Debian, install the build dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config python3 libgtk-3-dev \
  libudev-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libxkbcommon-dev libx11-dev
```

Install Rust through `rustup`. The checkout selects the toolchain from
`rust-toolchain.toml`.

Install the pinned verification tools:

```bash
cargo install cargo-nextest --version 0.9.138 --locked
cargo install rumdl --version 0.2.30 --locked
cargo install cargo-deny --version 0.20.2 --locked
```

Running the viewer requires a Vulkan-capable driver. TIFF import and
create-only publication require Linux kernel 5.8 or newer.

## Run

Open the generated development dataset:

```bash
cargo xtask run-dev
```

Open the ordinary dataset-independent application:

```bash
cargo run --release -p mirante4d-app
```

## Common Checks

Run the public profile:

```bash
cargo xtask verify-pr
```

Run one focused public leaf:

```bash
cargo xtask verify-leaf policy
cargo xtask verify-leaf lint
cargo xtask verify-leaf unit
cargo xtask verify-leaf contract
cargo xtask verify-leaf ui
```

Check documentation or generated verification metadata:

```bash
cargo xtask docs-check
cargo xtask verification-sync --check
```

Discover the complete current command surface from the executable:

```bash
cargo xtask --help
```

[Testing](TESTING.md) owns trusted GPU, product, performance, persistence,
fixture, import, and package check triggers. Do not copy those full contracts
into routine development instructions.

## Working Rules

- Keep generated packages, logs, reports, and private microscopy data under
  ignored local paths.
- Use focused checks while editing.
- Run the public profile for a substantial cross-cutting handoff.
- Run expensive local checks only when their owned boundary changes.
- Never run the quarantined linked-S0 host-stress workflow unattended.
- Add a dependency only for a current need. Run `cargo xtask verify-deps`.
- Put exact dependency exceptions only in
  [DEPENDENCY_EXCEPTIONS.md](DEPENDENCY_EXCEPTIONS.md).
- Run `cargo fmt --all` for Rust changes.
- Run `cargo xtask docs-check` for documentation changes.
- Use the high-risk planning workflow in [AGENTS.md](AGENTS.md).

Current package commands and support limits are in [Release](RELEASE.md).
