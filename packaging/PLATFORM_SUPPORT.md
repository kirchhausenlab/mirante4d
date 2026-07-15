# Packaging Platform Support

Mirante4D currently has a Linux x86_64 release-candidate package path through:

```bash
MIRANTE4D_APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
  cargo xtask package-linux-release
```

`cargo xtask package-dev` is retained as an alias that returns the release directory path.

## Linux

Status: current release-candidate package target for x86_64 artifacts. There is
no supported public release yet.

Minimum verified environment:

- Ubuntu 24.04-family Linux development machine
- x86_64 CPU architecture
- Vulkan-capable GPU/driver for the interactive viewer
- clean committed Git worktree
- `cargo-deny`, `appstreamcli`, `tar`, `sha256sum`, and `ldd`
- `appimagetool` available on `PATH` or through `MIRANTE4D_APPIMAGETOOL`

The unpacked Linux release directory includes:

- release `mirante4d-app` binary
- `README.md`
- project `LICENSE`
- `ASSET_PROVENANCE.md`
- `manifest.json` with version, target, native format/schema, and release identity
- `THIRD_PARTY_NOTICES.md`
- `PLATFORM_SUPPORT.md`
- `share/applications/org.kirchhausenlab.Mirante4D.desktop`
- `share/icons/hicolor/scalable/apps/mirante4d.svg`
- `share/metainfo/org.kirchhausenlab.Mirante4D.appdata.xml`
- `runtime-dependencies.txt` from `ldd`

The AppImage, tarball, full-commit-and-tree contents report, and
release-directory/AppImage/tarball smoke logs are sibling outputs under
`target/mirante4d/dist/`. The report and logs are validation sidecars and are
not included in the distributable release directory or tarball.

The AppImage also installs `README.md`, `LICENSE`, `ASSET_PROVENANCE.md`, and
`THIRD_PARTY_NOTICES.md` under
`usr/share/doc/mirante4d/`.

`.deb` packaging is deferred until Debian/Ubuntu integration is worth the maintenance burden.

## macOS

Status: explicitly blocked for release packaging in the current workspace.

Blockers:

- choose final bundle tool and `.app` layout
- define icon conversion pipeline for `.icns`
- add a macOS CI runner or release machine
- define signing and notarization process before external distribution
- run package smoke tests on macOS hardware with Metal backend diagnostics
- document local file-access behavior and quarantine/notarization behavior

## Windows

Status: explicitly blocked for release packaging in the current workspace.

Blockers:

- choose signed installer tooling
- define icon conversion pipeline for `.ico`
- add a Windows CI runner or release machine
- define signing certificate and signing policy
- audit runtime DLL dependencies for the chosen graphics stack
- run package smoke tests on Windows hardware with D3D12/Vulkan backend diagnostics
- verify uninstall or cleanup behavior
