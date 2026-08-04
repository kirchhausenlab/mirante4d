# ADR-0006: Separate Source Publication From Data Publication

Status: Accepted

Accepted: 2026-07-09

Last reviewed: 2026-08-04

## Context

Open-source code and redistributable microscopy data have different rights,
privacy, hosting, permanence, cost, integrity, provenance, and citation
requirements.

Publishing source does not prove that a full dataset is safe or useful to
publish.

## Decision

- Publish source under the MIT License from the approved public history.
- Keep private predecessor history outside the public object graph.
- Keep retained source assets and small fixtures tied to reviewed provenance
  and rights.
- Publish only small approved repository fixtures by default.
- Require a separate owner decision for any full microscopy dataset,
  derivative, hosting location, DOI, or release lifecycle.

## Consequences

Source and software work can continue without a public full dataset. Public
source availability does not imply dataset redistribution rights,
reproducibility, permanence, or scientific endorsement.

External dataset contributions remain outside the normal contribution policy.
A rights or privacy uncertainty blocks the affected artifact, not unrelated
source development.

## Guardrails

- Never put secrets, private history, or unpublished microscopy data in the
  public repository.
- Do not treat a public URL as proof of redistribution rights.
- Do not infer a dataset release from the presence of synthetic fixtures.
- A full-data release needs its own reviewed authority and verification.

[Current state](../CURRENT_STATE.md) owns current publication status.
[Contributing](../../CONTRIBUTING.md) owns contribution policy.
