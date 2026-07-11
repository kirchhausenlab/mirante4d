use std::{
    fs,
    path::{Path, PathBuf},
};

use glam::{DQuat, DVec3};
use mirante4d_analysis::{
    AnalysisOperationRecord, AnalysisPlot, AnalysisTable, SceneArtifactStore,
};
use mirante4d_core::{CameraView, ChannelTransferFunction, IsoLightState, TimeIndex};
use mirante4d_renderer::CrossSectionViewState;
use serde::{Deserialize, Serialize};

use crate::{
    AppState, ChannelDisplayPreset, ChannelRenderState, DvrOpacityTransfer,
    layer_state::active_layer_render_state_from_runtime,
    project_store::{
        AppAnalysisArtifactReference, PROJECT_ARTIFACTS_DIR, PROJECT_AUTOSAVE_DIR,
        PROJECT_PLOTS_DIR, PROJECT_TABLES_DIR, analysis_artifact_reference,
        autosave_project_json_path, dataset_reference_path_for_manifest,
        dataset_reference_path_from_manifest, ensure_project_package_layout,
        native_manifest_fingerprint_blake3, project_json_path, resolve_project_artifact_reference,
        write_json_artifact_atomically, write_project_json_atomically,
    },
    transfer_presets::transfer_from_layer_summary,
    viewer_layout::ViewerLayout,
};

pub(crate) const SESSION_FORMAT: &str = "mirante4d-project-v14";
const ANALYSIS_TABLE_ARTIFACT_FORMAT: &str = "mirante4d-analysis-table-v1";
const ANALYSIS_PLOT_ARTIFACT_FORMAT: &str = "mirante4d-analysis-plot-v1";
const MIN_SESSION_ORIENTATION_LENGTH_SQUARED: f64 = 1.0e-18;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSession {
    pub format: String,
    pub dataset: AppDatasetReference,
    pub active_layer_id: String,
    pub layer_display_states: Vec<AppLayerDisplayState>,
    pub channel_presets: Vec<ChannelDisplayPreset>,
    pub active_timepoint: TimeIndex,
    pub iso_light_state: IsoLightState,
    pub camera: CameraView,
    pub viewer_layout: AppViewerLayoutSession,
    pub scene_artifacts: SceneArtifactStore,
    pub analysis_tables: Vec<AnalysisTable>,
    pub analysis_plots: Vec<AnalysisPlot>,
    pub analysis_operations: Vec<AnalysisOperationRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppRecoverySession {
    pub autosave_path: PathBuf,
    pub session: AppSession,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProjectDirtySnapshot {
    session: AppSession,
}

impl ProjectDirtySnapshot {
    pub(crate) fn from_state(state: &AppState) -> Self {
        Self::from_session_and_state(session_from_state(state), state)
    }

    pub(crate) fn from_session_and_state(session: AppSession, state: &AppState) -> Self {
        let _ = state;
        Self { session }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppSessionManifest {
    pub format: String,
    pub dataset: AppDatasetReference,
    pub active_layer_id: String,
    pub layer_display_states: Vec<AppLayerDisplayState>,
    pub channel_presets: Vec<ChannelDisplayPreset>,
    pub active_timepoint: TimeIndex,
    pub iso_light_state: IsoLightState,
    pub camera: CameraView,
    pub viewer_layout: AppViewerLayoutSession,
    pub scene_artifacts: SceneArtifactStore,
    pub analysis_tables: Vec<AppAnalysisArtifactReference>,
    pub analysis_plots: Vec<AppAnalysisArtifactReference>,
    pub analysis_operations: Vec<AnalysisOperationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDatasetReference {
    pub path: PathBuf,
    pub dataset_id: String,
    pub format: String,
    pub schema_version: u32,
    pub manifest_fingerprint_blake3: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppLayerDisplayState {
    pub layer_id: String,
    pub transfer: ChannelTransferFunction,
    pub dvr_opacity_transfer: DvrOpacityTransfer,
    pub render_state: ChannelRenderState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppViewerLayoutSession {
    pub layout: AppViewerLayoutMode,
    pub cross_section: AppCrossSectionSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppViewerLayoutMode {
    #[serde(rename = "single3d")]
    Single3d,
    FourPanel,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCrossSectionSession {
    pub center_world: [f64; 3],
    pub orientation_xyzw: [f64; 4],
    pub scale_world_per_screen_point: f64,
    pub depth_world: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AnalysisTableArtifactPayload {
    pub(crate) format: String,
    pub(crate) artifact_id: String,
    pub(crate) table: AnalysisTable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AnalysisPlotArtifactPayload {
    pub(crate) format: String,
    pub(crate) artifact_id: String,
    pub(crate) plot: AnalysisPlot,
}

pub fn session_from_state(state: &AppState) -> AppSession {
    AppSession {
        format: SESSION_FORMAT.to_owned(),
        dataset: dataset_reference_from_state(state),
        active_layer_id: state.active_layer_id.clone(),
        layer_display_states: layer_display_states_from_state(state),
        channel_presets: state.channel_presets.clone(),
        active_timepoint: state.active_timepoint,
        iso_light_state: state.iso_light_state,
        camera: state.camera,
        viewer_layout: viewer_layout_session_from_state(state),
        scene_artifacts: state.scene_artifacts.clone(),
        analysis_tables: state.analysis_tables.clone(),
        analysis_plots: state.analysis_plots.clone(),
        analysis_operations: state.analysis_operations.clone(),
    }
}

fn session_manifest_from_session(
    project_path: &Path,
    session: &AppSession,
    analysis_tables: Vec<AppAnalysisArtifactReference>,
    analysis_plots: Vec<AppAnalysisArtifactReference>,
) -> AppSessionManifest {
    let mut dataset = session.dataset.clone();
    dataset.path = dataset_reference_path_for_manifest(project_path, &dataset.path);
    AppSessionManifest {
        format: session.format.clone(),
        dataset,
        active_layer_id: session.active_layer_id.clone(),
        layer_display_states: session.layer_display_states.clone(),
        channel_presets: session.channel_presets.clone(),
        active_timepoint: session.active_timepoint,
        iso_light_state: session
            .iso_light_state
            .validate()
            .expect("in-memory ISO light state is valid before project write"),
        camera: session.camera,
        viewer_layout: session.viewer_layout.clone(),
        scene_artifacts: session.scene_artifacts.clone(),
        analysis_tables,
        analysis_plots,
        analysis_operations: session.analysis_operations.clone(),
    }
}

fn session_from_manifest(
    project_path: &Path,
    manifest: AppSessionManifest,
) -> anyhow::Result<AppSession> {
    session_from_manifest_with_artifact_root(project_path, manifest, PROJECT_ARTIFACTS_DIR)
}

fn session_from_manifest_with_artifact_root(
    project_path: &Path,
    manifest: AppSessionManifest,
    artifact_root: &str,
) -> anyhow::Result<AppSession> {
    if manifest.format != SESSION_FORMAT {
        anyhow::bail!("unsupported session format {:?}", manifest.format);
    }
    let mut dataset = manifest.dataset;
    dataset.path = dataset_reference_path_from_manifest(project_path, &dataset.path);
    Ok(AppSession {
        format: manifest.format,
        dataset,
        active_layer_id: manifest.active_layer_id,
        layer_display_states: manifest.layer_display_states,
        channel_presets: manifest.channel_presets,
        active_timepoint: manifest.active_timepoint,
        iso_light_state: manifest.iso_light_state.validate()?,
        camera: manifest.camera,
        viewer_layout: manifest.viewer_layout,
        scene_artifacts: manifest.scene_artifacts,
        analysis_tables: read_analysis_table_artifacts_with_root(
            project_path,
            artifact_root,
            &manifest.analysis_tables,
        )?,
        analysis_plots: read_analysis_plot_artifacts_with_root(
            project_path,
            artifact_root,
            &manifest.analysis_plots,
        )?,
        analysis_operations: manifest.analysis_operations,
    })
}

fn viewer_layout_session_from_state(state: &AppState) -> AppViewerLayoutSession {
    AppViewerLayoutSession {
        layout: AppViewerLayoutMode::from(state.viewer_layout.layout()),
        cross_section: AppCrossSectionSession::from_view_state(state.viewer_layout.cross_section),
    }
}

pub(crate) fn apply_viewer_layout_session(
    state: &mut AppState,
    session: &AppViewerLayoutSession,
) -> anyhow::Result<()> {
    state.viewer_layout.cross_section = session.cross_section.to_view_state()?;
    match session.layout {
        AppViewerLayoutMode::Single3d => state.viewer_layout.switch_to_single_3d(),
        AppViewerLayoutMode::FourPanel => state.viewer_layout.switch_to_four_panel(),
    }
    state.cross_section_last_interaction_at = None;
    Ok(())
}

impl From<ViewerLayout> for AppViewerLayoutMode {
    fn from(value: ViewerLayout) -> Self {
        match value {
            ViewerLayout::Single3d => Self::Single3d,
            ViewerLayout::FourPanel => Self::FourPanel,
        }
    }
}

impl AppCrossSectionSession {
    fn from_view_state(state: CrossSectionViewState) -> Self {
        Self {
            center_world: state.center_world.to_array(),
            orientation_xyzw: state.orientation.to_array(),
            scale_world_per_screen_point: state.scale_world_per_screen_point,
            depth_world: state.depth_world,
        }
    }

    fn to_view_state(self) -> anyhow::Result<CrossSectionViewState> {
        let center_world = finite_vec3("cross_section.center_world", self.center_world)?;
        let orientation = finite_quat("cross_section.orientation_xyzw", self.orientation_xyzw)?;
        let scale = finite_positive(
            "cross_section.scale_world_per_screen_point",
            self.scale_world_per_screen_point,
        )?;
        let depth = finite_positive("cross_section.depth_world", self.depth_world)?;
        Ok(CrossSectionViewState::new(
            center_world,
            orientation,
            scale,
            depth,
        ))
    }
}

fn finite_vec3(name: &'static str, value: [f64; 3]) -> anyhow::Result<DVec3> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(DVec3::new(value[0], value[1], value[2]))
    } else {
        anyhow::bail!("{name} must contain finite values")
    }
}

fn finite_quat(name: &'static str, value: [f64; 4]) -> anyhow::Result<DQuat> {
    if !value.iter().all(|component| component.is_finite()) {
        anyhow::bail!("{name} must contain finite values");
    }
    let orientation = DQuat::from_xyzw(value[0], value[1], value[2], value[3]);
    if orientation.length_squared() <= MIN_SESSION_ORIENTATION_LENGTH_SQUARED {
        anyhow::bail!("{name} must be a nonzero quaternion");
    }
    Ok(orientation)
}

fn finite_positive(name: &'static str, value: f64) -> anyhow::Result<f64> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        anyhow::bail!("{name} must be finite and positive")
    }
}

fn dataset_reference_from_state(state: &AppState) -> AppDatasetReference {
    let manifest = state.dataset.manifest();
    AppDatasetReference {
        path: state.dataset_path.clone(),
        dataset_id: manifest.dataset.id.clone(),
        format: manifest.format.clone(),
        schema_version: manifest.schema_version,
        manifest_fingerprint_blake3: native_manifest_fingerprint_blake3(manifest),
    }
}

fn layer_display_states_from_state(state: &AppState) -> Vec<AppLayerDisplayState> {
    state
        .layers
        .iter()
        .map(|layer| AppLayerDisplayState {
            layer_id: layer.id.clone(),
            transfer: if layer.id == state.active_layer_id {
                state.active_layer_transfer.clone()
            } else {
                transfer_from_layer_summary(layer)
            },
            dvr_opacity_transfer: if layer.id == state.active_layer_id {
                state.active_dvr_opacity_transfer
            } else {
                layer.dvr_opacity_transfer
            },
            render_state: if layer.id == state.active_layer_id {
                active_layer_render_state_from_runtime(state)
            } else {
                layer.render_state
            },
        })
        .collect()
}

pub(crate) fn validate_opened_dataset_reference(
    state: &AppState,
    expected: &AppDatasetReference,
) -> anyhow::Result<()> {
    let actual = dataset_reference_from_state(state);
    if actual.dataset_id != expected.dataset_id {
        anyhow::bail!(
            "project dataset id {:?} does not match opened dataset id {:?}",
            expected.dataset_id,
            actual.dataset_id
        );
    }
    if actual.format != expected.format {
        anyhow::bail!(
            "project dataset format {:?} does not match opened dataset format {:?}",
            expected.format,
            actual.format
        );
    }
    if actual.schema_version != expected.schema_version {
        anyhow::bail!(
            "project dataset schema version {} does not match opened dataset schema version {}",
            expected.schema_version,
            actual.schema_version
        );
    }
    if actual.manifest_fingerprint_blake3 != expected.manifest_fingerprint_blake3 {
        anyhow::bail!(
            "project dataset manifest fingerprint {} does not match opened dataset fingerprint {}",
            expected.manifest_fingerprint_blake3,
            actual.manifest_fingerprint_blake3
        );
    }
    Ok(())
}

pub fn write_session_file(path: impl AsRef<Path>, session: &AppSession) -> anyhow::Result<()> {
    if session.format != SESSION_FORMAT {
        anyhow::bail!(
            "refusing to write unsupported session format {:?}",
            session.format
        );
    }
    let path = path.as_ref();
    ensure_project_package_layout(path)?;
    let table_artifacts = write_analysis_table_artifacts_with_root(
        path,
        PROJECT_ARTIFACTS_DIR,
        &session.analysis_tables,
    )?;
    let plot_artifacts = write_analysis_plot_artifacts_with_root(
        path,
        PROJECT_ARTIFACTS_DIR,
        &session.analysis_plots,
    )?;
    let manifest = session_manifest_from_session(path, session, table_artifacts, plot_artifacts);
    let encoded = serde_json::to_string_pretty(&manifest)?;
    write_project_json_atomically(path, &encoded)?;
    Ok(())
}

pub fn read_session_file(path: impl AsRef<Path>) -> anyhow::Result<AppSession> {
    let path = path.as_ref();
    if !path.is_dir() {
        anyhow::bail!(
            "Mirante4D project package must be a .m4dproj directory: {}",
            path.display()
        );
    }
    let encoded = fs::read_to_string(project_json_path(path))?;
    parse_project_session_manifest(path, &encoded)
}

pub fn parse_project_session_manifest(
    project_path: impl AsRef<Path>,
    encoded: &str,
) -> anyhow::Result<AppSession> {
    validate_session_format_header(encoded)?;
    let manifest: AppSessionManifest = serde_json::from_str(encoded)?;
    session_from_manifest(project_path.as_ref(), manifest)
}

#[derive(Deserialize)]
struct SessionFormatHeader {
    format: String,
}

fn validate_session_format_header(encoded: &str) -> anyhow::Result<()> {
    let header: SessionFormatHeader = serde_json::from_str(encoded)?;
    if header.format != SESSION_FORMAT {
        anyhow::bail!("unsupported session format {:?}", header.format);
    }
    Ok(())
}

pub fn write_autosave_snapshot(
    project_path: impl AsRef<Path>,
    session: &AppSession,
) -> anyhow::Result<PathBuf> {
    if session.format != SESSION_FORMAT {
        anyhow::bail!(
            "refusing to autosave unsupported session format {:?}",
            session.format
        );
    }
    let project_path = project_path.as_ref();
    ensure_project_package_layout(project_path)?;
    let table_artifacts = write_analysis_table_artifacts_with_root(
        project_path,
        PROJECT_AUTOSAVE_DIR,
        &session.analysis_tables,
    )?;
    let plot_artifacts = write_analysis_plot_artifacts_with_root(
        project_path,
        PROJECT_AUTOSAVE_DIR,
        &session.analysis_plots,
    )?;
    let manifest =
        session_manifest_from_session(project_path, session, table_artifacts, plot_artifacts);
    let encoded = serde_json::to_string_pretty(&manifest)?;
    let autosave_path = autosave_project_json_path(project_path);
    write_json_artifact_atomically(&autosave_path, &encoded)?;
    Ok(autosave_path)
}

pub fn read_autosave_snapshot(
    project_path: impl AsRef<Path>,
) -> anyhow::Result<AppRecoverySession> {
    let project_path = project_path.as_ref();
    if !project_path.is_dir() {
        anyhow::bail!(
            "Mirante4D project package must be a .m4dproj directory: {}",
            project_path.display()
        );
    }
    let autosave_path = autosave_project_json_path(project_path);
    let encoded = fs::read_to_string(&autosave_path)?;
    validate_session_format_header(&encoded)?;
    let manifest: AppSessionManifest = serde_json::from_str(&encoded)?;
    let session =
        session_from_manifest_with_artifact_root(project_path, manifest, PROJECT_AUTOSAVE_DIR)?;
    Ok(AppRecoverySession {
        autosave_path,
        session,
    })
}

fn write_analysis_table_artifacts_with_root(
    project_path: &Path,
    root_dir: &str,
    tables: &[AnalysisTable],
) -> anyhow::Result<Vec<AppAnalysisArtifactReference>> {
    tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            let reference = analysis_artifact_reference(
                root_dir,
                PROJECT_TABLES_DIR,
                "m4dtable.json",
                index,
                &table.id,
            );
            let payload = AnalysisTableArtifactPayload {
                format: ANALYSIS_TABLE_ARTIFACT_FORMAT.to_owned(),
                artifact_id: table.id.clone(),
                table: table.clone(),
            };
            let encoded = serde_json::to_string_pretty(&payload)?;
            write_json_artifact_atomically(&project_path.join(&reference.artifact_path), &encoded)?;
            Ok(reference)
        })
        .collect()
}

fn write_analysis_plot_artifacts_with_root(
    project_path: &Path,
    root_dir: &str,
    plots: &[AnalysisPlot],
) -> anyhow::Result<Vec<AppAnalysisArtifactReference>> {
    plots
        .iter()
        .enumerate()
        .map(|(index, plot)| {
            let reference = analysis_artifact_reference(
                root_dir,
                PROJECT_PLOTS_DIR,
                "m4dplot.json",
                index,
                &plot.id,
            );
            let payload = AnalysisPlotArtifactPayload {
                format: ANALYSIS_PLOT_ARTIFACT_FORMAT.to_owned(),
                artifact_id: plot.id.clone(),
                plot: plot.clone(),
            };
            let encoded = serde_json::to_string_pretty(&payload)?;
            write_json_artifact_atomically(&project_path.join(&reference.artifact_path), &encoded)?;
            Ok(reference)
        })
        .collect()
}

fn read_analysis_table_artifacts_with_root(
    project_path: &Path,
    root_dir: &str,
    references: &[AppAnalysisArtifactReference],
) -> anyhow::Result<Vec<AnalysisTable>> {
    references
        .iter()
        .map(|reference| {
            let path = resolve_project_artifact_reference(
                project_path,
                reference,
                root_dir,
                PROJECT_TABLES_DIR,
            )?;
            let encoded = fs::read_to_string(&path)?;
            let payload: AnalysisTableArtifactPayload = serde_json::from_str(&encoded)?;
            if payload.format != ANALYSIS_TABLE_ARTIFACT_FORMAT {
                anyhow::bail!(
                    "unsupported analysis table artifact format {:?}",
                    payload.format
                );
            }
            if payload.artifact_id != reference.artifact_id {
                anyhow::bail!(
                    "analysis table artifact id {:?} does not match project reference {:?}",
                    payload.artifact_id,
                    reference.artifact_id
                );
            }
            if payload.table.id != reference.artifact_id {
                anyhow::bail!(
                    "analysis table payload id {:?} does not match project reference {:?}",
                    payload.table.id,
                    reference.artifact_id
                );
            }
            Ok(payload.table)
        })
        .collect()
}

fn read_analysis_plot_artifacts_with_root(
    project_path: &Path,
    root_dir: &str,
    references: &[AppAnalysisArtifactReference],
) -> anyhow::Result<Vec<AnalysisPlot>> {
    references
        .iter()
        .map(|reference| {
            let path = resolve_project_artifact_reference(
                project_path,
                reference,
                root_dir,
                PROJECT_PLOTS_DIR,
            )?;
            let encoded = fs::read_to_string(&path)?;
            let payload: AnalysisPlotArtifactPayload = serde_json::from_str(&encoded)?;
            if payload.format != ANALYSIS_PLOT_ARTIFACT_FORMAT {
                anyhow::bail!(
                    "unsupported analysis plot artifact format {:?}",
                    payload.format
                );
            }
            if payload.artifact_id != reference.artifact_id {
                anyhow::bail!(
                    "analysis plot artifact id {:?} does not match project reference {:?}",
                    payload.artifact_id,
                    reference.artifact_id
                );
            }
            if payload.plot.id != reference.artifact_id {
                anyhow::bail!(
                    "analysis plot payload id {:?} does not match project reference {:?}",
                    payload.plot.id,
                    reference.artifact_id
                );
            }
            Ok(payload.plot)
        })
        .collect()
}
