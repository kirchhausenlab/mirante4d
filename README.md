# Mirante4D

Mirante4D is a native desktop viewer for large 4D microscopy datasets. It is
an academic project from the Kirchhausen Lab. The application uses Rust,
`wgpu`, `winit`, and `egui`.

> **Status:** Mirante4D is pre-alpha research software. Persisted formats can
> change through explicit hard cuts. There is no supported public release.

## Current Capabilities

- Import TIFF and OME-TIFF data into strict, sharded Mirante4D packages.
- Keep source microscopy data unchanged during import and validation.
- Stream datasets that are larger than RAM or VRAM.
- Render intensity data with MIP, DVR, and ISO modes.
- Display multiple channels with independent controls.
- Navigate 3D and linked cross-sections through bounded multiscale data.
- Play time series through a fixed-quality bounded temporal session.
- Compute exact whole-layer time traces and numeric box statistics.
- Save and reopen project state, tables, and plots.
- Build local Linux x86_64 release candidates.

[Current state](docs/CURRENT_STATE.md) owns the exact implemented behavior and
limitations.

## Build And Run

Mirante4D currently targets Linux x86_64. Import and create-only dataset
publication require Linux kernel 5.8 or newer.

Install the Rust toolchain selected by `rust-toolchain.toml`. Then run the
generated development dataset:

```bash
cargo xtask run-dev
```

Start the ordinary application without a dataset:

```bash
cargo run --release -p mirante4d-app
```

The welcome window can open an existing `.m4d` package or configure an
explicit per-channel TIFF source.

Run the public pull-request checks with:

```bash
cargo xtask verify-pr
```

GPU, performance, package, and real-data checks are separate local work. See
[development](docs/DEVELOPMENT.md) and [testing](docs/TESTING.md).

There is no public microscopy dataset release. Keep local research data
outside the repository.

## Documentation

- [Product and scope](docs/PRODUCT.md)
- [Current state](docs/CURRENT_STATE.md)
- [Current work](docs/planning/NOW.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Data format and safety](docs/DATA_FORMAT.md)
- [Testing and validation](docs/TESTING.md)
- [Documentation index](docs/README.md)

## Related Work

Mirante4D is a native successor to
[llsm_viewer](https://github.com/kirchhausenlab/llsm_viewer). It does not use
that viewer's architecture or data formats. Mirante4D is also related to the
lab's [SpatialDINO](https://github.com/kirchhausenlab/spatialdino) research
project and its
[bioRxiv preprint](https://doi.org/10.64898/2025.12.31.697247).

## Contributing And Citation

The project welcomes focused issues and pull requests. See
[CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and
[CITATION.cff](CITATION.cff).

Mirante4D uses the [MIT License](LICENSE). Asset and vendor records are in
[ASSET_PROVENANCE.md](ASSET_PROVENANCE.md).
