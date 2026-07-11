# Documentation

Mirante4D keeps the default read path short. Detailed specifications and the
foundation handoff are available when a task needs them; they are not required
reading for ordinary contributions.

## Read Order

1. [Product](PRODUCT.md) — who the application is for and what it is trying to
   achieve.
2. [Current state](CURRENT_STATE.md) — what exists today, including known
   limitations.
3. [Current work](planning/NOW.md) — the one active work package and what comes
   next.
4. The relevant domain document:
   - [Architecture](ARCHITECTURE.md)
   - [Data format](DATA_FORMAT.md)
   - [Testing](TESTING.md)
   - [Development](DEVELOPMENT.md)

Agents must also follow the root `AGENTS.md` and [agent guide](AGENTS.md).

## Reference Material

- [Foundation implementation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
  — approved scope, package order, and hard-cutover requirements.
- [Specifications](specs/README.md) — detailed contracts for the current code
  and approved replacement work.
- [Decisions](decisions/README.md) — durable architectural decisions.
- [Benchmarks](benchmarks/README.md) — measurement policy and curated baselines.
- [Release](release/README.md) — current Linux packaging status.
- [Open data](open-data/README.md) — public-data boundary and future release
  process.

## Documentation Rules

- Describe implemented behavior as current and proposed behavior as planned.
- Keep one authority for each fact; link instead of copying long rule lists.
- Update documentation with the code or decision that changes it.
- Move completed plans and historical evidence out of the active tree.
- Keep private datasets, workstation paths, credentials, and unpublished
  metadata out of public documentation.
- Verify local links before merging documentation changes; the normal local
  Markdown command is still owned by WP-01.
