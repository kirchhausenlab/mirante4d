# Deferred Derived-Label Capability

Status: DEFERRED
Last updated: 2026-07-14

Mirante4D has no segmentation product, persistence, renderer, or automation
surface. A previous prototype was removed during WP-02 rather than carried
across the foundation refactor.

Foundation completion does not reactivate segmentation. Any return requires a
separately approved capability plan that reconsiders the design from first
principles while retaining these scientific lessons:

- categorical IDs require exact, non-interpolated sampling and an explicit
  background policy;
- source geometry, dataset/layer identity, provenance, and completeness must
  remain explicit;
- derived mutable work must never overwrite source data and needs atomic,
  validated persistence;
- edits need bounded history, explicit collision/merge behavior, and clear
  locked-state semantics;
- storage and rendering must remain bounded for large sparse data without
  producing unbounded tiny-file counts.

This record preserves lessons only. It authorizes no code, format field,
feature flag, compatibility path, fixture, command, or test.
