# Foundation Refactor — Data Format, Lifecycle, And Identity Brief

Status: HANDOFF_READY SUBORDINATE
Program version: 0.21
Last updated: 2026-07-11
Implementation authorization: INHERITED ONLY THROUGH THE ACTIVATED HANDOFF AND PACKAGE ENTRY GATES
Parent authority: `docs/plans/active/FOUNDATION_REFACTOR_HANDOFF.md`
Authority scope: D-007/D-008/D-009 format, lifecycle, sharding, interoperability, and identity families

This brief cannot override program scope/status, the canonical D-018 repository sequence, D-022's public CI rollout, the work-package dependency graph, or handoff activation gates. A conflict or program-version mismatch blocks work and must be reconciled through the parent handoff. This brief carries no independent implementation authorization.

## Approved Format And Lifecycle Boundary

Status: OWNER-APPROVED TARGET POLICY under OD-018/OD-019. D-007 and D-008 are
resolved; D-009 identity canonicalization is separately resolved under OD-020.
This section does not change the current accepted `mirante4d-v1` reader or
authorize a format cutover.

### Architecture Alternatives

| Alternative | Benefit | Foundation cost/risk | Recommendation |
| --- | --- | --- | --- |
| Custom M4D plus separate OME export | Maximum internal freedom and closest to current layout | Public native data remains siloed; export duplicates large pixels and can lose extended semantics | Rejected for this foundation; requires an owner-approved reopening of D-007 |
| Plain/generic OME-NGFF as the product format | Widest theoretical ecosystem input | Standard permits too many codecs/layouts and lacks required validity, indexing, identity, integrity, and resource contracts; generic support would create fallback sprawl | Reject |
| Strict M4D profile layered on released OME-NGFF/Zarr v3 | Stores pixels once, gives external tools standard access where exact, and retains bounded M4D semantics | Requires a deliberate profile, extension namespace, channel-layout cutover, and honest interoperability levels | Recommended |

### Approved Layered Authority

The package is admitted by a small, strict, versioned M4D profile header. Once
admitted, every field has one normative owner:

1. **Released OME-NGFF core** owns facts it can represent exactly: pixel arrays,
   dtype, typed axes/units, multiscale discovery, standard scale/translation,
   and channel basics.
2. **Small namespaced M4D extension** owns only the strict profile/capability
   identity, stable logical layer mapping, full-affine or validity semantics
   absent from the released standard, dataset display defaults, and references/
   digests for provenance, identity, and indexes. Optional transitional OME
   display metadata is an external projection, not M4D authority.
3. **External compact indexes** own occupancy, valid counts, min/max, range
   hierarchies, integrity records, and other large acceleration data. They are
   paged, lazy, byte-budgeted, and regenerable where declared.
4. **Portable provenance/release records** own source identities, recipes,
   rights/citation, and derivation. Machine-local paths stay in a private local
   import record or project state, never portable scientific identity.
5. **Project state** owns current display/workspace choices. Dataset metadata
   supplies defaults only and display changes never alter scientific identity.

Shared pixels are stored once. A field must not be duplicated across OME and
M4D authority merely for convenience; any unavoidable mirror is validated for
semantic equality. Mirante4D still opens only the exact M4D profile and does
not become a generic OME-Zarr reader. Generic OME-Zarr ingestion, normalization,
or export remains a separately approved importer/converter boundary.

The physical storage profile fixes what OME-NGFF intentionally leaves open:
supported dtypes, regular chunk grid, one-timepoint/one-channel minimum read
unit, bounded shard size, codec/endianness subset, checksum policy, local
directory-store behavior, and maximum metadata/index page sizes. Draft OME
RFCs and release candidates cannot become persisted dependencies.

### Approved Physical Channel Cutover

Released OME-NGFF represents one standard multichannel image as one array per
scale with axes normally `t,c,z,y,x`. Separate `t,z,y,x` arrays are separate
images; metadata cannot truthfully relabel them as channels of one image.

The approved layout is:

```text
logical M4D layer ch_i -> physical OME image array + channel index i
array axes             -> t,c,z,y,x
inner brick            -> 1,1,bz,by,bx
outer shard            -> 1,1,sz,sy,sx
```

The app still exposes each channel as an independent logical `t,z,y,x` layer.
Because every chunk and shard has `c=1`, hidden channels require no read,
decode, upload, or render work. Co-registered channels in one image must share
stored dtype, spatial shape, and transform; heterogeneous dtype/grid images use
separate standard image groups linked by the M4D logical graph.

This refines the physical part of the approved dataset envelope without
changing its user-visible independent-channel rule. The alternative is to keep
separate physical `t,z,y,x` arrays and explicitly weaken interoperability to
separate images or a large rewriting export.

### Mandatory Sharding And Filesystem-Safety Invariant

OD-018 and D-007 are resolved: production arrays use Zarr v3 indexed sharding:

- the inner chunk is the small logical brick used for cancellation, cache
  eviction, and visible-work reads;
- an outer shard packs many spatial bricks into one physical object while its
  internal index permits an individual brick read without decoding the full
  shard;
- time and channel shard dimensions remain `1` by default, while spatial
  grouping supplies the file-count reduction;
- the current `64 MiB` target and `256 MiB` hard cap for uncompressed outer
  shards remain seed policy for WP-10A measurement, not permission to revert to
  unsharded brick files;
- validity arrays, occupancy/range/integrity indexes, and other large metadata
  are themselves packed into bounded chunked/sharded structures rather than
  creating one sidecar file per brick;
- tiny explicitly named conformance fixtures may use unsharded arrays only when
  the test is specifically about that layout. Import output, product fixtures,
  representative benchmarks, and public data never do.

Import preflight reports estimated logical-brick count, shard count, total
physical object count, maximum objects in one directory, minimum-read bytes,
and expected metadata/index objects. WP-10A must set per-profile object-count
and directory-fan-out limits before the first target-profile production write
or candidate and no later than WP-10C entry; DS-0 through DS-4 acceptance then
verifies actual counts. Any plan that maps logical bricks to individual files,
creates per-brick sidecars, or exceeds the approved physical object/fan-out
limit is rejected before writing payloads.

Illustratively, the approved `DS-3` boundary has about `94,710` logical bricks
at `s0`; grouping `4 x 4 x 4` spatial bricks per shard yields about `1,683`
`s0` shard objects and roughly `2,014` across its seven-scale pyramid, not one
file per brick. The `DS-4` boundary is roughly `5,475` shard objects across 365
timepoints and four scales under the same seed policy. WP-10A must recompute
these from the final profile and block any unexplained file-count regression;
the examples are scale checks, not permission to optimize only for them.

### Interoperability Claim Levels

Every public package/layer declares the strongest proved level:

| Level | Meaning |
| --- | --- |
| `IO-0 M4D-only` | Required scientific semantics are not representable by the pinned released OME profile; no generic interpretation claim |
| `IO-1 pixel-readable` | External Zarr/OME tools can discover/read pixel arrays, but named M4D extensions are required for full interpretation |
| `IO-2 lossless-OME` | Pixels, axes, units, channels, pyramid, and transforms are represented exactly in the pinned released OME profile |
| `IO-3 externally-verified` | `IO-2` plus official-schema validation and independent external pixel/metadata/transform readback on the exact package |

Stable OME-NGFF 0.5 supports scale and translation but not arbitrary affine
geometry. A rotated/sheared M4D grid must therefore be `IO-0`/`IO-1`, or an
explicit derived resampled OME image must be published with provenance. It must
never be approximated silently. Foundation interoperability covers dense
intensity multiscales only; labels, tables, plates, collections, remote stores,
and zipped stores remain outside the claim.

### Required Metadata And Validation Cutover

- The bootstrap header remains within the approved `4 MiB` ceiling and contains
  no per-brick, per-shard, source-file-list, histogram-bin, or range-tree
  expansion.
- Index pages remain at most `1 MiB`; open loads only bootstrap metadata and
  the pages needed for the first visible working set.
- Zarr metadata derives chunk/shard addressing. The M4D manifest does not list
  every physical object merely to rediscover the store.
- Import computes integrity while writing rather than rereading every shard,
  then performs bounded structural validation before commit. Full payload
  validation is a separately cancellable operation and is not repeated
  accidentally.
- Validation independently checks the official OME schema, the strict M4D
  profile/capability tuple, index consistency, and external expected facts.
  Mirante writer/reader agreement alone cannot prove conformance.
- Runtime and renderer consume a format-neutral logical dataset API; they do
  not receive OME JSON, M4D manifest DTOs, Zarr paths, or codec details.

### Approved Persisted-Contract Lifecycle

Lifecycle is assigned separately to datasets, projects, analysis artifacts,
preferences, and other durable contracts. A public repository, `v1` label, Git
tag, DOI, or generated fixture never grants stability by implication.

| State | Contract |
| --- | --- |
| `EXPERIMENTAL` | Current state. The core reads/writes one exact profile. Every incompatible change gets a new identity, fixtures/local packages are regenerated, the old core path is deleted, and unsupported data is rejected actionably. No converter obligation exists. |
| `CANDIDATE` | A revisioned profile/identity tuple is frozen for independent conformance, deterministic regeneration, product validation, and release review. Any contract change creates a new candidate and invalidates its evidence. Candidate is not a support promise. |
| `STABLE` | Granted only by a separate explicit owner-approved release gate after normative schemas, identity rules, independent fixtures, compatibility matrix, regeneration/conversion rehearsal, and product evidence pass. The promise is irreversible for the named version. |
| `DEPRECATED` / `SUPERSEDED` | An immutable successor exists, scientific/package/release identity relationships are recorded, and a verified regeneration path or separately approved side-by-side converter exists before the current app drops the old stable profile. |

The current dataset remains `mirante4d-v1`. WP-02 completed the project hard
cut from v13 to the current transitional `mirante4d-project-v14`. Those
contracts, analysis artifacts, and preferences are explicitly
`EXPERIMENTAL / NO COMPATIBILITY PROMISE`. The remaining owning packages
perform separate hard cuts without implicit converters: WP-07B owns
preferences/durable-state cutover, WP-10A/C owns the dataset format, and WP-12
owns analysis artifacts. This brief owns only the dataset lifecycle. Public
source and small experimental public fixtures do not change any contract's
status.

The dataset format should remain experimental through the technical foundation
and public-source release. The first stable obligation begins only when a
prospective profile passes the separately approved public-data candidate,
validation, publication, and owner gates. Project/user-artifact stability is a
separate future decision after transactional persistence and recovery proof.

For a future stable major:

- additive minor behavior is limited to specified optional fields/capabilities
  with defaults and conformance tests through one code path;
- unknown required capabilities and unknown majors reject clearly;
- the core app does not accumulate prior-major readers;
- before removing a stable major, publish and anonymously verify a direct
  side-by-side converter or deterministic regeneration/superseding package;
- converters never write in place, never become viewer fallbacks, and require
  their own explicit implementation authorization;
- a tiny header probe may identify an unsupported profile and direct the user
  to the remedy without decoding legacy payloads.

### Compatibility And Identity Tuple

The target header declares distinct compatibility dimensions rather than the
current duplicated `v1`/schema fields:

- format family and lifecycle state;
- scientific semantic-schema major/minor;
- physical storage-profile major/minor;
- index/acceleration-profile version;
- required capability identifiers;
- canonicalization/content-ID algorithm version;
- pinned OME-NGFF version/profile.

Writer, application, importer, and pipeline versions are provenance, not reader
compatibility authority. Resolved D-009 defines separate scientific content,
exact package/object, dataset release, derivation/recipe, artifact, and project-
reference identities. Recompression, resharding, sanitized provenance, or
regenerated acceleration preserves scientific content identity while changing
exact package identity.

### Format And Lifecycle Resolution

The owner approved all five recommendations on 2026-07-09 through OD-018/
OD-019. D-007 and D-008 are resolved. The physical-channel wording under D-004
is refined without changing its independent logical-channel behavior. D-009
identity canonicalization is now also resolved under OD-020.

Approval fixes the target architecture and lifecycle; it does not make the
current `mirante4d-v1` package an OME-NGFF profile, stabilize any existing
persisted contract, or authorize a dual reader. WP-10A must recapture the final
released OME standard, implement one hard-cut target profile, and produce the
required independent evidence before product cutover.

## Approved Identity Boundary

Status: OWNER-APPROVED TARGET POLICY under OD-020. D-009 is resolved. This
section fixes the dataset-identity target but does not change current behavior
or authorize implementation. D-010 transaction/durability mechanics belong
only to the [project-store brief](PROJECT_STORE_DURABILITY_BRIEF.md).

### D-009 Approved Identity Families

Use full, typed, versioned SHA-256 identifiers. SHA-256 is the sole normative
public identity algorithm because it is widely independently verifiable and
archival-tool friendly. BLAKE3 may remain an optional local cache/check aid,
but it never substitutes for or compares equal to a public identity.

No persisted bare hexadecimal digest is valid. The initial typed forms are:

```text
m4d-sc-v1-sha256:<64 lowercase hex>          scientific content
sha256:<64 lowercase hex>                    exact bytes in a typed object descriptor
m4d-package-v1-sha256:<64 lowercase hex>     exact package payload closure
m4d-recipe-v1-sha256:<64 lowercase hex>      reusable typed recipe
m4d-derivation-record-v1-sha256:<64 lowercase hex>
                                                exact recipe execution record
m4d-release-v1-sha256:<64 lowercase hex>     immutable dataset release
m4d-artifact-v1-sha256:<64 lowercase hex>    scientific artifact content
```

Every M4D semantic/tree preimage begins with a different fixed binary domain/
version tag. Exact-object digests are deliberately SHA-256 of the raw object
bytes, and the package ID is SHA-256 of the exact canonical root-manifest bytes;
their typed descriptor/root schema provides the domain rather than changing
the raw-byte digest. Changing a canonicalization rule, hash algorithm, tile
grid, or tree shape creates a new identity scheme; an implementation must
never reinterpret an old digest under new rules. Short prefixes are
display-only.

This is five identity responsibilities, with paired identifiers where exact
object/package and recipe/derivation need distinct levels:

| Responsibility | Answers | Must not be used as |
| --- | --- | --- |
| Scientific content | Are the scientific values, validity, axes, geometry, and logical layers exactly the same? | Proof of provenance, authorship, rights, or exact storage bytes |
| Exact object/package | Are these exact encoded bytes and this exact package tree intact? | Scientific sameness across a repack |
| Recipe/derivation record | What typed operation was specified, and what exact inputs produced what exact outputs? | App-version label or free-form audit note |
| Dataset release | Which immutable curator-approved science/package/provenance/rights/citation record was released? | Mutable mirror URL or local path |
| Project/artifact reference | Which verified science, immutable artifact version, and exact stored object does this project generation bind? | Human name, mutable UI handle, or filesystem location |

Full hash equality is the scheme's cryptographic equality/fixity criterion. It
does not mathematically prove identity or by itself establish authorship,
correctness, custody, rights, or trust; those come from validated release
records and publication custody, with signatures only if a later threat model
requires them.

#### Scientific Content Identity

The scientific content root includes only a closed canonical scientific model:

- semantic-schema identity and the voxel-center convention;
- deterministic logical layer keys/order and logical channel mapping, not
  physical array/channel position or a mutable display name;
- base/source-like logical `t,z,y,x` shape and scientific sample dtype;
- typed axes, canonical units, time calibration status, and base
  grid-to-world transform;
- a closed, versioned whitelist of scientifically meaningful acquisition and
  channel fields;
- effective base-scale validity; and
- exact valid base-scale sample values, represented through per-layer roots.

The scientific layer key is the zero-based logical ordinal in an explicit,
reviewed source-to-logical-layer mapping produced before pixel hashing. That
ordinal is scientific identity. A source-standard channel identifier may be
recorded as additional whitelisted semantics, but it never replaces the
ordinal. Source filesystem/enumeration order cannot assign the mapping
implicitly: an ambiguous source stops for an explicit mapping. Independent
imports using the same normalized mapping therefore choose the same keys even
if physical arrays or channels are reordered. A random import-time UUID,
filename, display label, or physical `c` offset cannot affect the key.

The scientific root excludes:

- paths, URLs, filenames, human dataset/display names, and descriptions;
- writer/app versions, host, user, timestamps, and provenance narrative;
- license, rights, citation, DOI, and release metadata;
- color, visibility, opacity, transfer functions, windows, cameras, layouts,
  and other display/project state;
- compression, byte order on disk, chunks, shards, object names, and directory
  layout; and
- derived multiscales, histograms, percentiles, occupancy/range/integrity
  indexes, and other acceleration data.

Storage-independent identity tiles use the initial fixed logical grid
`t=1, z=16, y=256, x=256` per logical layer. Edge origin and extent are part of
the leaf header; traversal is lexicographic by logical layer, then `t,z,y,x`.
Canonical samples use C order and explicit little-endian bytes: one byte for
`uint8`, two bytes for `uint16`, and exact finite IEEE-754 bits for `float32`,
including signed zero and subnormals. Non-finite scientific samples are
rejected under the current target contract.

Effective validity is hashed in logical `t,z,y,x` order with `x` fastest, the
first voxel in the least-significant bit of each byte, and every unused high
padding bit set to zero. Invalid integer samples use all-zero bytes and invalid
float samples use positive-zero `0x00000000` before value hashing. Changing an
irrelevant sentinel representation therefore preserves scientific identity
when the resulting validity is identical; a validity-bit change still changes
identity.

Spatial metadata is projected into canonical micrometers, the declared
voxel-center convention, and a row-major `4 x 4` binary64 grid-to-world matrix.
Metadata floating-point values use fixed-width IEEE-bit strings rather than
JSON numbers: binary64 is exactly `16` lowercase hexadecimal digits in
most-significant-nibble-first numeric bit order, independent of host/storage
endianness. Transform `-0` is normalized to `+0`; non-finite values are
rejected; and no tolerance or discretionary rounding is applied. The identity
profile pins exact rational conversion factors for its supported spatial units,
an operation/composition order, round-to-nearest-ties-to-even, and one rounded
binary64 result for each conversion. Equivalent standard scale/translation and
M4D-affine representations must first produce the same canonical matrix.

Canonical temporal coordinates are relative seconds. Calibration is exactly
one of `unknown`, `regular` with positive-zero origin and a step, or `explicit`
with a position vector; an unknown interval never becomes an invented `1.0`,
and wall-clock acquisition time remains provenance. Supported source time units
use pinned exact rational factors followed by one correctly rounded binary64
conversion. Explicit positions begin at positive zero and are strictly
increasing. Identity-bearing property names, enum values, and identifiers use
the profile's strict canonical ASCII grammar. Any permitted Unicode semantic
value is valid UTF-8 normalized to NFC under the Unicode version pinned by
`sc-v1`. Human prose remains excluded.

Each tile leaf hashes its domain, layer key, dtype, origin, edge extent,
validity, and canonical values with explicit length framing. Starting at the
leaf sequence, every internal node consumes exactly the next `1024` consecutive
children except the final partial node at that level; levels repeat in order
until one root remains, without adding a singleton level. Zero-length axes and
datasets with no logical layer are invalid, so there is no empty scientific
tree. Layer roots bind canonical layer descriptors and tree roots; the dataset
root binds the canonical dataset descriptor and ordered layer roots. The exact
domains, binary lengths, grouping, and published independent test vectors are
normative parts of `sc-v1`.

This intentionally preserves scientific identity across recompression,
resharding, physical-channel reordering with unchanged logical mapping,
regenerated pyramids/indexes, package relocation, and provenance sanitation.
It changes for one valid voxel bit, one validity bit, dtype, shape, axis/unit,
geometry, logical-layer mapping, or whitelisted scientific metadata.

#### Exact Object And Package Identity

Every finalized package has a canonical, bounded, path-sorted object manifest.
Every object descriptor contains a strict normalized lowercase-ASCII relative
path, separate typed media and logical-role identifiers, decimal-string byte
length, and exact SHA-256 of the object bytes. Absolute paths, `.`/`..`,
symlinks, duplicate normalized paths, unexpected finalized files, and unlisted
objects are invalid.

The manifest covers all delivered Zarr metadata and shards, packed indexes,
portable provenance/default records, and other package objects. Filesystem
ownership, permissions, timestamps, and enumeration order are not package
objects and are excluded.

Entries are path sorted and packed greedily into canonical JCS pages: append
the next entry if and only if the resulting exact canonical page is no larger
than `1,048,576` UTF-8 bytes; otherwise close the nonempty page and start the
next. A single entry that cannot fit is invalid. The fixed-path canonical root
manifest binds every page's path bounds, entry count, exact byte length, and
SHA-256. Pages are authenticated control objects referenced directly by the
root and never list themselves as payload entries.
`PackageId` is the SHA-256 of those exact root-manifest bytes. The root does not
list itself because its own bytes are hashed directly; every page and payload
object is included transitively. No other file is valid inside the finalized
`.m4d` package. Detached signatures, mutable locator catalogs, and archive
wrappers live outside the package and bind its ID externally.

Any changed byte or path in that closure—including recompression, resharding,
index regeneration, provenance sanitation, or a dataset-default change—creates
a new package ID even when the scientific ID is preserved. Runtime subchunk
checksums remain integrity accelerators, not identities.

#### Recipe, Derivation, Release, And Artifact Identity

`RecipeId` hashes a canonical typed operation DAG: registered operation name,
explicit semantic algorithm version, typed parameters, roles/schemas,
dtype/rounding, reduction or resampling kernel, boundary/interpolation/no-data
policy, ordering/precision policy, and RNG algorithm plus mandatory seed when
applicable. Every recipe declares whether it is bit-exact, numerically bounded,
or non-deterministic. App build, host, operator, paths, timestamps, and progress
are execution provenance, not recipe identity.

`DerivationRecordId` is deliberately an exact execution/provenance record, not
a second abstract algorithm identity. It binds `RecipeId` to named input
scientific/object IDs, named output scientific/artifact IDs, selected layer/
time/space scopes, implementation/build provenance, execution outcome, and
exact/approximate status. Storage packaging and acceleration generation are
recipes too, but they change package/derivation-record identity rather than
scientific identity.

An immutable release record uses a curator-assigned random dataset-series UUID
plus a monotonic release ordinal and `ReleaseId`. Its digest binds scientific
and package IDs, recipes/derivation records, portable provenance, schemas/profiles,
license/rights, citation, creators/institution/funders, evidence, publication
time, and any earlier releases it `supersedes`. A future reverse
`superseded_by` link belongs to the successor or a mutable catalog and never
mutates the old release. Mutable mirrors/local paths stay outside the digest;
a DOI is an external alias, not the identity.

Project-local mutable handles remain opaque UI identities. Every finalized
artifact version separately records an `ArtifactContentId`, exact object
descriptor, typed artifact schema/role, input scientific/layer IDs, and any
`DerivationRecordId`. Every admitted artifact role freezes a closed
inclusion/exclusion schema. By default the content ID includes schema/role,
typed semantic payload, units/coordinate frame, validity/completeness/
exactness, and source scientific/layer IDs whenever interpreting that payload
depends on them. It excludes human names, UI order/visibility/style, paths,
storage encoding, timestamps/operator, and execution provenance; the project
handle and derivation record own those facts. A role cannot ship until its
exceptions are explicit and test-vector covered. A changed table/plot payload
cannot remain silently valid merely because its human ID or path is unchanged.

#### Canonicalization And Publication Companions

[RFC 8785 JSON Canonicalization Scheme](https://www.rfc-editor.org/rfc/rfc8785.html)
is used only for bounded descriptors/pages after M4D semantic normalization,
not pixel payloads or a giant manifest. Every set-like array has a schema-fixed
sort key; map keys are unique; duplicate keys, invalid Unicode, and
schema-forbidden JSON floating numbers are rejected. Identity-bearing unsigned
integers use decimal strings matching exactly `0|[1-9][0-9]*`; signed fields
receive an equally explicit per-field grammar. Floats use the bit
representation above. These rules avoid I-JSON number loss, signed-zero,
Unicode, and semantic-order ambiguity that JCS alone does not resolve.
Descriptor triples are only inspired by the useful
[OCI descriptor](https://specs.opencontainers.org/image-spec/descriptor/)
concept of media type, byte size, and raw algorithm-qualified digest; M4D uses
its own typed media/role fields and decimal-string sizes and is not an OCI
image. SHA-256 follows
[NIST FIPS 180-4](https://csrc.nist.gov/pubs/fips/180-4/upd1/final).

[RO-Crate 1.3](https://www.researchobject.org/ro-crate/specification/1.3/)
is the recommended small publication/provenance/citation companion referencing
both scientific and package identities. It remains outside the `.m4d` package
closure so referencing the package ID creates no cycle; it is descriptive
metadata, not the exhaustive inventory or fixity authority. Raw JSON-LD
serialization is not scientific identity. BagIt may wrap a deposit when an
archive requires it; an external preservation repository may store each
release as an OCFL Object/version. Neither becomes the internal `.m4d` layout
or bootstrap authority.

Import/preprocessing computes scientific leaves and exact-object digests while
bytes are already streaming, persists immutable bounded descriptors, and
recomputes only affected tree paths. It must not reread terabytes merely to add
an optional second checksum. Existing unauthenticated data necessarily needs
one full scan before it can receive a verified scientific content identity.
