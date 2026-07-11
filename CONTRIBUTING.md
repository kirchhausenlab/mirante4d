# Contributing

Mirante4D is a small, maintainer-led academic project. Focused bug reports,
documentation fixes, and pull requests are welcome, but response times are
best effort. The software is pre-alpha: APIs, commands, and persisted formats
may change through explicit hard cutovers.

## Before A Pull Request

- Open an issue or discussion before a large architectural or product change.
- Read [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).
- Keep changes narrow and delete replaced paths instead of adding compatibility
  layers.
- Run `cargo xtask verify-bootstrap` and any additional checks relevant to the
  change.
- Explain what was verified and what remains unverified.
- Never include credentials, private paths, unpublished results, or microscopy
  data in an issue, pull request, log, screenshot, or test fixture.

Pull requests are reviewed by trusted maintainers. Required checks must pass
and review conversations must be resolved before merge. While the project has
one maintainer, no mechanical approving review is required; after a second
trusted maintainer is appointed, one approval is required. Write, maintain,
and administrator access is limited to explicitly trusted maintainers.

There is no CLA or DCO. By contributing, you agree that your contribution is
provided under the project's MIT License under the same terms as the outbound
project.

Do not contribute microscopy datasets or dataset derivatives through this
policy, even when they appear public or redistributable. The project currently
accepts source and documentation contributions only. Small test fixtures are
added only through a maintainer-reviewed provenance, rights, and independent-
validation process.

Changes to workflows, verification commands, fixture registries, or gate code
receive explicit maintainer review before an external workflow run is approved.
Public pull-request jobs receive no secrets and never run on the project's
trusted GPU, performance, or data machines.

For sensitive security issues, follow [SECURITY.md](SECURITY.md) instead of
opening a public issue.

Please be respectful and constructive. Maintainers may close contributions
that are out of scope, unsafe for scientific data, or too costly for this small
project to maintain.
