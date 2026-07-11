//! Immutable, framework-neutral dataset catalog values.
//!
//! This crate describes scientific layers already discovered by a dataset
//! source. It owns no filesystem access, serialization, storage layout,
//! decoding, scheduling, cache, lease, or runtime behavior.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::ScientificContentId;
use thiserror::Error;

pub const MAX_DATASET_LABEL_BYTES: usize = 256;
pub const MAX_LAYER_LABEL_BYTES: usize = 256;
pub const MAX_DATASET_LAYERS: usize = 4_096;

/// Whether the catalog has been bound to verified scientific content.
///
/// `Unverified` carries no substitute identifier. In particular, a package
/// slug, path, manifest value, or cache digest cannot be represented as a
/// verified scientific identity through this type.
///
/// This checkpoint-A value records a classification; constructing it is not a
/// verifier capability and does not authorize application attachment. The
/// verifier-owned admission route is introduced by WP-08, while the WP-07B
/// application boundary intentionally exposes no public verification command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScientificIdentityStatus {
    Unverified,
    Verified(ScientificContentId),
}

impl ScientificIdentityStatus {
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    pub const fn verified_id(&self) -> Option<&ScientificContentId> {
        match self {
            Self::Unverified => None,
            Self::Verified(identity) => Some(identity),
        }
    }
}

/// Immutable scientific and display-label facts for one logical layer.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetLayer {
    key: LogicalLayerKey,
    label: String,
    shape: Shape4D,
    dtype: IntensityDType,
    grid_to_world: GridToWorld,
}

impl DatasetLayer {
    pub fn new(
        key: LogicalLayerKey,
        label: impl AsRef<str>,
        shape: Shape4D,
        dtype: IntensityDType,
        grid_to_world: GridToWorld,
    ) -> Result<Self, DatasetCatalogError> {
        let label = validate_label("layer label", label.as_ref(), MAX_LAYER_LABEL_BYTES)?;
        Ok(Self {
            key,
            label,
            shape,
            dtype,
            grid_to_world,
        })
    }

    pub const fn key(&self) -> LogicalLayerKey {
        self.key
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn shape(&self) -> Shape4D {
        self.shape
    }

    pub const fn dtype(&self) -> IntensityDType {
        self.dtype
    }

    pub const fn grid_to_world(&self) -> GridToWorld {
        self.grid_to_world
    }
}

/// A bounded catalog keyed only by canonical logical-layer keys.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetCatalog {
    label: String,
    scientific_identity: ScientificIdentityStatus,
    layers: BTreeMap<LogicalLayerKey, DatasetLayer>,
}

impl DatasetCatalog {
    pub fn new(
        label: impl AsRef<str>,
        scientific_identity: ScientificIdentityStatus,
        layers: Vec<DatasetLayer>,
    ) -> Result<Self, DatasetCatalogError> {
        let label = validate_label("dataset label", label.as_ref(), MAX_DATASET_LABEL_BYTES)?;
        if layers.is_empty() {
            return Err(DatasetCatalogError::EmptyCatalog);
        }
        if layers.len() > MAX_DATASET_LAYERS {
            return Err(DatasetCatalogError::TooManyLayers {
                actual: layers.len(),
                maximum: MAX_DATASET_LAYERS,
            });
        }

        let mut by_key = BTreeMap::new();
        for layer in layers {
            let key = layer.key();
            if by_key.insert(key, layer).is_some() {
                return Err(DatasetCatalogError::DuplicateLayerKey {
                    ordinal: key.ordinal(),
                });
            }
        }

        Ok(Self {
            label,
            scientific_identity,
            layers: by_key,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn scientific_identity(&self) -> &ScientificIdentityStatus {
        &self.scientific_identity
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn layer(&self, key: LogicalLayerKey) -> Option<&DatasetLayer> {
        self.layers.get(&key)
    }

    /// Iterates in ascending `LogicalLayerKey` order, independent of input
    /// order or duplicate human-readable labels.
    pub fn layers(&self) -> impl ExactSizeIterator<Item = &DatasetLayer> {
        self.layers.values()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DatasetCatalogError {
    #[error("{kind} must not be empty")]
    EmptyLabel { kind: &'static str },
    #[error("{kind} exceeds {maximum} UTF-8 bytes")]
    LabelTooLong { kind: &'static str, maximum: usize },
    #[error("{kind} contains a control character")]
    LabelContainsControl { kind: &'static str },
    #[error("a dataset catalog must contain at least one logical layer")]
    EmptyCatalog,
    #[error("dataset catalog contains {actual} layers, exceeding the limit of {maximum}")]
    TooManyLayers { actual: usize, maximum: usize },
    #[error("logical layer key {ordinal} occurs more than once")]
    DuplicateLayerKey { ordinal: u32 },
}

fn validate_label(
    kind: &'static str,
    value: &str,
    maximum: usize,
) -> Result<String, DatasetCatalogError> {
    if value.trim().is_empty() {
        return Err(DatasetCatalogError::EmptyLabel { kind });
    }
    if value.len() > maximum {
        return Err(DatasetCatalogError::LabelTooLong { kind, maximum });
    }
    if value.chars().any(char::is_control) {
        return Err(DatasetCatalogError::LabelContainsControl { kind });
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_SCIENTIFIC_ID: &str =
        "m4d-sc-v1-sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn layer(key: u32, label: &str) -> DatasetLayer {
        DatasetLayer::new(
            LogicalLayerKey::new(key),
            label,
            Shape4D::new(3, 5, 7, 11).unwrap(),
            IntensityDType::Uint16,
            GridToWorld::scale(0.5, 0.75, 2.0).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn catalog_is_keyed_and_iterated_by_logical_layer_key() {
        let catalog = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Unverified,
            vec![layer(7, "green"), layer(2, "red"), layer(5, "green")],
        )
        .unwrap();

        assert_eq!(catalog.len(), 3);
        assert_eq!(
            catalog.layer(LogicalLayerKey::new(2)).unwrap().label(),
            "red"
        );
        assert_eq!(
            catalog.layers().map(DatasetLayer::key).collect::<Vec<_>>(),
            vec![
                LogicalLayerKey::new(2),
                LogicalLayerKey::new(5),
                LogicalLayerKey::new(7),
            ]
        );
    }

    #[test]
    fn duplicate_human_labels_are_not_identity_but_duplicate_keys_reject() {
        assert!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified,
                vec![layer(0, "channel"), layer(1, "channel")],
            )
            .is_ok()
        );

        assert_eq!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified,
                vec![layer(3, "first"), layer(3, "second")],
            ),
            Err(DatasetCatalogError::DuplicateLayerKey { ordinal: 3 })
        );
    }

    #[test]
    fn identity_status_cannot_confuse_unverified_catalogs_with_verified_content() {
        let unverified = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Unverified,
            vec![layer(0, "channel")],
        )
        .unwrap();
        assert!(!unverified.scientific_identity().is_verified());
        assert_eq!(unverified.scientific_identity().verified_id(), None);

        let identity = ScientificContentId::parse(ZERO_SCIENTIFIC_ID).unwrap();
        let verified = DatasetCatalog::new(
            "experiment",
            ScientificIdentityStatus::Verified(identity),
            vec![layer(0, "channel")],
        )
        .unwrap();
        assert_eq!(
            verified.scientific_identity().verified_id(),
            Some(&identity)
        );
    }

    #[test]
    fn layer_preserves_canonical_scientific_facts() {
        let layer = layer(4, "channel");
        assert_eq!(layer.key(), LogicalLayerKey::new(4));
        assert_eq!(layer.shape().dimensions(), [3, 5, 7, 11]);
        assert_eq!(layer.dtype(), IntensityDType::Uint16);
        assert_eq!(
            layer.grid_to_world().row_major(),
            GridToWorld::scale(0.5, 0.75, 2.0).unwrap().row_major()
        );
    }

    #[test]
    fn catalog_and_labels_are_bounded_before_collection() {
        assert_eq!(
            DatasetCatalog::new(
                "experiment",
                ScientificIdentityStatus::Unverified,
                Vec::new(),
            ),
            Err(DatasetCatalogError::EmptyCatalog)
        );
        assert_eq!(
            DatasetCatalog::new(
                " ",
                ScientificIdentityStatus::Unverified,
                vec![layer(0, "channel")],
            ),
            Err(DatasetCatalogError::EmptyLabel {
                kind: "dataset label"
            })
        );
        assert_eq!(
            DatasetLayer::new(
                LogicalLayerKey::new(0),
                "bad\nlabel",
                Shape4D::new(1, 1, 1, 1).unwrap(),
                IntensityDType::Uint8,
                GridToWorld::identity(),
            ),
            Err(DatasetCatalogError::LabelContainsControl {
                kind: "layer label"
            })
        );

        let oversized = "x".repeat(MAX_DATASET_LABEL_BYTES + 1);
        assert_eq!(
            DatasetCatalog::new(
                oversized,
                ScientificIdentityStatus::Unverified,
                vec![layer(0, "channel")],
            ),
            Err(DatasetCatalogError::LabelTooLong {
                kind: "dataset label",
                maximum: MAX_DATASET_LABEL_BYTES,
            })
        );

        let layers = (0..=MAX_DATASET_LAYERS)
            .map(|key| layer(u32::try_from(key).unwrap(), "channel"))
            .collect();
        assert_eq!(
            DatasetCatalog::new("experiment", ScientificIdentityStatus::Unverified, layers,),
            Err(DatasetCatalogError::TooManyLayers {
                actual: MAX_DATASET_LAYERS + 1,
                maximum: MAX_DATASET_LAYERS,
            })
        );
    }
}
