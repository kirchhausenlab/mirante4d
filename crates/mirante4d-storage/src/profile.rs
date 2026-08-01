use crate::{PROFILE_PYRAMID_SCALE_COUNT_MAX, ProfileLimits};

/// Exact experimental compatibility tuple accepted for WP-10A.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompatibilityTuple {
    pub format_family: &'static str,
    pub lifecycle: &'static str,
    pub semantic_schema: &'static str,
    pub storage_profile: &'static str,
    pub index_profile: &'static str,
    pub identity_profile: &'static str,
    pub ome_metadata_version: &'static str,
    pub ome_release: &'static str,
    pub zarr_format: u8,
    pub zarr_core: &'static str,
}

pub const PROFILE: CompatibilityTuple = CompatibilityTuple {
    format_family: "mirante4d",
    lifecycle: "EXPERIMENTAL",
    semantic_schema: "m4d-science-1.0",
    storage_profile: "m4d-zarr3-local-1.0",
    index_profile: "m4d-packed-index-1.0",
    identity_profile: "m4d-id-1",
    ome_metadata_version: "0.5",
    ome_release: "0.5.2",
    zarr_format: 3,
    zarr_core: "3.0",
};

pub const CAPABILITIES: [&str; 5] = [
    "m4d.bit-validity.v1",
    "m4d.identity.v1",
    "m4d.packed-index.v1",
    "m4d.strict-profile.v1",
    "zarr.sharding-indexed.v1",
];

pub const CHUNK_KEY_SEPARATOR: &str = "/";
pub const OUTER_CODEC: &str = "sharding_indexed 1.0";
pub const INNER_CODECS: [&str; 3] = [
    "bytes little-endian",
    "zstd level 3 checksum false",
    "crc32c",
];
pub const INDEX_CODECS: [&str; 2] = ["bytes little-endian", "crc32c"];
pub const INDEX_LOCATION: &str = "end";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageShape {
    pub inner_tczyx: [u64; 5],
    pub outer_tczyx: [u64; 5],
}

impl StorageShape {
    pub const PIXEL_3D: Self = Self {
        inner_tczyx: [1, 1, 64, 64, 64],
        outer_tczyx: [1, 1, 256, 256, 256],
    };
    pub const PIXEL_2D: Self = Self {
        inner_tczyx: [1, 1, 1, 256, 256],
        outer_tczyx: [1, 1, 1, 1024, 1024],
    };
    pub const VALIDITY_3D: Self = Self {
        inner_tczyx: [1, 1, 64, 64, 8],
        outer_tczyx: [1, 1, 256, 256, 32],
    };
    pub const VALIDITY_2D: Self = Self {
        inner_tczyx: [1, 1, 1, 256, 32],
        outer_tczyx: [1, 1, 1, 1024, 128],
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileKind {
    /// The sole compositional safety contract for the active experimental
    /// package format. This is not a dataset-size class calibrated from a
    /// representative acquisition.
    Current,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleCountRule {
    Maximum(u64),
    Exact(u64),
}

impl ScaleCountRule {
    pub const fn maximum(self) -> u64 {
        match self {
            Self::Maximum(value) | Self::Exact(value) => value,
        }
    }
}

impl ProfileKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Current => "M4D-COMPOSITIONAL-1",
        }
    }
}

pub const fn profile_limits(profile: ProfileKind) -> ProfileLimits {
    match profile {
        ProfileKind::Current => ProfileLimits::new(
            ScaleCountRule::Maximum(PROFILE_PYRAMID_SCALE_COUNT_MAX),
            None,
            crate::COMPOSITIONAL_LOGICAL_BRICKS_MAX,
            crate::COMPOSITIONAL_SHARDS_PER_COMPONENT_MAX,
            crate::COMPOSITIONAL_SHARDS_PER_COMPONENT_MAX,
            crate::COMPOSITIONAL_SHARDS_PER_COMPONENT_MAX,
            crate::COMPOSITIONAL_ZARR_METADATA_OBJECTS_MAX,
            crate::MANIFEST_PAGE_REFERENCES_MAX,
            crate::COMPOSITIONAL_PHYSICAL_OBJECTS_MAX,
            crate::COMPOSITIONAL_DIRECTORIES_MAX,
            crate::COMPOSITIONAL_DIRECTORY_FAN_OUT_MAX,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_sorted_and_unique() {
        assert!(CAPABILITIES.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn profile_tuple_is_the_only_frozen_experimental_tuple() {
        assert_eq!(PROFILE.storage_profile, "m4d-zarr3-local-1.0");
        assert_eq!(PROFILE.ome_release, "0.5.2");
        assert_eq!(PROFILE.zarr_format, 3);
        assert_eq!(PROFILE.zarr_core, "3.0");
        assert_eq!(StorageShape::PIXEL_3D.outer_tczyx, [1, 1, 256, 256, 256]);
        assert_eq!(INNER_CODECS[1], "zstd level 3 checksum false");
        assert_eq!(INDEX_LOCATION, "end");
        assert_eq!(
            profile_limits(ProfileKind::Current).scales.maximum(),
            PROFILE_PYRAMID_SCALE_COUNT_MAX
        );
    }
}
