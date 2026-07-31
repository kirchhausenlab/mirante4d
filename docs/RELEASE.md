# Release

Mirante4D has no supported public release. It has a durable local pre-alpha
Linux x86_64 packaging path for maintainer and research use.

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
- TIFF import and create-only dataset publication require Linux kernel 5.8 or
  newer; opening an existing package does not use that publication barrier.
- There is no auto-updater, signed channel, Windows/macOS package, or release
  support window.
- A successful build or packaging smoke is not a supported release and is not
  product validation.
- The contents report records the exact source commit and tree, artifact
  digests, dependency result, and the three package smoke results.

Application logs default to
`$XDG_STATE_HOME/mirante4d/mirante4d.log` or
`~/.local/state/mirante4d/mirante4d.log`.

The package path has passed the promoted small-fixture checks at 1280x720 and
1920x1080. That does not create a supported public release or broaden the
platform boundary above.

After packaging, run the existing small-fixture viewer check against the
unpacked packaged executable:

```bash
MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=target/mirante4d/dist/\
mirante4d-0.1.0-linux-x86_64-release/mirante4d-app \
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
```

This exercises MIP, DVR, ISO, linked panels, 1280x720, and a short
1920x1080 resize with the promoted small fixture. It is a local product check,
not a supported-release claim.

Then run the bounded packaged lifecycle check against that same executable:

```bash
MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=target/mirante4d/dist/\
mirante4d-0.1.0-linux-x86_64-release/mirante4d-app \
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate pre_alpha_reliability
```

`pre_alpha_reliability` uses the promoted small fixture, isolated state homes,
1280x720 mapped clients, no automatic retries, and three release-process
launches. It creates a real scheduled provisional autosave and applies
external SIGKILL only after a synced checkpoint; proves that the next launch
visibly exposes and explicitly recovers the dirty branch; then sends a real
window-manager close to a clean mapped client and requires successful exit
within ten seconds. Fallback termination, a missing recovery, a panic marker,
source mutation, or a nonzero clean-close exit fails the check. It does not
create a supported-release or crash-durability claim beyond those exact
steps.

The current local pre-alpha checkpoint passed both commands against its
unpacked packaged executable. The lifecycle run used exactly three mapped
launches and zero retries; all source comparisons were byte-identical, all
bounded stderr logs were panic-free, recovery opened dirty through the normal
actor route, and the clean native close exited successfully without fallback
cleanup. The generated contents and product-validation reports, rather than
this prose, remain authoritative for the exact commit, tree, binary path,
artifact digests, and observations.
