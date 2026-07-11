//! Validated, persistence-neutral durable project values.
//!
//! This preparatory crate owns no serialization, I/O, runtime, UI, renderer,
//! GPU, or task execution behavior.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use mirante4d_domain::{
    CameraView, CrossSectionView, IsoLightState, LayerTransfer, LogicalLayerKey, RenderState,
    TimeIndex, ViewerLayout,
};
use mirante4d_identity::{
    ArtifactContentId, DerivationRecordId, PackageId, RawObjectDescriptor, RecipeId, ReleaseId,
    ScientificContentId,
};
use thiserror::Error;

pub const MAX_DATASET_LOCATOR_HINT_BYTES: usize = 4096;
pub const MAX_CHANNEL_PRESET_ID_BYTES: usize = 64;
pub const MAX_PROJECT_LABEL_BYTES: usize = 256;
pub const MAX_VIEW_LAYERS: usize = 4_096;
pub const MAX_CHANNEL_PRESETS: usize = 1_024;
pub const MAX_CHANNEL_PRESET_ENTRIES: usize = MAX_VIEW_LAYERS;
pub const MAX_TOTAL_CHANNEL_PRESET_ENTRIES: usize = 16_384;
pub const MAX_ARTIFACTS: usize = 16_384;
pub const MAX_ARTIFACT_SOURCE_LAYERS: usize = MAX_VIEW_LAYERS;
pub const MAX_TOTAL_ARTIFACT_SOURCE_LAYER_REFERENCES: usize = 65_536;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProjectModelError {
    #[error("{kind} must be a canonical lowercase hyphenated UUID")]
    InvalidUuid { kind: &'static str },
    #[error(
        "project revision belongs to project {actual_project_id}, expected {expected_project_id}"
    )]
    RevisionProjectMismatch {
        expected_project_id: ProjectId,
        actual_project_id: ProjectId,
    },
    #[error(
        "project revision high-water belongs to project {actual_project_id}, expected {expected_project_id}"
    )]
    RevisionHighWaterProjectMismatch {
        expected_project_id: ProjectId,
        actual_project_id: ProjectId,
    },
    #[error(
        "project revision sequence {revision_sequence} exceeds persisted high-water sequence {high_water_sequence}"
    )]
    RevisionBeyondHighWater {
        revision_sequence: u64,
        high_water_sequence: u64,
    },
    #[error("project revision sequence overflowed")]
    RevisionOverflow,
    #[error("dataset locator hint must not be empty")]
    EmptyDatasetLocatorHint,
    #[error("dataset locator hint exceeds {maximum} UTF-8 bytes")]
    DatasetLocatorHintTooLong { maximum: usize },
    #[error("dataset locator hint contains a control character")]
    DatasetLocatorHintContainsControl,
    #[error("{kind} must not be empty")]
    EmptyLabel { kind: &'static str },
    #[error("{kind} exceeds {maximum} UTF-8 bytes")]
    LabelTooLong { kind: &'static str, maximum: usize },
    #[error("{kind} contains a control character")]
    LabelContainsControl { kind: &'static str },
    #[error("channel preset id must contain only ASCII letters, digits, '-' or '_'")]
    InvalidChannelPresetId,
    #[error("channel preset id exceeds {maximum} bytes")]
    ChannelPresetIdTooLong { maximum: usize },
    #[error("{collection} contains {actual} items, exceeding the limit of {maximum}")]
    CollectionLimitExceeded {
        collection: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("a view must contain at least one layer")]
    EmptyView,
    #[error("logical layer {ordinal} occurs more than once")]
    DuplicateLayer { ordinal: u32 },
    #[error("active logical layer {ordinal} is not present in the view")]
    ActiveLayerMissing { ordinal: u32 },
    #[error("layer order does not contain exactly the existing logical layers")]
    InvalidLayerOrder,
    #[error("channel preset {preset_id} occurs more than once")]
    DuplicateChannelPreset { preset_id: String },
    #[error("channel preset {preset_id} contains logical layer {ordinal} more than once")]
    DuplicatePresetLayer { preset_id: String, ordinal: u32 },
    #[error("channel preset {preset_id} does not cover exactly the view's logical layers")]
    InvalidPresetLayerClosure { preset_id: String },
    #[error("artifact handle {handle_id} occurs more than once")]
    DuplicateArtifactHandle { handle_id: ArtifactHandleId },
    #[error("artifact {handle_id} references logical layer {ordinal}, which is absent")]
    ArtifactLayerMissing {
        handle_id: ArtifactHandleId,
        ordinal: u32,
    },
    #[error("artifact {handle_id} references logical layer {ordinal} more than once")]
    DuplicateArtifactLayer {
        handle_id: ArtifactHandleId,
        ordinal: u32,
    },
    #[error("regenerable artifact {handle_id} requires both recipe and derivation identities")]
    RegenerableArtifactMissingProvenance { handle_id: ArtifactHandleId },
    #[error(
        "artifact schema {schema} requires media type {expected:?}, but the object declares {actual:?}"
    )]
    ArtifactMediaTypeMismatch {
        schema: ArtifactSchema,
        expected: &'static str,
        actual: String,
    },
    #[error(
        "artifact schema {schema} requires object role {expected:?}, but the object declares {actual:?}"
    )]
    ArtifactObjectRoleMismatch {
        schema: ArtifactSchema,
        expected: &'static str,
        actual: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectId([u8; 16]);

impl ProjectId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, ProjectModelError> {
        parse_uuid("project id", value).map(Self)
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_uuid(self.0, formatter)
    }
}

impl FromStr for ProjectId {
    type Err = ProjectModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectRevisionId {
    project_id: ProjectId,
    sequence: u64,
}

impl ProjectRevisionId {
    pub const fn initial(project_id: ProjectId) -> Self {
        Self {
            project_id,
            sequence: 0,
        }
    }

    pub const fn new(project_id: ProjectId, sequence: u64) -> Self {
        Self {
            project_id,
            sequence,
        }
    }

    pub const fn project_id(self) -> ProjectId {
        self.project_id
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

/// Project-bound revision allocation high-water.
///
/// The application owns one live value per project. Allocating after an older
/// current revision (for example, after undo) advances this high-water rather
/// than reusing the older revision's successor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectRevisionHighWater {
    project_id: ProjectId,
    sequence: u64,
}

impl ProjectRevisionHighWater {
    pub const fn initial(project_id: ProjectId) -> Self {
        Self {
            project_id,
            sequence: 0,
        }
    }

    /// Restores a validated persisted high-water value.
    pub const fn new(project_id: ProjectId, sequence: u64) -> Self {
        Self {
            project_id,
            sequence,
        }
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn allocate_after(
        &mut self,
        current_revision: ProjectRevisionId,
    ) -> Result<ProjectRevisionId, ProjectModelError> {
        validate_revision_project(current_revision, self.project_id)?;
        validate_revision_within_high_water(current_revision, self)?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ProjectModelError::RevisionOverflow)?;
        self.sequence = sequence;
        Ok(ProjectRevisionId::new(self.project_id, sequence))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactHandleId([u8; 16]);

impl ArtifactHandleId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(&self) -> [u8; 16] {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, ProjectModelError> {
        parse_uuid("artifact handle id", value).map(Self)
    }
}

impl fmt::Display for ArtifactHandleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_uuid(self.0, formatter)
    }
}

impl FromStr for ArtifactHandleId {
    type Err = ProjectModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetLocatorHint(String);

impl DatasetLocatorHint {
    /// Creates a bounded reopening hint. This value is never dataset identity.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ProjectModelError> {
        let value = value.as_ref();
        if value.len() > MAX_DATASET_LOCATOR_HINT_BYTES {
            return Err(ProjectModelError::DatasetLocatorHintTooLong {
                maximum: MAX_DATASET_LOCATOR_HINT_BYTES,
            });
        }
        if value.trim().is_empty() {
            return Err(ProjectModelError::EmptyDatasetLocatorHint);
        }
        if value.chars().any(char::is_control) {
            return Err(ProjectModelError::DatasetLocatorHintContainsControl);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetReference {
    scientific_content_id: ScientificContentId,
    package_id: Option<PackageId>,
    release_id: Option<ReleaseId>,
    locator_hint: Option<DatasetLocatorHint>,
}

impl DatasetReference {
    pub fn new(
        scientific_content_id: ScientificContentId,
        package_id: Option<PackageId>,
        release_id: Option<ReleaseId>,
        locator_hint: Option<DatasetLocatorHint>,
    ) -> Self {
        Self {
            scientific_content_id,
            package_id,
            release_id,
            locator_hint,
        }
    }

    pub fn scientific_content_id(&self) -> &ScientificContentId {
        &self.scientific_content_id
    }

    pub fn package_id(&self) -> Option<&PackageId> {
        self.package_id.as_ref()
    }

    pub fn release_id(&self) -> Option<&ReleaseId> {
        self.release_id.as_ref()
    }

    pub fn locator_hint(&self) -> Option<&DatasetLocatorHint> {
        self.locator_hint.as_ref()
    }

    /// Compares the sole scientific-identity fact, deliberately ignoring the
    /// package/release pins and locator hint.
    pub fn has_same_scientific_content(&self, other: &Self) -> bool {
        self.scientific_content_id == other.scientific_content_id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayerViewState {
    layer_key: LogicalLayerKey,
    visible: bool,
    transfer: LayerTransfer,
    render_state: RenderState,
}

impl LayerViewState {
    pub fn new(
        layer_key: LogicalLayerKey,
        visible: bool,
        transfer: LayerTransfer,
        render_state: RenderState,
    ) -> Self {
        Self {
            layer_key,
            visible,
            transfer,
            render_state,
        }
    }

    pub const fn layer_key(&self) -> LogicalLayerKey {
        self.layer_key
    }

    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub fn transfer(&self) -> &LayerTransfer {
        &self.transfer
    }

    pub fn render_state(&self) -> &RenderState {
        &self.render_state
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelPresetId(String);

impl ChannelPresetId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ProjectModelError> {
        let value = value.as_ref();
        if value.len() > MAX_CHANNEL_PRESET_ID_BYTES {
            return Err(ProjectModelError::ChannelPresetIdTooLong {
                maximum: MAX_CHANNEL_PRESET_ID_BYTES,
            });
        }
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ProjectModelError::InvalidChannelPresetId);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChannelPresetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelPresetEntry {
    layer_key: LogicalLayerKey,
    visible: bool,
    transfer: LayerTransfer,
    render_state: RenderState,
}

impl ChannelPresetEntry {
    pub fn new(
        layer_key: LogicalLayerKey,
        visible: bool,
        transfer: LayerTransfer,
        render_state: RenderState,
    ) -> Self {
        Self {
            layer_key,
            visible,
            transfer,
            render_state,
        }
    }

    pub const fn layer_key(&self) -> LogicalLayerKey {
        self.layer_key
    }

    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub fn transfer(&self) -> &LayerTransfer {
        &self.transfer
    }

    pub fn render_state(&self) -> &RenderState {
        &self.render_state
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelPreset {
    id: ChannelPresetId,
    label: String,
    entries: Vec<ChannelPresetEntry>,
}

impl ChannelPreset {
    pub fn new(
        id: ChannelPresetId,
        label: impl AsRef<str>,
        entries: Vec<ChannelPresetEntry>,
    ) -> Result<Self, ProjectModelError> {
        validate_collection_limit(
            "channel preset entries",
            entries.len(),
            MAX_CHANNEL_PRESET_ENTRIES,
        )?;
        let label = validate_label("channel preset label", label.as_ref())?;
        let entries = canonicalize_preset_entries(&id, entries)?;
        Ok(Self { id, label, entries })
    }

    pub fn id(&self) -> &ChannelPresetId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn entries(&self) -> &[ChannelPresetEntry] {
        &self.entries
    }

    pub fn entry(&self, layer_key: LogicalLayerKey) -> Option<&ChannelPresetEntry> {
        self.entries
            .binary_search_by_key(&layer_key, ChannelPresetEntry::layer_key)
            .ok()
            .map(|index| &self.entries[index])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    layers: Vec<LayerViewState>,
    active_layer: LogicalLayerKey,
    timepoint: TimeIndex,
    camera: CameraView,
    layout: ViewerLayout,
    cross_section: CrossSectionView,
    iso_light: IsoLightState,
}

impl ViewState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        layers: Vec<LayerViewState>,
        active_layer: LogicalLayerKey,
        timepoint: TimeIndex,
        camera: CameraView,
        layout: ViewerLayout,
        cross_section: CrossSectionView,
        iso_light: IsoLightState,
    ) -> Result<Self, ProjectModelError> {
        validate_collection_limit("view layers", layers.len(), MAX_VIEW_LAYERS)?;
        validate_layers(&layers, active_layer)?;
        Ok(Self {
            layers,
            active_layer,
            timepoint,
            camera,
            layout,
            cross_section,
            iso_light,
        })
    }

    pub fn layers(&self) -> &[LayerViewState] {
        &self.layers
    }

    pub const fn active_layer(&self) -> LogicalLayerKey {
        self.active_layer
    }

    pub const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub fn camera(&self) -> &CameraView {
        &self.camera
    }

    pub const fn layout(&self) -> ViewerLayout {
        self.layout
    }

    pub fn cross_section(&self) -> &CrossSectionView {
        &self.cross_section
    }

    pub fn iso_light(&self) -> &IsoLightState {
        &self.iso_light
    }

    pub fn layer(&self, key: LogicalLayerKey) -> Option<&LayerViewState> {
        self.layers.iter().find(|layer| layer.layer_key == key)
    }

    pub fn with_layer_order(
        &self,
        layer_order: Vec<LogicalLayerKey>,
    ) -> Result<Self, ProjectModelError> {
        validate_collection_limit("view layer order", layer_order.len(), MAX_VIEW_LAYERS)?;
        let layers_by_key = self
            .layers
            .iter()
            .map(|layer| (layer.layer_key(), layer))
            .collect::<BTreeMap<_, _>>();
        let requested_keys = layer_order.iter().copied().collect::<BTreeSet<_>>();
        if layer_order.len() != self.layers.len()
            || requested_keys.len() != layer_order.len()
            || requested_keys != layers_by_key.keys().copied().collect()
        {
            return Err(ProjectModelError::InvalidLayerOrder);
        }
        let layers = layer_order
            .into_iter()
            .map(|key| (*layers_by_key.get(&key).expect("layer order was validated")).clone())
            .collect();
        Self::new(
            layers,
            self.active_layer,
            self.timepoint,
            self.camera,
            self.layout,
            self.cross_section,
            self.iso_light,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtifactSchema {
    RoiV1,
    TrackV1,
    AnnotationV1,
    MeasurementV1,
    AnalysisTableV1,
    AnalysisPlotV1,
}

impl ArtifactSchema {
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::RoiV1 => "application/vnd.mirante4d.roi-v1+json",
            Self::TrackV1 => "application/vnd.mirante4d.track-v1+json",
            Self::AnnotationV1 => "application/vnd.mirante4d.annotation-v1+json",
            Self::MeasurementV1 => "application/vnd.mirante4d.measurement-v1+json",
            Self::AnalysisTableV1 => "application/vnd.mirante4d.analysis-table-v1+json",
            Self::AnalysisPlotV1 => "application/vnd.mirante4d.analysis-plot-v1+json",
        }
    }

    pub const fn object_role(self) -> &'static str {
        match self {
            Self::RoiV1 => "artifact.roi.v1",
            Self::TrackV1 => "artifact.track.v1",
            Self::AnnotationV1 => "artifact.annotation.v1",
            Self::MeasurementV1 => "artifact.measurement.v1",
            Self::AnalysisTableV1 => "artifact.analysis-table.v1",
            Self::AnalysisPlotV1 => "artifact.analysis-plot.v1",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoiV1 => "roi.v1",
            Self::TrackV1 => "track.v1",
            Self::AnnotationV1 => "annotation.v1",
            Self::MeasurementV1 => "measurement.v1",
            Self::AnalysisTableV1 => "analysis-table.v1",
            Self::AnalysisPlotV1 => "analysis-plot.v1",
        }
    }
}

impl fmt::Display for ArtifactSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCompleteness {
    Partial,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactRecoverability {
    Regenerable,
    NonRegenerable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReference {
    handle_id: ArtifactHandleId,
    schema: ArtifactSchema,
    content_id: ArtifactContentId,
    object: RawObjectDescriptor,
    derivation_id: Option<DerivationRecordId>,
    recipe_id: Option<RecipeId>,
    source_layers: Vec<LogicalLayerKey>,
    label: String,
    visible: bool,
    completeness: ArtifactCompleteness,
    recoverability: ArtifactRecoverability,
}

impl ArtifactReference {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        handle_id: ArtifactHandleId,
        schema: ArtifactSchema,
        content_id: ArtifactContentId,
        object: RawObjectDescriptor,
        derivation_id: Option<DerivationRecordId>,
        recipe_id: Option<RecipeId>,
        source_layers: Vec<LogicalLayerKey>,
        label: impl AsRef<str>,
        visible: bool,
        completeness: ArtifactCompleteness,
        recoverability: ArtifactRecoverability,
    ) -> Result<Self, ProjectModelError> {
        validate_collection_limit(
            "artifact source layers",
            source_layers.len(),
            MAX_ARTIFACT_SOURCE_LAYERS,
        )?;
        validate_artifact_descriptor(schema, &object)?;
        let label = validate_label("artifact label", label.as_ref())?;
        let source_layers = canonicalize_artifact_source_layers(&handle_id, source_layers)?;
        if recoverability == ArtifactRecoverability::Regenerable
            && (derivation_id.is_none() || recipe_id.is_none())
        {
            return Err(ProjectModelError::RegenerableArtifactMissingProvenance { handle_id });
        }
        Ok(Self {
            handle_id,
            schema,
            content_id,
            object,
            derivation_id,
            recipe_id,
            source_layers,
            label,
            visible,
            completeness,
            recoverability,
        })
    }

    pub fn handle_id(&self) -> &ArtifactHandleId {
        &self.handle_id
    }

    pub const fn schema(&self) -> ArtifactSchema {
        self.schema
    }

    pub fn content_id(&self) -> &ArtifactContentId {
        &self.content_id
    }

    pub fn object(&self) -> &RawObjectDescriptor {
        &self.object
    }

    pub fn derivation_id(&self) -> Option<&DerivationRecordId> {
        self.derivation_id.as_ref()
    }

    pub fn recipe_id(&self) -> Option<&RecipeId> {
        self.recipe_id.as_ref()
    }

    pub fn source_layers(&self) -> &[LogicalLayerKey] {
        &self.source_layers
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub const fn completeness(&self) -> ArtifactCompleteness {
        self.completeness
    }

    pub const fn recoverability(&self) -> ArtifactRecoverability {
        self.recoverability
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectState {
    project_id: ProjectId,
    dataset: DatasetReference,
    view: ViewState,
    channel_presets: Vec<ChannelPreset>,
    artifacts: Vec<ArtifactReference>,
}

impl ProjectState {
    pub fn new(
        project_id: ProjectId,
        dataset: DatasetReference,
        view: ViewState,
        channel_presets: Vec<ChannelPreset>,
        artifacts: Vec<ArtifactReference>,
    ) -> Result<Self, ProjectModelError> {
        validate_collection_limit(
            "channel presets",
            channel_presets.len(),
            MAX_CHANNEL_PRESETS,
        )?;
        validate_collection_limit("artifacts", artifacts.len(), MAX_ARTIFACTS)?;
        validate_collection_limit(
            "total channel preset entries",
            channel_presets
                .iter()
                .map(|preset| preset.entries.len())
                .sum(),
            MAX_TOTAL_CHANNEL_PRESET_ENTRIES,
        )?;
        validate_collection_limit(
            "total artifact source layer references",
            artifacts
                .iter()
                .map(|artifact| artifact.source_layers.len())
                .sum(),
            MAX_TOTAL_ARTIFACT_SOURCE_LAYER_REFERENCES,
        )?;
        validate_project_closure(&view, &channel_presets, &artifacts)?;
        Ok(Self {
            project_id,
            dataset,
            view,
            channel_presets,
            artifacts,
        })
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub fn dataset(&self) -> &DatasetReference {
        &self.dataset
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    pub fn channel_presets(&self) -> &[ChannelPreset] {
        &self.channel_presets
    }

    pub fn channel_preset(&self, id: &ChannelPresetId) -> Option<&ChannelPreset> {
        self.channel_presets.iter().find(|preset| preset.id == *id)
    }

    pub fn artifacts(&self) -> &[ArtifactReference] {
        &self.artifacts
    }

    pub fn artifact(&self, id: &ArtifactHandleId) -> Option<&ArtifactReference> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.handle_id == *id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectGenerationProjection {
    revision: ProjectRevisionId,
    revision_high_water: ProjectRevisionHighWater,
    state: ProjectState,
}

impl ProjectGenerationProjection {
    pub fn new(
        revision: ProjectRevisionId,
        revision_high_water: ProjectRevisionHighWater,
        state: ProjectState,
    ) -> Result<Self, ProjectModelError> {
        validate_revision_project(revision, state.project_id)?;
        if revision_high_water.project_id != state.project_id {
            return Err(ProjectModelError::RevisionHighWaterProjectMismatch {
                expected_project_id: state.project_id,
                actual_project_id: revision_high_water.project_id,
            });
        }
        validate_revision_within_high_water(revision, &revision_high_water)?;
        Ok(Self {
            revision,
            revision_high_water,
            state,
        })
    }

    pub const fn revision(&self) -> ProjectRevisionId {
        self.revision
    }

    pub const fn revision_high_water(&self) -> &ProjectRevisionHighWater {
        &self.revision_high_water
    }

    pub fn state(&self) -> &ProjectState {
        &self.state
    }

    pub fn into_parts(self) -> (ProjectRevisionId, ProjectRevisionHighWater, ProjectState) {
        (self.revision, self.revision_high_water, self.state)
    }
}

fn validate_layers(
    layers: &[LayerViewState],
    active_layer: LogicalLayerKey,
) -> Result<(), ProjectModelError> {
    if layers.is_empty() {
        return Err(ProjectModelError::EmptyView);
    }
    let mut keys = BTreeSet::new();
    for key in layers.iter().map(LayerViewState::layer_key) {
        if !keys.insert(key) {
            return Err(ProjectModelError::DuplicateLayer {
                ordinal: key.ordinal(),
            });
        }
    }
    if !keys.contains(&active_layer) {
        return Err(ProjectModelError::ActiveLayerMissing {
            ordinal: active_layer.ordinal(),
        });
    }
    Ok(())
}

fn canonicalize_preset_entries(
    id: &ChannelPresetId,
    entries: Vec<ChannelPresetEntry>,
) -> Result<Vec<ChannelPresetEntry>, ProjectModelError> {
    let mut entries_by_key = BTreeMap::new();
    for entry in entries {
        let key = entry.layer_key();
        if entries_by_key.insert(key, entry).is_some() {
            return Err(ProjectModelError::DuplicatePresetLayer {
                preset_id: id.as_str().to_owned(),
                ordinal: key.ordinal(),
            });
        }
    }
    Ok(entries_by_key.into_values().collect())
}

fn validate_project_closure(
    view: &ViewState,
    presets: &[ChannelPreset],
    artifacts: &[ArtifactReference],
) -> Result<(), ProjectModelError> {
    let layer_keys = view
        .layers
        .iter()
        .map(LayerViewState::layer_key)
        .collect::<BTreeSet<_>>();
    let mut preset_ids = BTreeSet::new();
    for preset in presets {
        if !preset_ids.insert(&preset.id) {
            return Err(ProjectModelError::DuplicateChannelPreset {
                preset_id: preset.id.as_str().to_owned(),
            });
        }
        if preset.entries.len() != layer_keys.len()
            || preset
                .entries
                .iter()
                .any(|entry| !layer_keys.contains(&entry.layer_key()))
        {
            return Err(ProjectModelError::InvalidPresetLayerClosure {
                preset_id: preset.id.as_str().to_owned(),
            });
        }
    }
    let mut artifact_handles = BTreeSet::new();
    for artifact in artifacts {
        if !artifact_handles.insert(&artifact.handle_id) {
            return Err(ProjectModelError::DuplicateArtifactHandle {
                handle_id: artifact.handle_id.clone(),
            });
        }
        for key in &artifact.source_layers {
            if !layer_keys.contains(key) {
                return Err(ProjectModelError::ArtifactLayerMissing {
                    handle_id: artifact.handle_id.clone(),
                    ordinal: key.ordinal(),
                });
            }
        }
    }
    Ok(())
}

fn canonicalize_artifact_source_layers(
    handle_id: &ArtifactHandleId,
    source_layers: Vec<LogicalLayerKey>,
) -> Result<Vec<LogicalLayerKey>, ProjectModelError> {
    let mut canonical = BTreeSet::new();
    for key in source_layers {
        if !canonical.insert(key) {
            return Err(ProjectModelError::DuplicateArtifactLayer {
                handle_id: handle_id.clone(),
                ordinal: key.ordinal(),
            });
        }
    }
    Ok(canonical.into_iter().collect())
}

fn validate_artifact_descriptor(
    schema: ArtifactSchema,
    object: &RawObjectDescriptor,
) -> Result<(), ProjectModelError> {
    let expected_media_type = schema.media_type();
    if object.media_type().as_str() != expected_media_type {
        return Err(ProjectModelError::ArtifactMediaTypeMismatch {
            schema,
            expected: expected_media_type,
            actual: object.media_type().as_str().to_owned(),
        });
    }
    let expected_object_role = schema.object_role();
    if object.role().as_str() != expected_object_role {
        return Err(ProjectModelError::ArtifactObjectRoleMismatch {
            schema,
            expected: expected_object_role,
            actual: object.role().as_str().to_owned(),
        });
    }
    Ok(())
}

fn validate_collection_limit(
    collection: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), ProjectModelError> {
    if actual > maximum {
        Err(ProjectModelError::CollectionLimitExceeded {
            collection,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn validate_revision_project(
    revision: ProjectRevisionId,
    expected_project_id: ProjectId,
) -> Result<(), ProjectModelError> {
    if revision.project_id == expected_project_id {
        Ok(())
    } else {
        Err(ProjectModelError::RevisionProjectMismatch {
            expected_project_id,
            actual_project_id: revision.project_id,
        })
    }
}

fn validate_revision_within_high_water(
    revision: ProjectRevisionId,
    high_water: &ProjectRevisionHighWater,
) -> Result<(), ProjectModelError> {
    validate_revision_project(revision, high_water.project_id)?;
    if revision.sequence > high_water.sequence {
        Err(ProjectModelError::RevisionBeyondHighWater {
            revision_sequence: revision.sequence,
            high_water_sequence: high_water.sequence,
        })
    } else {
        Ok(())
    }
}

fn validate_label(kind: &'static str, value: &str) -> Result<String, ProjectModelError> {
    if value.len() > MAX_PROJECT_LABEL_BYTES {
        return Err(ProjectModelError::LabelTooLong {
            kind,
            maximum: MAX_PROJECT_LABEL_BYTES,
        });
    }
    if value.trim().is_empty() {
        return Err(ProjectModelError::EmptyLabel { kind });
    }
    if value.chars().any(char::is_control) {
        return Err(ProjectModelError::LabelContainsControl { kind });
    }
    Ok(value.to_owned())
}

fn parse_uuid(kind: &'static str, value: &str) -> Result<[u8; 16], ProjectModelError> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        });
    if !valid_shape {
        return Err(ProjectModelError::InvalidUuid { kind });
    }
    let mut result = [0_u8; 16];
    let mut output_index = 0;
    let mut high_nibble = None;
    for byte in bytes.iter().copied().filter(|byte| *byte != b'-') {
        let nibble = hex_nibble(byte).expect("UUID shape validated lowercase hexadecimal");
        if let Some(high) = high_nibble.take() {
            result[output_index] = (high << 4) | nibble;
            output_index += 1;
        } else {
            high_nibble = Some(nibble);
        }
    }
    Ok(result)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn format_uuid(bytes: [u8; 16], formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            formatter.write_str("-")?;
        }
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
