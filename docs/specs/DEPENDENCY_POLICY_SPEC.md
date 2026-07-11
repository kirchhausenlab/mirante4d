# Dependency Policy Specification

Status: ACCEPTED — current policy; forward gate topology superseded by plan 0.21
Last updated: 2026-07-10

Forward foundation dependency remediation is sequenced by the
[foundation refactor handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md);
this spec remains factual authority for the current policy until its owning
work package cuts over.

## Purpose

Define how external dependencies are selected, reviewed, and maintained.

## Scope

This spec covers Rust crates, native libraries, build tools, test tools, and optional platform-specific dependencies.

## Non-Goals

- Avoiding dependencies categorically.
- Reimplementing mature infrastructure without reason.
- Adding dependencies for old-format compatibility.

## Requirements

- Every dependency must have a clear reason.
- Prefer mature, maintained crates for boring infrastructure.
- Avoid large frameworks unless they provide substantial value.
- Avoid abandoned crates in core paths.
- Avoid crates with hidden network behavior.
- Avoid dependencies for tiny helpers that are easy and safer to write locally.
- License and security advisory checks should be automated.
- Optional dependencies must not become silent behavior changes.

## Dependency Classes

- Core runtime dependency: used by app, renderer, data engine, format, or preprocessing.
- Dev/test dependency: used only for tests, fixtures, benchmarks, or tooling.
- Optional acceleration dependency: CUDA/native/vendor-specific functionality.
- Platform dependency: OS-specific integration or packaging support.

Core runtime dependencies require the highest scrutiny.

## Implemented Tools

Current tooling:

- `cargo xtask verify-deps`
- `deny.toml`
- `docs/DEPENDENCY_EXCEPTIONS.md`

`verify-deps` runs native checks before invoking `cargo-deny`:

- all dependencies must come from the workspace path or crates.io
- all dependencies must expose a machine-readable license expression
- license expressions must be satisfiable by the approved allowlist
- package-specific exceptions must be documented
- `cargo-deny` must be installed for advisory, bans, source, and independent license checks

Install the advisory tool with:

```bash
cargo install cargo-deny --locked
```

`cargo tree` checks may still be used manually for dependency-growth investigations.

## License Allowlist

Allowed license atoms:

- `0BSD`
- `Apache-2.0`
- `Apache-2.0 WITH LLVM-exception`
- `BSD-2-Clause`
- `BSD-3-Clause`
- `BSL-1.0`
- `CC0-1.0`
- `ISC`
- `MIT`
- `MIT-0`
- `Unicode-3.0`
- `Unlicense`
- `Zlib`

`epaint_default_fonts` has a package-specific exception for `OFL-1.1` and `Ubuntu-font-1.0`; see `docs/DEPENDENCY_EXCEPTIONS.md`.

`colored` has a package-specific dev/test exception for `MPL-2.0` because it is pulled transitively by the UI screenshot comparison stack; see `docs/DEPENDENCY_EXCEPTIONS.md`. This exception is enforced by the native `xtask` metadata checker and is intentionally not listed in `deny.toml` unless `cargo-deny` encounters the package in its checked graph.

## Advisory Exceptions

`paste` has a package-specific advisory exception for `RUSTSEC-2024-0436`; see `docs/DEPENDENCY_EXCEPTIONS.md`. This is accepted only because it is pulled transitively by the current Zarr stack and the advisory reports no safe upgrade.

That current exception predates the foundation target's mandatory owner and
expiry fields. WP-03 must eliminate it or recertify it through the narrow,
owner-visible, time-bounded process before the public-root gate; this spec does
not silently grandfather it into the public repository.

## Open Foundation Advisory Baseline

The 2026-07-10 pinned offline cargo-deny recapture confirmed the five
`PUB-004` findings below. They are assigned refactor inputs, not approved
exceptions, and their presence means the current exact lockfile is not
publication-ready.

| Assignment | Current package | Advisory | WP-03 disposition |
| --- | --- | --- | --- |
| `DEP-BLOCK-001` | `anyhow` 1.0.102 | `RUSTSEC-2026-0190` | Resolve to at least 1.0.103; no exception. |
| `DEP-BLOCK-002` | `crossbeam-epoch` 0.9.18 | `RUSTSEC-2026-0204` | Resolve to at least 0.9.20 and verify Zarr/cache/parallel paths; no exception. |
| `DEP-BLOCK-003` | `quick-xml` 0.39.4 | `RUSTSEC-2026-0194` | Eliminate every direct/transitive copy below 0.41 and verify hostile XML work bounds; no exception. |
| `DEP-BLOCK-004` | `quick-xml` 0.39.4 | `RUSTSEC-2026-0195` | Same graph-wide remediation plus allocation-bound tests; no exception. |
| `DEP-BLOCK-005` | `ttf-parser` 0.25.1 | `RUSTSEC-2026-0192` | Replace/upgrade the GUI path; only a separately approved narrow exception may expire no later than WP-14 entry. |

## Duplicate Versions

Duplicate transitive crate versions are warnings for now. This keeps dependency growth visible without blocking on GUI, GPU, platform, and Zarr dependency trees outside Mirante4D's direct control. Direct Mirante4D dependency duplication or duplicate versions in core runtime paths still require review.

## Invariants

- No dependency is added casually.
- No compatibility dependency is added unless explicitly requested by the user.
- Optional vendor-specific dependencies must be explicit and isolated.
- Dependencies that affect output format or runtime behavior must be documented.
- New license/source exceptions must update `docs/DEPENDENCY_EXCEPTIONS.md`, the native `xtask` checker, and `deny.toml` when the exception is encountered by `cargo-deny`.

## Failure Modes

- transitive dependency pulls in unsupported license
- dependency becomes unmaintained
- crate adds network behavior
- native dependency complicates packaging
- multiple versions create binary size or security issues

## Testing Requirements

- Dependency policy checks are currently reachable through `verify-full`; that
  aggregate placement is factual inventory, not a forward requirement. The
  approved foundation target gives dependency/source/license/advisory policy a
  nonrecursive owning leaf and a pinned `cargo-deny` lineage.
- License/advisory failures must fail `verify-deps`.
- Optional dependency builds should have targeted checks when enabled.

## Open Questions

None at this time.
