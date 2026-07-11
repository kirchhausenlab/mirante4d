# Release

Mirante4D has no supported public release. The current packaging path builds a
local Linux x86_64 release candidate for validation.

## Build

Install `cargo-deny`, `appimagetool`, `tar`, and `ldd` (`libc-bin` on
Ubuntu/Debian), then run:

```bash
MIRANTE4D_APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
  cargo xtask package-linux-release
```

Outputs under `target/mirante4d/dist/` include an AppImage, tarball, unpacked
release directory, contents report, and smoke evidence. The command runs the
dependency-policy check before building; a missing checker is a hard failure.

## Current Boundary

- Linux x86_64 is the only package target.
- There is no auto-updater, signed channel, Windows/macOS package, or release
  support window.
- A successful build or packaging smoke is not a supported release and is not
  product validation.
- Release claims bind the exact source revision, package digest, dependency
  result, integrity result, packaged-product run, and evidence set.

Application logs default to
`$XDG_STATE_HOME/mirante4d/mirante4d.log` or
`~/.local/state/mirante4d/mirante4d.log`.

The final release and contributor gates belong to WP-14. Their approved target
is in the [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
The WP-06A checkpoint does not change release support or qualify the package-
capability lane; that lane remains pending until it has an honest
unsupported-GPU diagnostic command.
