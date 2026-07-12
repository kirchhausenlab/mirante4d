use crate::ProfileLimits;

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
    Ds0,
    Ds1,
    Ds2,
    Ds3,
    Ds4,
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
            Self::Ds0 => "DS-0",
            Self::Ds1 => "DS-1",
            Self::Ds2 => "DS-2",
            Self::Ds3 => "DS-3",
            Self::Ds4 => "DS-4",
        }
    }
}

pub const fn profile_limits(profile: ProfileKind) -> ProfileLimits {
    match profile {
        ProfileKind::Ds0 => ProfileLimits::new(
            ScaleCountRule::Maximum(7),
            Some(67_108_864),
            4_096,
            64,
            64,
            7,
            92,
            1,
            256,
            128,
            64,
        ),
        ProfileKind::Ds1 => ProfileLimits::new(
            ScaleCountRule::Maximum(5),
            None,
            3_314,
            76,
            76,
            5,
            68,
            1,
            256,
            256,
            32,
        ),
        ProfileKind::Ds2 => ProfileLimits::new(
            ScaleCountRule::Exact(1),
            None,
            2_048,
            32,
            32,
            1,
            20,
            1,
            128,
            256,
            64,
        ),
        ProfileKind::Ds3 => ProfileLimits::new(
            ScaleCountRule::Exact(7),
            None,
            109_196,
            2_014,
            2_014,
            12,
            92,
            3,
            4_352,
            512,
            32,
        ),
        ProfileKind::Ds4 => ProfileLimits::new(
            ScaleCountRule::Exact(4),
            None,
            86_870,
            5_475,
            5_475,
            8,
            56,
            6,
            11_264,
            14_336,
            512,
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
    }
}
