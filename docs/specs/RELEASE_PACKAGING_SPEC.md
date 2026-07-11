# Release And Packaging Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the accepted packaging and distribution direction for Mirante4D.

## Scope

This spec covers desktop release targets, package artifacts, app bundles,
config/cache/log locations, and release verification.

## Non-Goals

- Browser/static deployment.
- App store distribution as an initial requirement.
- User-facing CLI tools.
- Headless/server/batch product modes.

## Target Platforms

Current platform status:

- Linux x86_64 is the supported first release packaging target.
- Windows desktop packaging is intended but blocked until installer tooling,
  signing policy, runtime dependency audit, and platform smoke evidence exist.
- macOS packaging is intended but blocked until `.app` layout, icon conversion,
  signing/notarization, quarantine behavior, and platform smoke evidence exist.

Current implementation status:

- Linux release packaging is produced by `cargo xtask package-linux-release`.
- `cargo xtask package-dev` is retained as an alias that returns the Linux
  release directory path.
- Linux release outputs are written under `target/mirante4d/dist/` and include
  a release directory, AppImage, tarball, and contents report.
- The release package includes the release `mirante4d-app` binary, `README.md`,
  `manifest.json`, `THIRD_PARTY_NOTICES.md`, `PLATFORM_SUPPORT.md`, Linux
  desktop metadata, AppStream metadata, scalable icon, runtime dependency
  audit, smoke-test logs, and release report.
- The packaging gate runs dependency/advisory/license checks, builds
  `mirante4d-app` in release mode, runs `ldd` on the packaged binary, and
  fails if any required runtime dependency is missing.
- The packaging gate smoke-tests release-directory, AppImage, and tarball
  artifact paths with `MIRANTE4D_APP_SMOKE=1` against a generated strict native
  fixture and verifies the smoke output includes the packaged app version.
- The smoke path opens the dataset, renders the first frame, and initializes a
  GPU renderer when available. GPU unavailability is reported in the smoke log
  but is not a package failure by itself.
- macOS and Windows release packages remain explicitly blocked in
  `packaging/PLATFORM_SUPPORT.md`.

## Product Shape

Mirante4D should ship as a single GUI viewer application. Preprocessing, validation, diagnostics, and benchmarking features that matter to users should be accessible through the GUI unless a future explicit decision changes this.

Developer-only automation such as `xtask` is allowed, but it is not a user-facing product surface and must not become a second implementation path.

## Runtime Locations

Platform-appropriate locations are required for:

- user preferences
- cache
- logs
- benchmark outputs
- temporary preprocessing outputs

Do not scatter generated files next to source data unless explicitly requested by the user.

Packaged Linux builds expose the log path in runtime diagnostics. Linux logs
default to `$XDG_STATE_HOME/mirante4d/mirante4d.log` or
`~/.local/state/mirante4d/mirante4d.log`.

## Invariants

- End users should not need Rust, Cargo, Node, Python, or command-line setup to run the GUI release.
- Release builds must include version identity.
- Release verification must run before a release is claimed.
- Installers must not overwrite user data.

## Failure Modes

- missing GPU/runtime dependency
- unsigned macOS app blocked
- Linux graphics backend mismatch
- installer writes to wrong location
- release build differs from tested build

## Testing Requirements

- Release smoke test per target platform when packaging exists.
- Release-directory, AppImage, and tarball smoke verification for Linux.
- Version display verification.
- Config/cache/log path verification.
- Runtime dependency audit for packaged Linux binaries.
- Contents report with artifact paths, SHA-256 hashes, file sizes, required
  content checks, smoke summaries, app version, native format/schema version,
  git revision, and sample-data exclusion status.

## Open Questions

- Release cadence.
- Windows installer tooling and signing policy.
- macOS bundle tooling, signing, notarization, and quarantine behavior.
- Whether `.deb` packaging is worth the maintenance burden after AppImage and
  tarball support.
