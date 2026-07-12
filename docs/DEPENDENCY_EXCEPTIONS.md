# Dependency Exceptions

Status: ACCEPTED
Last updated: 2026-07-11

This file records dependency-policy exceptions allowed by `cargo xtask verify-deps`.
Exceptions that are encountered by `cargo-deny` are also listed in `deny.toml`;
native-checker-only dev/test exceptions are documented here and enforced by
`xtask`.

## Active Exceptions

### `EXC-PASTE-WP10A-1` — `paste`

Allowed advisory exception:

- `RUSTSEC-2024-0436`

Exact reviewed inclusion graph:

- `paste 1.0.15`
- `zarrs 0.23.13`
- `zarrs_data_type 0.9.0`
- `zarrs_plugin 0.4.1`

The dependency gate checks the all-features metadata graph, including every
target-conditioned edge. It rejects workspace paths that bypass
`mirante4d-data` or `mirante4d-format`, and rejects any path from
`mirante4d-storage` to this exception.

Reason: the current schema-1 bridge in `mirante4d-format` and `mirante4d-data`
still needs this released Zarr graph. RustSec classifies the advisory as
unmaintained information and provides no patched `paste` release. This is not
authorization for target storage to inherit the exception; WP-10A must choose
that implementation dependency separately in its profile-freeze supplement.

Owner: Mirante4D maintainers.

Review: on every Zarr dependency update.

Expiry: when the current schema-1 path is deleted at WP-10C, or earlier if an
upstream release removes `paste`.

### `epaint_default_fonts`

Allowed additional licenses:

- `OFL-1.1`
- `Ubuntu-font-1.0`

Reason: this crate ships default UI font assets used transitively by `egui`/`eframe`. These are font asset licenses, not general-purpose code licenses. The exception is package-specific and must not be generalized without a fresh decision.

### `colored`

Allowed additional licenses:

- `MPL-2.0`

Reason: `colored` is pulled transitively by `dify`, which is used only by the dev/test `egui_kittest` screenshot comparison feature for UI visual regression tests. It is not a runtime dependency of the Mirante4D application, renderer, data engine, format, or preprocessing crates. The exception is package-specific and must be removed if the snapshot comparison stack stops requiring it or if `colored` enters a runtime dependency path.

This exception is enforced by the native `xtask` metadata checker. It is not
listed in `deny.toml` because `cargo-deny` currently does not encounter this
dev/test package in its checked graph and warns on unused license exceptions.

## Policy

- Exceptions must be specific to a package and license.
- Exceptions must include a reason.
- New exceptions require updating this file, the native `xtask` dependency-policy checker, and `deny.toml` when `cargo-deny` encounters the exception.
- Compatibility dependencies are still forbidden unless explicitly requested by the user.
