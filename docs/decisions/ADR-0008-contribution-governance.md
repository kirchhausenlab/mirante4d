# ADR-0008 — Lightweight Contribution Governance

Status: ACCEPTED AND IMPLEMENTED
Accepted: 2026-07-10
Last reviewed: 2026-07-15
Public contribution intake: OPERATIONAL
Decision ID: D-003
Current-state effect: PUBLIC SOURCE POLICY AND REMOTE CONTROLS OPERATIONAL

This decision resolves source governance. The public-facing policy is installed
in this snapshot. WP-04 made the repository public and verified its roles,
rules, and workflow controls.

## Context

The public project will initially have one maintainer. Governance must be
honest, simple, permissive, and workable without pretending that independent
review or a large community already exists. Source contribution policy is
separate from the deferred dataset-contribution policy in D-020.

## Decision

- Use maintainer-led GitHub pull-request review.
- Accept code/documentation contributions under the repository's MIT license
  through an explicit inbound-equals-outbound statement in `CONTRIBUTING.md`.
- Require neither a CLA nor DCO/sign-off initially.
- While there is one maintainer, require no mechanical approving review so the
  owner cannot deadlock; require one approval when a second trusted maintainer
  exists. Conversation resolution and required checks still apply.
- Restrict write/maintain/admin to explicitly trusted maintainers. Apply the
  D-022 external-workflow approval, read-only token, standard-runner, no-secret,
  no-cache/artifact-by-default, and workflow-review rules.
- Publish concise conduct, support, maintainer-authority, and response guidance
  in the contributor-facing documentation, plus a security-reporting route,
  issue/PR templates, and the project's pre-alpha/no-stability status. Separate
  policy files are not required unless the project later outgrows the combined
  guidance.
- Accept no external dataset contribution under this policy; D-020 remains
  deferred and fail-closed.

## Alternatives Not Recommended

- A CLA before a concrete institutional or relicensing need exists.
- DCO/sign-off ceremony without a stated provenance problem it solves.
- Mandatory independent approval while only one maintainer exists.
- Open dataset uploads before rights, withdrawal, review, and hosting policy.

## Consequences

The initial path is easy for contributors and imposes no separate agreement
system. If ownership, institutional, patent, or relicensing needs later become
concrete, governance can change prospectively through a new owner-approved ADR;
that future possibility does not retroactively alter accepted contributions.

Source contribution intake is operational in the public repository with the
approved roles, rules, and workflow controls applied and read back during
WP-04. D-020 remains the separate fail-closed authority for dataset
contributions.

## Enforcement

- Contributor, conduct, security, support, template, license,
  maintainer-authority, and pre-alpha disclosures remain concise and verified.
- WP-04 applied and read back the accepted maintainer, review, fork, token,
  workflow, and branch controls; later changes must preserve the policy.
- No source-governance file or setting may imply that external datasets are
  accepted.
- A future CLA, DCO, approval-count, maintainer-role, or inbound-license change
  requires a new owner-approved ADR and prospective policy update.
- Later governance changes require a short approved plan and prospective
  policy update.

## Owning Documents

- [Development](../DEVELOPMENT.md)
