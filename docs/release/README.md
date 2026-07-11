# Release

Mirante4D has no supported public release yet. The current packaging path is a
Linux x86_64 development/release candidate used for local validation.

## Build A Local Package

Install `cargo-deny`, `appimagetool`, `tar`, and `ldd` (`libc-bin` on
Ubuntu/Debian), then run:

```bash
MIRANTE4D_APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
  cargo xtask package-linux-release
```

Outputs are written under `target/mirante4d/dist/` and include an AppImage,
tarball, unpacked directory, contents report, and smoke evidence.

The command runs the dependency-policy check before building, so a missing
`cargo-deny` is a hard failure rather than a skipped release check.

## Current Boundary

- Linux x86_64 is the only current package target.
- Updates are manual; there is no auto-updater or signed release channel.
- A package build is not a public release by itself.
- Release claims require the exact package smoke, dependency, integrity, and
  product-open evidence defined by the active testing policy.

Logs default to `$XDG_STATE_HOME/mirante4d/mirante4d.log` or
`~/.local/state/mirante4d/mirante4d.log`.
