# Packaging Platform Support

Mirante4D currently builds local pre-alpha packages for Linux x86_64. There is
no supported public release.

## Linux x86_64

The package requires:

- an x86_64 Linux system;
- a Vulkan-capable GPU and driver for the viewer; and
- Linux kernel 5.8 or newer for TIFF import and create-only publication.

The unpacked release directory contains:

- `mirante4d-app`;
- `README.md`;
- `LICENSE`;
- `ASSET_PROVENANCE.md`;
- `manifest.json`;
- `THIRD_PARTY_NOTICES.md`;
- `PLATFORM_SUPPORT.md`;
- desktop, icon, and AppStream metadata; and
- `runtime-dependencies.txt`.

The AppImage, tarball, unpacked directory, exact contents report, and smoke
logs are sibling build outputs. Validation reports and logs are not packaged
as product files.

The package has no updater, signed channel, or support window. Windows,
macOS, other CPU architectures, and 4K operation are not qualified.

The source repository [Release](../docs/RELEASE.md) document owns the current
build and validation procedure.
