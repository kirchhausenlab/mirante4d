use mirante4d_identity::{
    ExactBytesDigest, ExactBytesFacts, ExactBytesHasher, MediaType, ObjectRole, PackageId,
    RawObjectDescriptor,
};
use serde::{Deserialize, Serialize};

use super::{
    ControlError, IMAGE_COUNT_MAX, LEVEL_COUNT_MAX, MAX_PORTABLE_CONTROL_OBJECT_BYTES,
    PORTABLE_RECORD_COUNT_MAX, U64Decimal, jcs,
};
use crate::{PackagePath, ProfileKind, profile_limits};

const DESCRIPTOR_OBJECT: &str = "manifest descriptor";
const PAGE_OBJECT: &str = "manifest page";
const ROOT_OBJECT: &str = "manifest root";
const PAGE_SCHEMA: &str = "m4d-manifest-page";
const ROOT_SCHEMA: &str = "m4d-manifest-root";
const MAX_DESCRIPTOR_BYTES: usize = 512;
const MAX_MANIFEST_DESCRIPTORS: usize =
    profile_limits(ProfileKind::Ds4).total_physical_objects as usize;
const MAX_MANIFEST_PAGES: usize = profile_limits(ProfileKind::Ds4).manifest_pages as usize;

/// The closed object kinds admitted by the experimental package manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageObjectKind {
    ZarrRoot,
    ZarrImagesGroup,
    ZarrValidityGroup,
    ZarrIndexesGroup,
    ZarrImageGroup,
    ZarrPixelArray,
    ZarrValidityArray,
    ZarrPackedIndexArray,
    PixelShard,
    ValidityShard,
    PackedIndexShard,
    Profile,
    Science,
    DisplayDefaults,
    PortableRecord,
}

impl PackageObjectKind {
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::ZarrRoot
            | Self::ZarrImagesGroup
            | Self::ZarrValidityGroup
            | Self::ZarrIndexesGroup
            | Self::ZarrImageGroup
            | Self::ZarrPixelArray
            | Self::ZarrValidityArray
            | Self::ZarrPackedIndexArray => "application/vnd.zarr+json",
            Self::PixelShard | Self::ValidityShard | Self::PackedIndexShard => {
                "application/vnd.zarr.shard"
            }
            Self::Profile => "application/vnd.mirante4d.profile+json",
            Self::Science => "application/vnd.mirante4d.science+json",
            Self::DisplayDefaults => "application/vnd.mirante4d.display-defaults+json",
            Self::PortableRecord => "application/vnd.mirante4d.portable-record+json",
        }
    }

    pub const fn logical_role(self) -> &'static str {
        match self {
            Self::ZarrRoot => "zarr.root",
            Self::ZarrImagesGroup => "zarr.images-group",
            Self::ZarrValidityGroup => "zarr.validity-group",
            Self::ZarrIndexesGroup => "zarr.indexes-group",
            Self::ZarrImageGroup => "zarr.image-group",
            Self::ZarrPixelArray => "zarr.pixel-array",
            Self::ZarrValidityArray => "zarr.validity-array",
            Self::ZarrPackedIndexArray => "zarr.packed-index-array",
            Self::PixelShard => "pixel.shard",
            Self::ValidityShard => "validity.shard",
            Self::PackedIndexShard => "packed-index.shard",
            Self::Profile => "m4d.profile",
            Self::Science => "m4d.science",
            Self::DisplayDefaults => "m4d.display-defaults",
            Self::PortableRecord => "m4d.portable-record",
        }
    }

    fn parse(media_type: &str, logical_role: &str) -> Result<Self, ControlError> {
        const KINDS: [PackageObjectKind; 15] = [
            PackageObjectKind::ZarrRoot,
            PackageObjectKind::ZarrImagesGroup,
            PackageObjectKind::ZarrValidityGroup,
            PackageObjectKind::ZarrIndexesGroup,
            PackageObjectKind::ZarrImageGroup,
            PackageObjectKind::ZarrPixelArray,
            PackageObjectKind::ZarrValidityArray,
            PackageObjectKind::ZarrPackedIndexArray,
            PackageObjectKind::PixelShard,
            PackageObjectKind::ValidityShard,
            PackageObjectKind::PackedIndexShard,
            PackageObjectKind::Profile,
            PackageObjectKind::Science,
            PackageObjectKind::DisplayDefaults,
            PackageObjectKind::PortableRecord,
        ];
        KINDS
            .into_iter()
            .find(|kind| kind.media_type() == media_type && kind.logical_role() == logical_role)
            .ok_or(ControlError::InvalidControlObject {
                object: DESCRIPTOR_OBJECT,
                reason: "media_type and logical_role are not an admitted pair",
            })
    }
}

/// One path-bound exact object descriptor in a package manifest page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageObjectDescriptor {
    path: PackagePath,
    kind: PackageObjectKind,
    raw: RawObjectDescriptor,
}

impl PackageObjectDescriptor {
    pub fn new(
        path: PackagePath,
        kind: PackageObjectKind,
        byte_length: u64,
        digest: ExactBytesDigest,
    ) -> Result<Self, ControlError> {
        validate_kind_path(kind, &path)?;
        let media_type = MediaType::parse(kind.media_type()).map_err(|_| invalid_descriptor())?;
        let logical_role =
            ObjectRole::parse(kind.logical_role()).map_err(|_| invalid_descriptor())?;
        let value = Self {
            path,
            kind,
            raw: RawObjectDescriptor::new(digest, byte_length, media_type, logical_role),
        };
        value.canonical_bytes()?;
        Ok(value)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_DESCRIPTOR_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: DESCRIPTOR_OBJECT,
                maximum: MAX_DESCRIPTOR_BYTES,
            });
        }
        let wire: WirePackageObjectDescriptor = parse_json(bytes, DESCRIPTOR_OBJECT)?;
        let value = Self::try_from(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject {
                object: DESCRIPTOR_OBJECT,
            });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        validate_kind_path(self.kind, &self.path)?;
        if self.raw.media_type().as_str() != self.kind.media_type()
            || self.raw.role().as_str() != self.kind.logical_role()
        {
            return invalid(DESCRIPTOR_OBJECT, "descriptor registry facts drifted");
        }
        encode_wire(
            WirePackageObjectDescriptor::from(self),
            DESCRIPTOR_OBJECT,
            MAX_DESCRIPTOR_BYTES,
        )
    }

    pub const fn kind(&self) -> PackageObjectKind {
        self.kind
    }

    pub const fn path(&self) -> &PackagePath {
        &self.path
    }

    pub const fn raw(&self) -> &RawObjectDescriptor {
        &self.raw
    }
}

/// One bounded canonical page of path-sorted package object descriptors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestPage {
    entries: Vec<PackageObjectDescriptor>,
}

impl ManifestPage {
    pub fn new(entries: Vec<PackageObjectDescriptor>) -> Result<Self, ControlError> {
        if entries.len() > MAX_MANIFEST_DESCRIPTORS {
            return invalid(
                PAGE_OBJECT,
                "manifest descriptor count exceeds the package-wide maximum",
            );
        }
        validate_entry_order(&entries)?;
        let page = Self { entries };
        page.canonical_bytes()?;
        Ok(page)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: PAGE_OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireManifestPage = parse_json(bytes, PAGE_OBJECT)?;
        let value = Self::try_from(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject {
                object: PAGE_OBJECT,
            });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        validate_entry_order(&self.entries)?;
        encode_page_entries(&self.entries)
    }

    pub fn exact_bytes_facts(&self) -> Result<ExactBytesFacts, ControlError> {
        let bytes = self.canonical_bytes()?;
        ExactBytesHasher::hash(&bytes).map_err(|_| ControlError::InvalidControlObject {
            object: PAGE_OBJECT,
            reason: "page byte length exceeds exact-object framing",
        })
    }

    pub fn entries(&self) -> &[PackageObjectDescriptor] {
        &self.entries
    }

    pub fn first_path(&self) -> &PackagePath {
        &self.entries[0].path
    }

    pub fn last_path(&self) -> &PackagePath {
        &self.entries[self.entries.len() - 1].path
    }
}

/// One exact authenticated reference from the manifest root to a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestPageReference {
    path: PackagePath,
    first_path: PackagePath,
    last_path: PackagePath,
    entry_count: u64,
    byte_length: u64,
    digest: ExactBytesDigest,
}

impl ManifestPageReference {
    fn from_page(ordinal: u32, page: &ManifestPage) -> Result<Self, ControlError> {
        let facts = page.exact_bytes_facts()?;
        Ok(Self {
            path: manifest_page_path(ordinal)?,
            first_path: page.first_path().clone(),
            last_path: page.last_path().clone(),
            entry_count: u64::try_from(page.entries.len()).map_err(|_| {
                ControlError::InvalidControlObject {
                    object: ROOT_OBJECT,
                    reason: "page entry count exceeds u64",
                }
            })?,
            byte_length: facts.byte_length(),
            digest: facts.digest(),
        })
    }

    pub const fn path(&self) -> &PackagePath {
        &self.path
    }

    pub const fn first_path(&self) -> &PackagePath {
        &self.first_path
    }

    pub const fn last_path(&self) -> &PackagePath {
        &self.last_path
    }

    pub const fn entry_count(&self) -> u64 {
        self.entry_count
    }

    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub const fn digest(&self) -> ExactBytesDigest {
        self.digest
    }
}

/// The bounded authenticated manifest root whose exact bytes define PackageId.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestRoot {
    pages: Vec<ManifestPageReference>,
}

impl ManifestRoot {
    pub fn new(pages: &[ManifestPage]) -> Result<Self, ControlError> {
        validate_greedy_pages(pages)?;
        let references = pages
            .iter()
            .enumerate()
            .map(|(ordinal, page)| {
                let ordinal =
                    u32::try_from(ordinal).map_err(|_| ControlError::InvalidControlObject {
                        object: ROOT_OBJECT,
                        reason: "manifest page ordinal exceeds u32",
                    })?;
                ManifestPageReference::from_page(ordinal, page)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let root = Self { pages: references };
        root.canonical_bytes()?;
        Ok(root)
    }

    /// Parses only the canonical root structure and its page references.
    /// Call [`Self::verify_pages`] with pages decoded from their exact raw
    /// bytes before treating the manifest page closure as authenticated.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ControlError> {
        if bytes.len() > MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return Err(ControlError::ControlObjectTooLarge {
                object: ROOT_OBJECT,
                maximum: MAX_PORTABLE_CONTROL_OBJECT_BYTES,
            });
        }
        let wire: WireManifestRoot = parse_json(bytes, ROOT_OBJECT)?;
        let value = Self::try_from(wire)?;
        if value.canonical_bytes()?.as_slice() != bytes {
            return Err(ControlError::NonCanonicalControlObject {
                object: ROOT_OBJECT,
            });
        }
        Ok(value)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        validate_page_references(&self.pages)?;
        encode_wire(
            WireManifestRoot::from(self),
            ROOT_OBJECT,
            MAX_PORTABLE_CONTROL_OBJECT_BYTES,
        )
    }

    pub fn package_id(&self) -> Result<PackageId, ControlError> {
        // This identifies the exact root bytes. Full package verification must
        // additionally authenticate raw page and payload bytes and reject
        // unlisted filesystem objects.
        Ok(PackageId::from_manifest_root_bytes(
            &self.canonical_bytes()?,
        ))
    }

    /// Verifies canonical page values against this root and the greedy packing.
    ///
    /// A filesystem reader must first parse the exact raw page bytes and must
    /// separately verify every described payload and the absence of extras.
    pub fn verify_pages(&self, pages: &[ManifestPage]) -> Result<(), ControlError> {
        if Self::new(pages)?.pages != self.pages {
            return invalid(
                ROOT_OBJECT,
                "page contents do not verify the root references",
            );
        }
        Ok(())
    }

    pub fn pages(&self) -> &[ManifestPageReference] {
        &self.pages
    }
}

/// Returns the sole canonical path for one zero-based manifest page ordinal.
pub fn manifest_page_path(ordinal: u32) -> Result<PackagePath, ControlError> {
    if usize::try_from(ordinal).map_or(true, |ordinal| ordinal >= MAX_MANIFEST_PAGES) {
        return invalid(ROOT_OBJECT, "manifest page ordinal exceeds five");
    }
    package_path(
        &format!("m4d/manifest/pages/p{ordinal:08}.json"),
        ROOT_OBJECT,
    )
}

/// Sorts descriptors and packs the exact canonical greedy page sequence.
pub fn pack_manifest_pages(
    mut entries: Vec<PackageObjectDescriptor>,
) -> Result<Vec<ManifestPage>, ControlError> {
    if entries.is_empty() {
        return invalid(
            PAGE_OBJECT,
            "a manifest must contain at least one descriptor",
        );
    }
    if entries.len() > MAX_MANIFEST_DESCRIPTORS {
        return invalid(
            PAGE_OBJECT,
            "manifest descriptor count exceeds the package-wide maximum",
        );
    }
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return invalid(PAGE_OBJECT, "manifest descriptor paths must be unique");
    }

    let empty_page_size = encode_page_entries(&[])?.len();
    let mut pages = Vec::new();
    let mut current = Vec::new();
    let mut current_size = empty_page_size;
    for entry in entries {
        let descriptor_size = entry.canonical_bytes()?.len();
        let separator = usize::from(!current.is_empty());
        let candidate_size = current_size
            .checked_add(separator)
            .and_then(|size| size.checked_add(descriptor_size))
            .ok_or(ControlError::InvalidControlObject {
                object: PAGE_OBJECT,
                reason: "manifest page size overflowed usize",
            })?;
        if candidate_size <= MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            current.push(entry);
            current_size = candidate_size;
            continue;
        }
        if current.is_empty() {
            return invalid(PAGE_OBJECT, "one descriptor cannot fit in a manifest page");
        }
        pages.push(ManifestPage::new(std::mem::take(&mut current))?);
        current_size = empty_page_size.checked_add(descriptor_size).ok_or(
            ControlError::InvalidControlObject {
                object: PAGE_OBJECT,
                reason: "manifest page size overflowed usize",
            },
        )?;
        current.push(entry);
    }
    if !current.is_empty() {
        pages.push(ManifestPage::new(current)?);
    }
    if pages.len() > MAX_MANIFEST_PAGES {
        return invalid(PAGE_OBJECT, "manifest requires more than six pages");
    }
    Ok(pages)
}

fn validate_greedy_pages(pages: &[ManifestPage]) -> Result<(), ControlError> {
    if pages.is_empty() || pages.len() > MAX_MANIFEST_PAGES {
        return invalid(
            ROOT_OBJECT,
            "manifest root must reference one through six pages",
        );
    }
    let mut descriptor_count = 0_usize;
    for (ordinal, page) in pages.iter().enumerate() {
        validate_entry_order(&page.entries)?;
        descriptor_count = descriptor_count.checked_add(page.entries.len()).ok_or(
            ControlError::InvalidControlObject {
                object: ROOT_OBJECT,
                reason: "manifest descriptor count overflowed usize",
            },
        )?;
        if descriptor_count > MAX_MANIFEST_DESCRIPTORS {
            return invalid(
                ROOT_OBJECT,
                "manifest descriptor count exceeds the package-wide maximum",
            );
        }

        let Some(next_page) = pages.get(ordinal + 1) else {
            continue;
        };
        if page.last_path() >= next_page.first_path() {
            return invalid(
                ROOT_OBJECT,
                "manifest page path ranges must be strictly increasing",
            );
        }
        let page_size = page.canonical_bytes()?.len();
        let next_descriptor_size = next_page.entries[0].canonical_bytes()?.len();
        let appended_size = page_size
            .checked_add(1)
            .and_then(|size| size.checked_add(next_descriptor_size))
            .ok_or(ControlError::InvalidControlObject {
                object: ROOT_OBJECT,
                reason: "manifest page boundary size overflowed usize",
            })?;
        if appended_size <= MAX_PORTABLE_CONTROL_OBJECT_BYTES {
            return invalid(
                ROOT_OBJECT,
                "manifest pages are not the exact greedy packing",
            );
        }
    }
    Ok(())
}

fn validate_entry_order(entries: &[PackageObjectDescriptor]) -> Result<(), ControlError> {
    if entries.is_empty() {
        return invalid(PAGE_OBJECT, "manifest pages must be nonempty");
    }
    if !entries.windows(2).all(|pair| pair[0].path < pair[1].path) {
        return invalid(
            PAGE_OBJECT,
            "manifest entries must be strictly path-sorted and unique",
        );
    }
    for entry in entries {
        entry.canonical_bytes()?;
    }
    Ok(())
}

fn validate_page_references(pages: &[ManifestPageReference]) -> Result<(), ControlError> {
    if pages.is_empty() || pages.len() > MAX_MANIFEST_PAGES {
        return invalid(
            ROOT_OBJECT,
            "manifest root must reference one through six pages",
        );
    }
    let mut descriptor_count = 0_u64;
    for (ordinal, page) in pages.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| ControlError::InvalidControlObject {
            object: ROOT_OBJECT,
            reason: "manifest page ordinal exceeds u32",
        })?;
        if page.path != manifest_page_path(ordinal)? {
            return invalid(
                ROOT_OBJECT,
                "manifest page paths must be contiguous from zero",
            );
        }
        if page.entry_count == 0
            || page.byte_length == 0
            || page.byte_length > MAX_PORTABLE_CONTROL_OBJECT_BYTES as u64
            || page.first_path > page.last_path
        {
            return invalid(ROOT_OBJECT, "manifest page reference bounds are invalid");
        }
        if ordinal != 0 && pages[ordinal as usize - 1].last_path >= page.first_path {
            return invalid(
                ROOT_OBJECT,
                "manifest page path ranges must be strictly increasing and nonoverlapping",
            );
        }
        descriptor_count = descriptor_count.checked_add(page.entry_count).ok_or(
            ControlError::InvalidControlObject {
                object: ROOT_OBJECT,
                reason: "manifest descriptor count overflowed u64",
            },
        )?;
    }
    if descriptor_count > MAX_MANIFEST_DESCRIPTORS as u64 {
        return invalid(
            ROOT_OBJECT,
            "manifest descriptor count exceeds the package-wide maximum",
        );
    }
    Ok(())
}

fn validate_kind_path(kind: PackageObjectKind, path: &PackagePath) -> Result<(), ControlError> {
    let components = path.as_str().split('/').collect::<Vec<_>>();
    let valid = match kind {
        PackageObjectKind::ZarrRoot => components == ["zarr.json"],
        PackageObjectKind::ZarrImagesGroup => components == ["images", "zarr.json"],
        PackageObjectKind::ZarrValidityGroup => components == ["validity", "zarr.json"],
        PackageObjectKind::ZarrIndexesGroup => components == ["indexes", "zarr.json"],
        PackageObjectKind::ZarrImageGroup => {
            components.len() == 3
                && components[0] == "images"
                && image_ordinal(components[1])
                && components[2] == "zarr.json"
        }
        PackageObjectKind::ZarrPixelArray => {
            components.len() == 4
                && components[0] == "images"
                && image_ordinal(components[1])
                && scale_ordinal(components[2])
                && components[3] == "zarr.json"
        }
        PackageObjectKind::ZarrValidityArray => {
            components.len() == 3
                && components[0] == "validity"
                && image_scale(components[1])
                && components[2] == "zarr.json"
        }
        PackageObjectKind::ZarrPackedIndexArray => {
            components.len() == 3
                && components[0] == "indexes"
                && image_scale(components[1])
                && components[2] == "zarr.json"
        }
        PackageObjectKind::PixelShard => {
            components.len() == 9
                && components[0] == "images"
                && image_ordinal(components[1])
                && scale_ordinal(components[2])
                && components[3] == "c"
                && coordinates(&components[4..], 5)
        }
        PackageObjectKind::ValidityShard => {
            components.len() == 8
                && components[0] == "validity"
                && image_scale(components[1])
                && components[2] == "c"
                && coordinates(&components[3..], 5)
        }
        PackageObjectKind::PackedIndexShard => {
            components.len() == 5
                && components[0] == "indexes"
                && image_scale(components[1])
                && components[2] == "c"
                && coordinates(&components[3..4], 1)
                && components[4] == "0"
        }
        PackageObjectKind::Profile => path.as_str() == "m4d/profile.json",
        PackageObjectKind::Science => path.as_str() == "m4d/science.json",
        PackageObjectKind::DisplayDefaults => path.as_str() == "m4d/display.json",
        PackageObjectKind::PortableRecord => portable_record_path(path.as_str()),
    };
    if !valid {
        return invalid(
            DESCRIPTOR_OBJECT,
            "path does not match the registered logical role",
        );
    }
    Ok(())
}

fn image_ordinal(value: &str) -> bool {
    indexed_ordinal(value, 'i', 8, IMAGE_COUNT_MAX as u32)
}

fn scale_ordinal(value: &str) -> bool {
    indexed_ordinal(value, 's', 2, LEVEL_COUNT_MAX as u32)
}

fn image_scale(value: &str) -> bool {
    value
        .split_once('-')
        .is_some_and(|(image, scale)| image_ordinal(image) && scale_ordinal(scale))
}

fn indexed_ordinal(value: &str, prefix: char, width: usize, exclusive_max: u32) -> bool {
    let Some(digits) = value.strip_prefix(prefix) else {
        return false;
    };
    digits.len() == width
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits
            .parse::<u32>()
            .is_ok_and(|ordinal| ordinal < exclusive_max)
}

fn coordinates(values: &[&str], count: usize) -> bool {
    values.len() == count && values.iter().all(|value| U64Decimal::parse(value).is_ok())
}

fn portable_record_path(path: &str) -> bool {
    path.strip_prefix("m4d/records/r")
        .and_then(|value| value.strip_suffix(".json"))
        .is_some_and(|digits| {
            digits.len() == 8
                && digits.bytes().all(|byte| byte.is_ascii_digit())
                && digits
                    .parse::<usize>()
                    .is_ok_and(|ordinal| ordinal < PORTABLE_RECORD_COUNT_MAX)
        })
}

fn package_path(value: &str, object: &'static str) -> Result<PackagePath, ControlError> {
    PackagePath::parse(value).map_err(|_| ControlError::InvalidControlObject {
        object,
        reason: "path violates the portable package-path grammar",
    })
}

fn parse_json<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    object: &'static str,
) -> Result<T, ControlError> {
    serde_json::from_slice(bytes).map_err(|error| ControlError::MalformedControlObject {
        object,
        detail: error.to_string(),
    })
}

fn encode_wire<T: Serialize>(
    wire: T,
    object: &'static str,
    maximum: usize,
) -> Result<Vec<u8>, ControlError> {
    let value =
        serde_json::to_value(wire).map_err(|error| ControlError::MalformedControlObject {
            object,
            detail: error.to_string(),
        })?;
    jcs::encode(&value, object, maximum)
}

fn encode_page_entries(entries: &[PackageObjectDescriptor]) -> Result<Vec<u8>, ControlError> {
    encode_wire(
        WireManifestPage {
            schema: PAGE_SCHEMA.to_owned(),
            schema_version: 1,
            entries: entries
                .iter()
                .map(WirePackageObjectDescriptor::from)
                .collect(),
        },
        PAGE_OBJECT,
        MAX_PORTABLE_CONTROL_OBJECT_BYTES,
    )
}

fn invalid_descriptor() -> ControlError {
    ControlError::InvalidControlObject {
        object: DESCRIPTOR_OBJECT,
        reason: "fixed media_type or logical_role registry value is invalid",
    }
}

fn invalid<T>(object: &'static str, reason: &'static str) -> Result<T, ControlError> {
    Err(ControlError::InvalidControlObject { object, reason })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePackageObjectDescriptor {
    path: String,
    media_type: String,
    logical_role: String,
    bytes: String,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireManifestPage {
    schema: String,
    schema_version: u64,
    entries: Vec<WirePackageObjectDescriptor>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireManifestRoot {
    schema: String,
    schema_version: u64,
    pages: Vec<WireManifestPageReference>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireManifestPageReference {
    path: String,
    first_path: String,
    last_path: String,
    entry_count: String,
    bytes: String,
    digest: String,
}

impl TryFrom<WirePackageObjectDescriptor> for PackageObjectDescriptor {
    type Error = ControlError;

    fn try_from(wire: WirePackageObjectDescriptor) -> Result<Self, Self::Error> {
        MediaType::parse(&wire.media_type).map_err(|_| invalid_descriptor())?;
        ObjectRole::parse(&wire.logical_role).map_err(|_| invalid_descriptor())?;
        let kind = PackageObjectKind::parse(&wire.media_type, &wire.logical_role)?;
        let byte_length = U64Decimal::parse(&wire.bytes)?.get();
        let digest = ExactBytesDigest::parse(&wire.digest).map_err(|_| {
            ControlError::InvalidControlObject {
                object: DESCRIPTOR_OBJECT,
                reason: "digest is not a canonical exact-bytes SHA-256 identifier",
            }
        })?;
        Self::new(
            package_path(&wire.path, DESCRIPTOR_OBJECT)?,
            kind,
            byte_length,
            digest,
        )
    }
}

impl From<&PackageObjectDescriptor> for WirePackageObjectDescriptor {
    fn from(value: &PackageObjectDescriptor) -> Self {
        Self {
            path: value.path.to_string(),
            media_type: value.raw.media_type().to_string(),
            logical_role: value.raw.role().to_string(),
            bytes: value.raw.byte_length().to_string(),
            digest: value.raw.digest().to_string(),
        }
    }
}

impl TryFrom<WireManifestPage> for ManifestPage {
    type Error = ControlError;

    fn try_from(wire: WireManifestPage) -> Result<Self, Self::Error> {
        if wire.schema != PAGE_SCHEMA || wire.schema_version != 1 {
            return invalid(
                PAGE_OBJECT,
                "manifest page schema or version is unsupported",
            );
        }
        Self::new(
            wire.entries
                .into_iter()
                .map(PackageObjectDescriptor::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl TryFrom<WireManifestRoot> for ManifestRoot {
    type Error = ControlError;

    fn try_from(wire: WireManifestRoot) -> Result<Self, Self::Error> {
        if wire.schema != ROOT_SCHEMA || wire.schema_version != 1 {
            return invalid(
                ROOT_OBJECT,
                "manifest root schema or version is unsupported",
            );
        }
        let root = Self {
            pages: wire
                .pages
                .into_iter()
                .map(ManifestPageReference::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        };
        validate_page_references(&root.pages)?;
        Ok(root)
    }
}

impl TryFrom<WireManifestPageReference> for ManifestPageReference {
    type Error = ControlError;

    fn try_from(wire: WireManifestPageReference) -> Result<Self, Self::Error> {
        Ok(Self {
            path: package_path(&wire.path, ROOT_OBJECT)?,
            first_path: package_path(&wire.first_path, ROOT_OBJECT)?,
            last_path: package_path(&wire.last_path, ROOT_OBJECT)?,
            entry_count: U64Decimal::parse(&wire.entry_count)?.get(),
            byte_length: U64Decimal::parse(&wire.bytes)?.get(),
            digest: ExactBytesDigest::parse(&wire.digest).map_err(|_| {
                ControlError::InvalidControlObject {
                    object: ROOT_OBJECT,
                    reason: "manifest page digest is invalid",
                }
            })?,
        })
    }
}

impl From<&ManifestRoot> for WireManifestRoot {
    fn from(value: &ManifestRoot) -> Self {
        Self {
            schema: ROOT_SCHEMA.to_owned(),
            schema_version: 1,
            pages: value
                .pages
                .iter()
                .map(WireManifestPageReference::from)
                .collect(),
        }
    }
}

impl From<&ManifestPageReference> for WireManifestPageReference {
    fn from(value: &ManifestPageReference) -> Self {
        Self {
            path: value.path.to_string(),
            first_path: value.first_path.to_string(),
            last_path: value.last_path.to_string(),
            entry_count: value.entry_count.to_string(),
            bytes: value.byte_length.to_string(),
            digest: value.digest.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(hex: char) -> ExactBytesDigest {
        ExactBytesDigest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn descriptor(
        path: &str,
        kind: PackageObjectKind,
        byte_length: u64,
    ) -> PackageObjectDescriptor {
        PackageObjectDescriptor::new(
            PackagePath::parse(path).unwrap(),
            kind,
            byte_length,
            digest('0'),
        )
        .unwrap()
    }

    #[test]
    fn manifest_pages_and_root_roundtrip_with_exact_package_identity() {
        let entries = vec![
            descriptor("zarr.json", PackageObjectKind::ZarrRoot, 3),
            descriptor("m4d/profile.json", PackageObjectKind::Profile, 2),
        ];
        let pages = pack_manifest_pages(entries).unwrap();
        assert_eq!(pages.len(), 1);

        let expected_page = r#"{"entries":[{"bytes":"2","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","logical_role":"m4d.profile","media_type":"application/vnd.mirante4d.profile+json","path":"m4d/profile.json"},{"bytes":"3","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","logical_role":"zarr.root","media_type":"application/vnd.zarr+json","path":"zarr.json"}],"schema":"m4d-manifest-page","schema_version":1}"#;
        let page_bytes = pages[0].canonical_bytes().unwrap();
        assert_eq!(page_bytes, expected_page.as_bytes());
        assert_eq!(
            ManifestPage::parse_canonical(&page_bytes).unwrap(),
            pages[0]
        );

        let root = ManifestRoot::new(&pages).unwrap();
        root.verify_pages(&pages).unwrap();
        let root_bytes = root.canonical_bytes().unwrap();
        let expected_root = r#"{"pages":[{"bytes":"451","digest":"sha256:7f27a9f7c756a15952b6d8677ae9857c7956a932e44c7cb1c12fbeaa721b7537","entry_count":"2","first_path":"m4d/profile.json","last_path":"zarr.json","path":"m4d/manifest/pages/p00000000.json"}],"schema":"m4d-manifest-root","schema_version":1}"#;
        let expected_package_id = PackageId::parse(concat!(
            "m4d-package-v1-sha256:",
            "6fdaa2bea94a90de377d36ce2fcb490beec8b0ef8bf4dab3a65f257ef86b352e"
        ))
        .unwrap();
        assert_eq!(root_bytes, expected_root.as_bytes());
        assert_eq!(root.package_id().unwrap(), expected_package_id);
        assert_eq!(ManifestRoot::parse_canonical(&root_bytes).unwrap(), root);
        assert_eq!(
            root.package_id().unwrap(),
            PackageId::from_manifest_root_bytes(&root_bytes)
        );
        assert!(!String::from_utf8_lossy(&root_bytes).contains("package_id"));

        let changed = PackageObjectDescriptor::new(
            PackagePath::parse("m4d/profile.json").unwrap(),
            PackageObjectKind::Profile,
            2,
            digest('1'),
        )
        .unwrap();
        let changed_root = ManifestRoot::new(
            &pack_manifest_pages(vec![
                changed,
                descriptor("zarr.json", PackageObjectKind::ZarrRoot, 3),
            ])
            .unwrap(),
        )
        .unwrap();
        assert_ne!(
            root.package_id().unwrap(),
            changed_root.package_id().unwrap()
        );
    }

    #[test]
    fn manifest_rejects_registry_path_order_and_reference_drift() {
        for (path, kind) in [
            ("zarr.json", PackageObjectKind::ZarrRoot),
            ("images/zarr.json", PackageObjectKind::ZarrImagesGroup),
            ("validity/zarr.json", PackageObjectKind::ZarrValidityGroup),
            ("indexes/zarr.json", PackageObjectKind::ZarrIndexesGroup),
            (
                "images/i00000003/zarr.json",
                PackageObjectKind::ZarrImageGroup,
            ),
            (
                "images/i00000003/s06/zarr.json",
                PackageObjectKind::ZarrPixelArray,
            ),
            (
                "validity/i00000003-s06/zarr.json",
                PackageObjectKind::ZarrValidityArray,
            ),
            (
                "indexes/i00000003-s06/zarr.json",
                PackageObjectKind::ZarrPackedIndexArray,
            ),
            (
                "images/i00000003/s06/c/0/1/2/3/4",
                PackageObjectKind::PixelShard,
            ),
            (
                "validity/i00000003-s06/c/0/1/2/3/4",
                PackageObjectKind::ValidityShard,
            ),
            (
                "indexes/i00000003-s06/c/4/0",
                PackageObjectKind::PackedIndexShard,
            ),
            ("m4d/profile.json", PackageObjectKind::Profile),
            ("m4d/science.json", PackageObjectKind::Science),
            ("m4d/display.json", PackageObjectKind::DisplayDefaults),
            (
                "m4d/records/r00000013.json",
                PackageObjectKind::PortableRecord,
            ),
        ] {
            descriptor(path, kind, 1);
        }
        for (path, kind) in [
            ("m4d/manifest/root.json", PackageObjectKind::PortableRecord),
            (
                "m4d/manifest/pages/p00000000.json",
                PackageObjectKind::PortableRecord,
            ),
            (
                "images/i00000004/zarr.json",
                PackageObjectKind::ZarrImageGroup,
            ),
            (
                "images/i00000000/s07/zarr.json",
                PackageObjectKind::ZarrPixelArray,
            ),
            (
                "images/i00000000/s00/c/00/0/0/0/0",
                PackageObjectKind::PixelShard,
            ),
            (
                "indexes/i00000000-s00/c/0/1",
                PackageObjectKind::PackedIndexShard,
            ),
            (
                "m4d/records/r00000014.json",
                PackageObjectKind::PortableRecord,
            ),
        ] {
            assert!(
                PackageObjectDescriptor::new(
                    PackagePath::parse(path).unwrap(),
                    kind,
                    1,
                    digest('0'),
                )
                .is_err(),
                "accepted {path}"
            );
        }
        assert!(
            PackageObjectDescriptor::new(
                PackagePath::parse("zarr.json").unwrap(),
                PackageObjectKind::Profile,
                1,
                digest('0'),
            )
            .is_err()
        );
        assert!(
            PackageObjectDescriptor::parse_canonical(
                br#"{"bytes":"1","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","logical_role":"unknown","media_type":"application/json","path":"zarr.json"}"#
            )
            .is_err()
        );

        let profile = descriptor("m4d/profile.json", PackageObjectKind::Profile, 2);
        let root_zarr = descriptor("zarr.json", PackageObjectKind::ZarrRoot, 3);
        assert!(ManifestPage::new(vec![root_zarr.clone(), profile.clone()]).is_err());
        assert!(ManifestPage::new(vec![profile.clone(), profile.clone()]).is_err());

        let canonical_page = ManifestPage::new(vec![profile.clone(), root_zarr.clone()])
            .unwrap()
            .canonical_bytes()
            .unwrap();
        let duplicate = String::from_utf8(canonical_page.clone()).unwrap().replacen(
            "\"entries\":",
            "\"entries\":[],\"entries\":",
            1,
        );
        for wire in [
            duplicate,
            format!(" {}", String::from_utf8(canonical_page.clone()).unwrap()),
            String::from_utf8(canonical_page).unwrap().replacen(
                "\"schema\":",
                "\"extra\":false,\"schema\":",
                1,
            ),
        ] {
            assert!(ManifestPage::parse_canonical(wire.as_bytes()).is_err());
        }

        let split = vec![
            ManifestPage::new(vec![profile]).unwrap(),
            ManifestPage::new(vec![root_zarr]).unwrap(),
        ];
        assert!(ManifestRoot::new(&split).is_err());

        let pages = pack_manifest_pages(vec![descriptor(
            "m4d/profile.json",
            PackageObjectKind::Profile,
            2,
        )])
        .unwrap();
        let mut root = ManifestRoot::new(&pages).unwrap();
        let canonical_root = String::from_utf8(root.canonical_bytes().unwrap()).unwrap();
        for wire in [
            canonical_root.replacen("\"pages\":", "\"pages\":[],\"pages\":", 1),
            format!(" {canonical_root}"),
            canonical_root.replacen("\"pages\":", "\"extra\":false,\"pages\":", 1),
        ] {
            assert!(ManifestRoot::parse_canonical(wire.as_bytes()).is_err());
        }
        root.pages[0].byte_length += 1;
        assert!(root.verify_pages(&pages).is_err());
        assert!(ManifestRoot::new(&[]).is_err());
    }

    #[test]
    fn two_thousand_max_fact_descriptors_fit_one_linear_greedy_page() {
        let empty_page_overhead = encode_page_entries(&[]).unwrap().len();
        assert!(
            empty_page_overhead + 2_000 * MAX_DESCRIPTOR_BYTES + 1_999
                <= MAX_PORTABLE_CONTROL_OBJECT_BYTES
        );

        let maximum = u64::MAX;
        let entries = (0..2_000_u64)
            .map(|index| {
                descriptor(
                    &format!(
                        "images/i00000000/s00/c/{index}/{maximum}/{maximum}/{maximum}/{maximum}"
                    ),
                    PackageObjectKind::PixelShard,
                    maximum,
                )
            })
            .collect();
        let pages = pack_manifest_pages(entries).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].entries().len(), 2_000);
        assert!(pages[0].canonical_bytes().unwrap().len() <= MAX_PORTABLE_CONTROL_OBJECT_BYTES);
    }
}
