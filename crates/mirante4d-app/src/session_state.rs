use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use mirante4d_renderer::gpu::GpuRenderer;

use crate::{
    AppPreferences, AppState,
    analysis_workspace::normalize_analysis_selection,
    dataset_opening::open_dataset_with_preferences_and_render_first_frame,
    layer_state::{
        activate_layer_timepoint_state_only, set_layer_dvr_opacity_transfer_state,
        set_layer_render_state, set_layer_transfer_state,
    },
    project_session::{
        AppLayerDisplayState, AppSession, SESSION_FORMAT, apply_viewer_layout_session,
        session_from_state, validate_opened_dataset_reference, write_autosave_snapshot,
        write_session_file,
    },
    project_store::ensure_project_package_layout,
    render_state::rerender_state_with_backend,
    transfer_presets::validate_channel_presets_for_state,
};

fn apply_session_layer_display_states(
    state: &mut AppState,
    display_states: &[AppLayerDisplayState],
) -> anyhow::Result<()> {
    if display_states.len() != state.layers.len() {
        anyhow::bail!(
            "project layer display state count {} does not match dataset layer count {}",
            display_states.len(),
            state.layers.len()
        );
    }
    let mut seen = HashSet::new();
    for display_state in display_states {
        if !seen.insert(display_state.layer_id.clone()) {
            anyhow::bail!(
                "project layer display state has duplicate layer id {:?}",
                display_state.layer_id
            );
        }
        let layer_index = state
            .layers
            .iter()
            .position(|layer| layer.id == display_state.layer_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "project layer display state references missing layer {:?}",
                    display_state.layer_id
                )
            })?;
        set_layer_transfer_state(state, layer_index, display_state.transfer.clone())?;
        set_layer_dvr_opacity_transfer_state(
            state,
            layer_index,
            display_state.dvr_opacity_transfer,
        )?;
        set_layer_render_state(state, layer_index, display_state.render_state)?;
    }
    Ok(())
}

pub fn write_session_file_for_state(
    path: impl AsRef<Path>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let session = session_from_state(state);
    write_session_file(path, &session)
}

pub fn write_autosave_snapshot_for_state(
    project_path: impl AsRef<Path>,
    state: &AppState,
) -> anyhow::Result<PathBuf> {
    let project_path = project_path.as_ref();
    ensure_project_package_layout(project_path)?;
    let session = session_from_state(state);
    write_autosave_snapshot(project_path, &session)
}

#[cfg(test)]
pub(crate) fn open_state_from_session(
    session: &AppSession,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<AppState> {
    open_state_from_session_with_preferences(session, gpu_renderer, &AppPreferences::default())
}

#[cfg(test)]
pub(crate) fn open_state_from_recovery_session(
    recovery: &crate::project_session::AppRecoverySession,
    gpu_renderer: Option<&GpuRenderer>,
) -> anyhow::Result<AppState> {
    let mut state = open_state_from_session(&recovery.session, gpu_renderer)?;
    state.last_workflow_message = Some(format!(
        "Opened autosave recovery {}",
        recovery.autosave_path.display()
    ));
    Ok(state)
}

pub(crate) fn open_state_from_session_with_preferences(
    session: &AppSession,
    gpu_renderer: Option<&GpuRenderer>,
    preferences: &AppPreferences,
) -> anyhow::Result<AppState> {
    if session.format != SESSION_FORMAT {
        anyhow::bail!("unsupported session format {:?}", session.format);
    }
    let mut state =
        open_dataset_with_preferences_and_render_first_frame(&session.dataset.path, preferences)?;
    validate_opened_dataset_reference(&state, &session.dataset)?;
    apply_session_layer_display_states(&mut state, &session.layer_display_states)?;
    state.channel_preset_warnings =
        validate_channel_presets_for_state(&state, &session.channel_presets);
    state.channel_presets = session.channel_presets.clone();
    state.selected_channel_preset_index = (!state.channel_presets.is_empty()).then_some(0);
    let layer_index = state
        .layers
        .iter()
        .position(|layer| layer.id == session.active_layer_id)
        .ok_or_else(|| {
            anyhow::anyhow!("session layer {:?} was not found", session.active_layer_id)
        })?;
    activate_layer_timepoint_state_only(&mut state, layer_index, session.active_timepoint)?;
    state.iso_light_state = session.iso_light_state.validate()?;
    state.camera = session.camera;
    state.active_projection = session.camera.projection;
    apply_viewer_layout_session(&mut state, &session.viewer_layout)?;
    state.scene_artifacts = session.scene_artifacts.clone();
    state.analysis_tables = session.analysis_tables.clone();
    state.analysis_plots = session.analysis_plots.clone();
    state.analysis_operations = session.analysis_operations.clone();
    state.last_analysis_export_csv = None;
    normalize_analysis_selection(&mut state);
    rerender_state_with_backend(&mut state, gpu_renderer)?;
    state.last_render_error = None;
    state.last_workflow_message = Some("Opened project".to_owned());
    Ok(state)
}

pub(crate) fn open_state_from_session_with_relocated_dataset(
    session: &AppSession,
    relocated_dataset_path: impl AsRef<Path>,
    gpu_renderer: Option<&GpuRenderer>,
    preferences: &AppPreferences,
) -> anyhow::Result<AppState> {
    let mut relocated = session.clone();
    relocated.dataset.path = relocated_dataset_path.as_ref().to_path_buf();
    open_state_from_session_with_preferences(&relocated, gpu_renderer, preferences)
}
