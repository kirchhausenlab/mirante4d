# Release

Mirante4D has no supported public release. The current packaging path builds a
local Linux x86_64 release candidate for validation.

## Build

Start from a clean committed checkout. Install `cargo-deny`, `appimagetool`,
`appstreamcli`, `tar`, `sha256sum`, and `ldd` (`libc-bin` on Ubuntu/Debian),
then run:

```bash
MIRANTE4D_APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
  cargo xtask package-linux-release
```

Outputs under `target/mirante4d/dist/` include an AppImage, tarball, unpacked
release directory, contents report, and three smoke logs. The report and smoke
logs sit beside the distributable artifacts; they are not packaged inside
them. The command runs the dependency-policy check before building, and records
the full source commit and tree in the report. A dirty checkout or missing
checker is a hard failure.

## Current Boundary

- Linux x86_64 is the only package target.
- There is no auto-updater, signed channel, Windows/macOS package, or release
  support window.
- A successful build or packaging smoke is not a supported release and is not
  product validation.
- The contents report records the exact source commit and tree, artifact
  digests, dependency result, and the three package smoke results.

Application logs default to
`$XDG_STATE_HOME/mirante4d/mirante4d.log` or
`~/.local/state/mirante4d/mirante4d.log`.

WP-14 validates this local package path. It does not create a supported public
release or change the platform boundary above.
