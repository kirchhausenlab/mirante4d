# Hardware Matrix

Status: APPROVED FOUNDATION TARGET — not yet a support matrix
Last updated: 2026-07-11

Mirante4D currently claims one product platform: Linux x86_64 with a qualifying
Vulkan GPU. Windows and macOS remain compile/portability work until separately
qualified. CPU/software rendering is for tests, diagnostics, and reference
output, not a silent interactive fallback.

## Hardware Classes

- `HW-0`: CPU-only correctness and policy verification. This is not an
  interactive-viewer support claim.
- `HW-1`: unverified minimum candidate: 4 physical/8 logical CPU threads,
  16 GiB RAM, local SSD, 1280x720 display, and a discrete Vulkan GPU with at
  least 4 GiB VRAM. It cannot be advertised until an exact weaker machine
  passes the complete qualification matrix.
- `HW-2`: the fixed Linux/Vulkan reference class used for trusted local GPU,
  product-open, and performance evidence. Its private machine identity is held
  outside the repository. Portable evidence records only the class and the
  required sanitized hardware/driver facts.
- `HW-3`: optional future capacity characterization with 64 GiB RAM and at
  least 16 GiB discrete VRAM. It is not a release requirement.

Display qualification stops at 1920x1080. 4K and spanning-display workloads
are outside the foundation scope.

## Evidence Rules

A qualified run binds the exact source revision, executable/package digest,
dataset tier and digest, CPU/GPU ledgers, OS/kernel/backend/driver, storage and
display facts, power/thermal calibration, cold/warm state, sample count,
metric definition, and thresholds. Hosted runners provide portability and
correctness evidence only; they are never performance references.

Trusted GPU or private-data machines execute only maintainer-selected immutable
revisions. They are not attached as public self-hosted runners, and the tested
process receives no upload credential.

No current benchmark JSON is a hard baseline. WP-14 owns the final calibrated
hardware and performance claims.
