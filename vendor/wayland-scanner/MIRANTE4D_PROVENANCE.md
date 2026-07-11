# Vendored `wayland-scanner`

Mirante4D temporarily carries `wayland-scanner` 0.31.10 as a local path patch
because its latest crates.io release constrains `quick-xml` to the vulnerable
0.39 release line. The upstream project has accepted the 0.41 correction but
has not yet published it.

- Upstream project: <https://github.com/Smithay/wayland-rs>
- License: MIT (`LICENSE.txt`)
- Source archive:
  <https://crates.io/api/v1/crates/wayland-scanner/0.31.10/download>
- Source archive SHA-256:
  `9c324a910fd86ebdc364a3e61ec1f11737d3b1d6c273c0239ee8ff4bc0d24b4a`
- Release source commit recorded by the archive:
  `a3d7927d87799b2955bf491b51c7c2a3a82da661`
- Upstream `GeneralRef` API adaptation:
  <https://github.com/Smithay/wayland-rs/commit/ec2d932855593d48aa83c76820f3efbcfea86d39>
- Upstream dependency security correction:
  <https://github.com/Smithay/wayland-rs/commit/d07c4f91f28b42e5a485823ffd9d8d5a210b1053>

The crates.io archive was extracted in full. Mirante4D changes only the
`GeneralRef::xml_content()` call to upstream's `xml10_content()` adaptation,
the `quick-xml` requirement and corresponding lock entry from the released
0.39 line to 0.41, upstream's matching changelog line, and this provenance
record. The unrelated `similar` dev-dependency change from the first upstream
commit is deliberately excluded. No other scanner source code is changed.

To recapture the source archive:

```bash
curl -fsSL \
  https://crates.io/api/v1/crates/wayland-scanner/0.31.10/download \
  -o wayland-scanner-0.31.10.crate
printf '%s  %s\n' \
  9c324a910fd86ebdc364a3e61ec1f11737d3b1d6c273c0239ee8ff4bc0d24b4a \
  wayland-scanner-0.31.10.crate | sha256sum --check
```

Replace this path patch with the first reviewed crates.io release that accepts
`quick-xml` 0.41 or newer.
