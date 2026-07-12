# Target-profile T1 authority

This directory contains the independent, off-product WP-10A-C authority
tooling. It never imports Mirante4D production crates.

- `cases-v1.tsv` is the shared declarative value input.
- `fact_oracle/` computes scientific facts without opening package bytes.
- `producer/` writes candidate package bytes but cannot certify them.
- `reader/` observes candidate bytes through the pinned external reader.
- `hand_vectors/` verifies separately frozen critical byte/hash vectors.
- `validate.py` performs offline manifest and bounded-archive validation.
- `reproduce.py` creates two ignored candidates and requires byte equality.

Generation writes only below `target/mirante4d/fixture-candidates`. Tracked
files under `fixtures/target` appear only in the separately reviewed promotion
checkpoint.

For `affine_mod_decimate`, level 0 is
`(10000*t + 4000*c + 97*z + 13*y + 3*x) mod 65521`; level `L` samples level-0
coordinates multiplied by `2^L`. For `f32_cycle`, the listed bit patterns are
indexed in logical `t,c,z,y,x` C order. Logical channel 3 is entirely invalid;
the other channels are valid exactly when
`(z*Y*X + y*X + x + 3*c) mod 11 != 0`. Invalid raw storage samples retain the
declared cycle bits, while scientific canonical values use positive zero.
