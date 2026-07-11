# Agent Guide

Mirante4D is an early-stage native desktop viewer for large 4D microscopy
datasets. It is a hard-cutover successor to the browser-based `llsm_viewer`,
not a compatibility-preserving port.

## Required Read Order

Before changing the repository, read:

1. `docs/CURRENT_STATE.md`
2. `docs/planning/NOW.md`
3. The domain document relevant to the task

For foundation work, also read
`docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md` and the owning work-package
brief.

## Non-Negotiable Rules

- Backward compatibility is forbidden unless the user explicitly requests it.
  Do not add legacy readers, compatibility shims, fallback paths, dual-format
  branches, or commented-out predecessor code.
- Keep current facts separate from approved targets. A plan is not an
  implementation.
- Prefer clear domain ownership and typed models over generic modules or loose
  maps.
- Preserve source data. Writes must be explicit, validated, and recoverable or
  atomic where practical.
- Make performance claims only from named measurements.
- Keep the project appropriate for a small academic open-source team. Add
  process only when it protects scientific correctness, user data, or release
  integrity.

## High-Risk Changes

Architecture, rendering, GPU, preprocessing, persistence, data-format,
large-dataset, and corrective refactors require an approved plan before code
changes. The plan must state:

- user-visible outcome;
- boundary or invariant being changed;
- paths being replaced or deleted;
- important requirement-to-evidence mappings;
- non-goals and risks;
- automated and product-open validation.

Do not narrow an architectural request into a local patch without saying so.

## Verification Language

- **Implemented:** the change exists.
- **Automated-verified:** the relevant automated checks passed.
- **Product-validated:** the real desktop application was opened and the
  affected workflow was exercised on the relevant dataset.

Rendering, viewport, GPU, data-loading, interaction, and large-dataset work is
not complete without product-open validation unless the user explicitly waives
it. Report exact commands, datasets, failures, skipped checks, and residual
risk.

## Expected Stack

- Rust
- `wgpu`
- `winit`
- `egui` / `egui-wgpu`
- strict native packages with explicit CPU, memory, GPU, and I/O budgets

CUDA is not a baseline dependency. Add it only if a future benchmark justifies
an explicit optional NVIDIA path.
