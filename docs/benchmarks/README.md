# Benchmarks

This folder defines how Mirante4D performance should be measured.

## Files

- `BENCHMARK_PLAN.md` - benchmark categories, metrics, and acceptance policy.
- `HARDWARE_MATRIX.md` - target hardware classes to test.
- `DATASET_FIXTURES.md` - synthetic and real fixture policy.
- `baselines/` - optional curated baseline reports for named hardware/configuration cases.

## Rule

Performance claims must include:

- commit or build identity
- OS
- CPU
- GPU adapter/backend
- memory/VRAM if known
- dataset fixture
- metric definition
- baseline and result

Initial responsiveness, first-frame, frame-rate, memory, playback, and regression targets are defined in `../specs/PERFORMANCE_TARGETS_SPEC.md`.
