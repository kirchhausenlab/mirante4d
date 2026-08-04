# Release

Mirante4D has no supported public release. It has one local pre-alpha Linux
x86_64 release-candidate path for maintainer and research use.

## Build

Start from a clean committed checkout. Install `cargo-deny`, `appimagetool`,
`appstreamcli`, `tar`, `sha256sum`, and `ldd`. Then run:

```bash
MIRANTE4D_APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
  cargo xtask package-linux-release
```

The command builds these sibling outputs under `target/mirante4d/dist/`:

- an unpacked release directory;
- an AppImage;
- a tarball;
- an exact contents report; and
- release-directory, AppImage, and tarball smoke logs.

Reports and logs are not inside the distributable package. The build records
the exact clean commit and tree and checks dependency policy before packaging.

## Product Checks

Run the render-mode check against the unpacked executable:

```bash
MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=/absolute/path/to/mirante4d-app \
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate target_fixture_render_modes
```

Run the bounded package lifecycle check against the same executable:

```bash
MIRANTE4D_PRODUCT_VALIDATE_APP_BINARY=/absolute/path/to/mirante4d-app \
MIRANTE4D_PRODUCT_VALIDATE_DISPLAY_CLASS=real_display \
  cargo xtask product-validate pre_alpha_reliability
```

The lifecycle check uses three normal process launches. It creates a durable
provisional autosave, applies external termination only after the checkpoint,
requires explicit recovery on the next launch, and then requires one clean
window-manager close. It rejects fallback termination, missing recovery,
panic output, source mutation, and a nonzero clean close.

## Current Boundary

- Linux x86_64 is the only package target.
- Import and create-only publication require Linux kernel 5.8 or newer.
- A Vulkan-capable GPU and driver are required for the interactive viewer.
- There is no updater, signed channel, support window, Windows package, macOS
  package, or 4K qualification.
- A successful package build or smoke check is not a supported release.
- A package product check qualifies only its stated revision, workload,
  display, and hardware.

Application logs use `$XDG_STATE_HOME/mirante4d/mirante4d.log` or
`~/.local/state/mirante4d/mirante4d.log`.

The distributed platform note is
[`packaging/PLATFORM_SUPPORT.md`](../packaging/PLATFORM_SUPPORT.md). This file
owns repository release status.
