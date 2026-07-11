# Mirante4D

Mirante4D is a native desktop viewer for large 4D microscopy datasets. It is
an early-stage academic project developed in the Kirchhausen Lab and written in
Rust with `wgpu`, `winit`, and `egui`.

> **Status:** pre-alpha research software. The application is under active
> foundational refactoring, has no stable file-format promise, and is not yet
> distributed as a supported public release.

## Current Capabilities

- Import microscopy data into strict native `mirante4d-v1` packages.
- Stream datasets that are larger than RAM or VRAM.
- Render intensity channels with MIP, DVR, and ISO modes.
- Display multiple channels with per-channel rendering controls.
- Save native project/session state and run analysis workflows.
- Build and package the application for Linux x86_64.

Current implementation facts and limitations are recorded in
[docs/CURRENT_STATE.md](docs/CURRENT_STATE.md).

## Build And Run

Mirante4D currently targets Linux x86_64. Install the Rust toolchain selected by
`rust-toolchain.toml`, clone the repository, and run:

```bash
cargo xtask run-dev
```

This builds the application and opens a generated development dataset. For
normal development and testing commands, see
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

There is no public microscopy dataset release yet. Local sample data must stay
outside the repository.

For a bounded local check before submitting a change, run:

```bash
cargo xtask verify-pr
```

This runs the public policy and Rust checks. GPU, packaged-product,
performance, and real-data evidence remain separate trusted-local work; see
[docs/TESTING.md](docs/TESTING.md).

## Documentation

- [Product and scope](docs/PRODUCT.md)
- [Current state](docs/CURRENT_STATE.md)
- [Current work](docs/planning/NOW.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Data format](docs/DATA_FORMAT.md)
- [Testing](docs/TESTING.md)
- [Documentation index](docs/README.md)

## Related Work

Mirante4D is a native successor to the browser-based
[llsm_viewer](https://github.com/kirchhausenlab/llsm_viewer). It does not
preserve that viewer's architecture or data formats. Mirante4D is also related
to the lab's [SpatialDINO](https://github.com/kirchhausenlab/spatialdino)
research project and is briefly described in its
[bioRxiv preprint](https://doi.org/10.64898/2025.12.31.697247).

## Contributing And Citation

The project is maintainer-led and welcomes focused issues and pull requests.
See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and
[CITATION.cff](CITATION.cff).

Mirante4D is licensed under the [MIT License](LICENSE). Retained source and
visual assets, generated snapshots, dependency-provided fonts, and reviewed
fixture/vendor additions are tracked in
[ASSET_PROVENANCE.md](ASSET_PROVENANCE.md).
