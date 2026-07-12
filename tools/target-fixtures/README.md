# Target-format authority tooling

This directory is isolated verification tooling for WP-10A-C. It does not
import Mirante4D production crates and is not reachable from the product.

Verify the checked offline standards mirror:

```sh
/usr/bin/python3 tools/target-fixtures/standards_check.py
```

Reproduce the first external-reader capability probe:

```sh
/usr/bin/python3 tools/target-fixtures/reader_probe/reproduce.py
```

The probe builds one tiny Zarr-v3 array manually, twice, then reads both copies
with the hash-locked zarr-python environment. It proves only that this external
reader decodes the selected indexed-sharding, bytes, zstd, and CRC32C subset.
It is not a promoted T1 fixture, complete M4D package, OME/IO-3 validation,
identity proof, product test, or performance result.

`standards_check.py --fetch` is the explicit maintainer-only operation that
retrieves the immutable upstream bytes named by the accepted manifest. Normal
verification is offline and rejects missing, extra, or changed artifacts.
