# Benchmark Baselines

No benchmark baseline is currently authoritative or committed in this
directory. The former machine- and experiment-specific reports were private
diagnostic history and were removed before the public-source snapshot.

A future baseline may be added only by its owning verification work package.
It must identify an immutable revision, build profile, fixture class and
digest, hardware class, backend, viewport, metric definition, calibration
state, sample count, and acceptance rule. Private qualification data is named
publicly only by an opaque `T5-QUAL-*` identifier; raw paths and experiment
labels stay in the private evidence resolver.

Baseline comparison is never part of the ordinary pull-request profile. Until
WP-14 promotes a statistically qualified baseline, benchmark reports are
diagnostic and must remain under ignored `target/mirante4d/` paths.

The existing helper commands remain available for later replacement work:

```text
cargo xtask bench-check <current.json> <baseline.json>
cargo xtask baseline-audit
cargo xtask baseline-refresh-plan [report-root]
```

An empty baseline directory is valid. It is more truthful than publishing old
measurements as current performance evidence.
