# Documentation Audit

Use this checklist for documentation cleanup and reorganization. The purpose is
to keep current information easy to find, not to create process for its own
sake.

## Authority

When documents disagree, use this order:

1. explicit current user direction;
2. root `AGENTS.md` and `docs/AGENTS.md`;
3. `docs/CURRENT_STATE.md` for implemented facts;
4. `docs/planning/NOW.md` for active work;
5. the active foundation handoff for approved target scope and package order;
6. current code and generated command/report output;
7. domain documents and detailed specs;
8. archive material as history only.

## Audit

1. Inventory root and active documentation.
2. Check the default read path in `docs/README.md`.
3. Compare current behavior claims with code constants and command output.
4. Check local links and headings.
5. Look for contradictory status, duplicate authority, private paths, and
   historical evidence in active files.
6. Classify each affected file as keep, rewrite, merge, archive, or delete.
7. Make the smallest coherent cleanup.
8. Run Markdown, schema, and relevant command checks.

## Keep, Archive, Or Delete

- Keep a file active when it explains current behavior, current work, or an
  approved contract that is still being implemented.
- Archive completed plans and useful historical evidence.
- Delete aliases, empty placeholders, duplicated instructions, and stale
  guidance that adds no historical value.
- Git history is sufficient for ordinary superseded wording; not every deleted
  document needs an archived copy.

## Completion

A documentation cleanup is complete when:

- the default read path is short and accurate;
- current and planned behavior are clearly separated;
- active links and machine-readable documents validate;
- superseded material is outside active navigation;
- skipped checks and remaining uncertainty are reported.

Documentation-only changes do not require product-open validation unless they
change or assert product-validation results.
