# Architecture Decision Records

Program version: 0.21
Last updated: 2026-07-11

These ADRs record accepted decisions for the foundation program. Most remain
targets; implementation notes say when an owning package completed one. None
independently authorizes implementation or replaces
`docs/CURRENT_STATE.md` as the authority for current facts. The
[foundation implementation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
owns program status, sequencing, promotion, and the same-version brief bundle.

| ADR | Target decision | Decision IDs |
| --- | --- | --- |
| [ADR-0001](ADR-0001-foundation-program-and-hard-cutovers.md) | Deep foundation program with staged hard cutovers and no compatibility debris | OD-001–OD-008; PRG-001/002/003/008/015 |
| [ADR-0002](ADR-0002-strict-m4d-format-lifecycle-and-identity.md) | Strict M4D/OME-NGFF profile, lifecycle, sharding, and identity families | D-007/D-008/D-009 |
| [ADR-0003](ADR-0003-immutable-project-generations.md) | Immutable content-addressed project generations and durability | D-010 |
| [ADR-0004](ADR-0004-workspace-graph-and-two-epoch-cutover.md) | Sixteen-crate ownership DAG and two-epoch clean-repository gated trunk | D-017/D-018/D-019 |
| [ADR-0005](ADR-0005-verification-and-zero-cost-ci.md) | Six verification leaves, two zero-cost checks, and trusted-local evidence | D-022/D-023 |
| [ADR-0006](ADR-0006-publication-clean-root-source-first.md) | Clean public source root before a separate full-data release | D-001/D-002/D-019/D-021 |
| [ADR-0007](ADR-0007-foundation-dataset-hardware-product-envelope.md) | Profile-based dataset/hardware envelope, required GPU, Linux/Vulkan, 1080p, no segmentation | D-004/D-005/D-006/D-015/D-016 |
| [ADR-0008](ADR-0008-contribution-governance.md) | Maintainer-led MIT contributions with no CLA/DCO initially | D-003 |
| [ADR-0009](ADR-0009-canonical-model-contract.md) | Pure canonical domain, identity, and project model before the product hard cutover | WP-07A |

D-003 contribution governance is implemented and operational in the public
repository; WP-04 applied and read back its remote controls. ADR-0009 was
implemented and accepted by WP-07A at `foundation-wp-07a-exit-1`; its model
is now the live authority through the WP-07B-B hard cutover. D-011 through
D-014 and D-020 belong only to the deferred open-data follow-on. New ADRs must
not convert a later-gated decision into implementation authority.
