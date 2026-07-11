# Documentation

This is the sole owner of Mirante4D's documentation read order and human
index. The machine inventory is
[`documentation-index.json`](documentation-index.json).

## Read Order

1. [Product](PRODUCT.md) — product scope and non-goals.
2. [Current state](CURRENT_STATE.md) — implemented facts and limitations.
3. [Current work](planning/NOW.md) — the current checkpoint and next package.
4. The document that owns the task:
   - [Architecture](ARCHITECTURE.md)
   - [Data format and safety](DATA_FORMAT.md)
   - [Testing and evidence](TESTING.md)
   - [Development commands](DEVELOPMENT.md)
   - [Release status](RELEASE.md)
   - [Decisions](decisions/README.md)
   - [Unresolved backlog](BACKLOG.md)

Agents must also follow the [agent guide](AGENTS.md). Dependency-policy
exceptions have one separate tool-owned
[ledger](DEPENDENCY_EXCEPTIONS.md).

## Plans

The [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns the
approved package order and links its active technical contracts. These are
targets until the current-state authority says they are implemented.

Two future outcomes are deliberately separate:

- [public microscopy data](plans/deferred/OPEN_DATA_FOLLOW_ON.md);
- [possible post-foundation segmentation](plans/deferred/SEGMENTATION.md).

## Documentation Rules

- Keep one authority for each fact and link to it instead of copying it.
- Label implemented, target, deferred, and reference material honestly.
- Update the owning document in the same change that changes a fact.
- Delete superseded plans and policies; Git history is the archive.
- Keep private datasets, machine paths, credentials, and unpublished metadata
  out of public documentation.
- Run `cargo xtask docs-check` before merging documentation changes.
