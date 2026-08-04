# Documentation

This file owns the documentation read order and human index.

## Read Order

1. [Product](PRODUCT.md) — product scope and non-goals.
2. [Current state](CURRENT_STATE.md) — implemented facts and limitations.
3. [Current work](planning/NOW.md) — active outcomes and the next action.
4. Read the document that owns the task:
   - [Architecture](ARCHITECTURE.md)
   - [Data format and safety](DATA_FORMAT.md)
   - [Testing and validation](TESTING.md)
   - [Development commands](DEVELOPMENT.md)
   - [Release status](RELEASE.md)
   - [Decisions](decisions/README.md)
   - [Unresolved backlog](BACKLOG.md)

Agents must also follow the [agent guide](AGENTS.md). Active dependency-policy
exceptions have one tool-owned [record](DEPENDENCY_EXCEPTIONS.md).

## Active Plans

The
[viewer rendering correctness, recovery, and numerical cutover](plans/active/VIEWER_RENDERING_CORRECTNESS_RECOVERY_AND_NUMERICAL_CUTOVER.md)
owns the unfinished static-performance and product-closeout work.

The [viewer GPU testing refactor](plans/active/VIEWER_GPU_TESTING_REFACTOR.md)
owns the pending initial performance baseline and owner evidence.

## Document Lifecycle

- Keep one owner for each current fact.
- Link to an owner instead of copying its text.
- Keep plans only while a concrete outcome remains unfinished.
- On closeout, move current facts to their owners and delete the plan.
- Keep exact test results in machine reports, commits, or pull requests.
- Keep unapproved future work as short backlog entries.
- Use Git history as the archive. Do not create a documentation archive.
- Run `cargo xtask docs-check` for documentation changes.
