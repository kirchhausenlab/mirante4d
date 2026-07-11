# Mirante4D Agent Entry Point

This repository is a greenfield rewrite of the browser-based `llsm_viewer` into a native desktop application for large 4D microscopy datasets.

Read `docs/AGENTS.md` before making any change. It contains the project invariants, documentation policy, expected stack, quality bar, high-risk work guardrails, and the required read order for agents.

Hard rule: backward compatibility is forbidden unless the user explicitly asks for it. Do not add legacy readers, compatibility shims, fallback branches for old formats, commented-out old code, migration clutter, or "safe" alternate paths by default. This project uses hard cutovers while it is early and greenfield.

Hard rule: broad architecture, rendering, performance, data format, preprocessing, GPU, or corrective refactor work must follow the high-risk workflow in `docs/AGENTS.md` before implementation. Do not reinterpret a user's architectural goal into a narrower local patch without explicitly reporting the gap and getting approval.

Hard rule: for rendering, viewport, GPU, data-loading, interaction, or large-dataset work, automated tests and smoke tests are not enough to call the work complete. The real interactive viewer must be opened and exercised on the relevant dataset unless the user explicitly waives product-open validation. See `docs/AGENTS.md` and `docs/TESTING.md`.
