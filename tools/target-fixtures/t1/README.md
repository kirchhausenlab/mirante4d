# Target-profile T1 authority

This directory contains the independent, off-product WP-10A-C authority
tooling. It never imports Mirante4D production crates.

- `cases-v1.tsv` is the shared declarative value input.
- `fact_oracle/` computes scientific facts without opening package bytes.
- `producer/` writes candidate package bytes but cannot certify them.
- `mutations-v1.json` defines compact negative derivations without copying
  package trees into the repository.
- `hand_vectors/` verifies separately frozen critical byte/hash vectors.
- `reader/` independently reads every logical array and exact package fact.
- `validate.py` checks the promoted authority offline and fail-closed.
- `reproduce.py` assembles the candidate authority twice and compares bytes.

The reviewed C4 checkpoint promotes the exact `target-m4d-v1` authority under
`fixtures/target`. Generation still writes only below
`target/mirante4d/fixture-candidates`; the reproducer never writes tracked
authority files.

For `affine_mod_decimate`, level 0 is
`(10000*t + 4000*c + 97*z + 13*y + 3*x) mod 65521`; level `L` samples level-0
coordinates multiplied by `2^L`. For `f32_cycle`, the listed bit patterns are
indexed in logical `t,c,z,y,x` C order. Logical channel 3 is entirely invalid;
the other channels are valid exactly when
`(z*Y*X + y*X + x + 3*c) mod 11 != 0`. Invalid raw storage samples retain the
declared cycle bits, while scientific canonical values use positive zero.
