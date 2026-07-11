# Source-fixture tooling

This isolated tooling builds the four approved small synthetic TIFF source
families. The producer writes bytes, the Rust fact oracle derives facts without
opening TIFF, and the Pillow/lxml reader independently observes the result.
None imports Mirante4D production code.

Reproduce twice and update the checked-in archive and manifest:

```sh
/usr/bin/python3.12 tools/source-fixtures/reproduce.py \
  --work-directory /tmp/mirante4d-source-fixture-evidence \
  --write-repository
```

Validate schema, archive semantics, lineage pins, and negative controls:

```sh
/usr/bin/python3 tools/source-fixtures/validate.py --self-test
```

The reproducer downloads only the exact SHA-256-bound wheels and OME XSD. It
requires the repository's exact Rust 1.96.1 toolchain and approved Ubuntu
CPython 3.12.3 binary.
