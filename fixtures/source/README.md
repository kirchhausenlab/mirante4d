# Synthetic TIFF source fixtures

`mirante4d-source-tiff-fixtures-v1.tar` is a small, deterministic, uncompressed
ustar archive of synthetic TIFF inputs and their fact, grouping, mutation, and
provenance records. It is source material for import tests; it is not an M4D/T1
dataset, a performance dataset, or public microscopy data.

`manifest.json` binds the archive contents, independent lineages, exact tools,
double-run reproduction, and validation facts. The checked
`independent-reader-report.json` is the exact digest-bound observation output
named by that manifest, so contract tests can consume independent facts
without regenerating or blessing them. Reproduce and validate the fixtures
with the commands documented in `tools/source-fixtures/README.md`.
