use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, ApplicationState, SourceSessionGeneration,
};
use mirante4d_data::BrickReadPool;
use mirante4d_domain::TimeIndex;
use mirante4d_renderer::gpu::GpuRenderer;
use mirante4d_settings::recommended_for_current_system;

use crate::{
    brick_streaming::{
        BrickSubmissionOptions, BrickSubmissionResult, apply_brick_read_outcome,
        cancel_brick_tickets, create_brick_read_pool, current_resident_frame_ready,
        submit_visible_bricks_to_pool, submit_visible_bricks_to_pool_with_options,
        view_for_snapshot,
    },
    cross_section_read_queue::create_cross_section_read_pool,
    current_runtime::{
        analysis::CurrentAnalysisRuntime, dataset::CurrentDatasetRuntime,
        render::CurrentRenderRuntime, ui::CurrentUiRuntime,
    },
    dataset_opening::{
        OpenedCurrentSource, open_dataset_with_resource_policy_and_render_first_frame,
    },
    layer_state::reconcile_view_runtime,
    playback::stepped_timepoint,
    render_state::rerender_state_with_backend,
    render_state_from_resident_bricks_with_backend,
    viewport::resident_brick_render_supported,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSmokeOptions {
    pub disable_gpu: bool,
    pub playback_steps: usize,
    pub timeout: Duration,
}

impl Default for AppSmokeOptions {
    fn default() -> Self {
        Self {
            disable_gpu: false,
            playback_steps: 0,
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSmokeReport {
    pub dataset_label: String,
    pub layer_count: usize,
    pub frame_width: u64,
    pub frame_height: u64,
    pub nonzero_pixels: u64,
    pub max_value: u16,
    pub displayed_scale_level: Option<u32>,
    pub target_scale_level: u32,
    pub render_mode: mirante4d_domain::RenderMode,
    pub gpu_adapter_summary: Option<String>,
    pub playback: Vec<PlaybackSmokeFrame>,
}

/// Runs the explicit headless support smoke without exposing predecessor or
/// temporary runtime owners through the public API.
pub fn run_headless_smoke(
    path: impl AsRef<Path>,
    options: AppSmokeOptions,
) -> anyhow::Result<AppSmokeReport> {
    let resource_policy = recommended_for_current_system(None)?;
    let OpenedCurrentSource {
        startup_diagnostics: _,
        catalog,
        workspace,
        mut dataset_runtime,
        mut render_runtime,
        mut analysis_runtime,
    } = open_dataset_with_resource_policy_and_render_first_frame(path, resource_policy)?;
    let mut application = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        catalog.as_ref().clone(),
        workspace,
        resource_policy,
    )
    .map_err(|code| anyhow::anyhow!("smoke application state rejected: {code:?}"))?;
    dataset_runtime.brick_read_pool = create_brick_read_pool(&dataset_runtime);
    dataset_runtime.cross_section_read_pool = create_cross_section_read_pool(&dataset_runtime);
    if dataset_runtime.brick_read_pool.is_none()
        || dataset_runtime.cross_section_read_pool.is_none()
    {
        anyhow::bail!("failed to start bounded smoke dataset workers");
    }
    let ui_runtime = CurrentUiRuntime::new(resource_policy, None);
    let runtime_policy = resource_policy.current_runtime_adapter();
    let gpu_renderer = if options.disable_gpu {
        None
    } else {
        GpuRenderer::new_with_cache_budgets_blocking(
            runtime_policy.gpu_dense_cache_budget_bytes(),
            runtime_policy.gpu_brick_cache_budget_bytes(),
        )
        .ok()
        .map(Arc::new)
    };
    render_runtime.gpu_renderer = gpu_renderer.clone();
    let snapshot = application.snapshot();
    render_first_streamed_frame_for_smoke(
        &snapshot,
        &mut dataset_runtime,
        &mut render_runtime,
        &mut analysis_runtime,
        &ui_runtime,
        gpu_renderer.as_deref(),
        options.timeout,
    )?;
    let playback = render_playback_steps_for_smoke(
        &mut application,
        &mut dataset_runtime,
        &mut render_runtime,
        &mut analysis_runtime,
        &ui_runtime,
        gpu_renderer.as_deref(),
        options.playback_steps,
        options.timeout,
    )?;
    let final_snapshot = application.snapshot();
    let render_mode = active_render_mode(&final_snapshot)?;
    let gpu_adapter_summary = gpu_renderer.as_ref().map(|renderer| {
        let adapter = renderer.adapter_diagnostics();
        format!(
            "{} {} {} driver={} {}",
            adapter.backend, adapter.device_type, adapter.name, adapter.driver, adapter.driver_info
        )
    });
    Ok(AppSmokeReport {
        dataset_label: final_snapshot.catalog().label().to_owned(),
        layer_count: final_snapshot.catalog().len(),
        frame_width: render_runtime.frame.width,
        frame_height: render_runtime.frame.height,
        nonzero_pixels: render_runtime.diagnostics.nonzero_pixels,
        max_value: render_runtime.diagnostics.max_value,
        displayed_scale_level: render_runtime.frame_fidelity.displayed_scale_level,
        target_scale_level: render_runtime.frame_fidelity.target_scale_level,
        render_mode,
        gpu_adapter_summary,
        playback,
    })
}

/// Smoke-only rendering entry point. Allowing `gpu_renderer == None` here is
/// the explicit CPU-reference test path; product rendering must not treat it
/// as an interactive fallback.
#[allow(clippy::redundant_closure_call)] // the closure restores the taken pool after fallible work
pub(crate) fn render_first_streamed_frame_for_smoke(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    ui: &CurrentUiRuntime,
    gpu_renderer: Option<&GpuRenderer>,
    timeout: Duration,
) -> anyhow::Result<()> {
    rerender_state_with_backend(snapshot, dataset, analysis, ui, render, gpu_renderer)?;
    if dataset.active_volume_u8.is_some()
        || dataset.active_volume.is_some()
        || dataset.active_volume_f32.is_some()
    {
        return Ok(());
    }

    let mode = active_render_mode(snapshot)?;
    if !resident_brick_render_supported(mode) {
        anyhow::bail!("smoke streaming render does not support {mode:?}");
    }

    let pool = take_smoke_pool(dataset)?;
    let result = (|| {
        let submission = submit_visible_bricks_to_pool(snapshot, dataset, analysis, render, &pool);
        install_submission_tickets(dataset, submission);
        wait_for_smoke_frame(
            snapshot,
            dataset,
            render,
            analysis,
            ui,
            &pool,
            gpu_renderer,
            timeout,
            None,
        )
    })();
    dataset.brick_read_pool = Some(pool);
    result
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackSmokeFrame {
    pub timepoint: u64,
    pub elapsed_ms: f64,
    pub nonzero_pixels: u64,
    pub max_value: u16,
    pub displayed_scale_level: u32,
    pub target_scale_level: u32,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_playback_steps_for_smoke(
    application: &mut ApplicationState,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    ui: &CurrentUiRuntime,
    gpu_renderer: Option<&GpuRenderer>,
    steps: usize,
    timeout_per_step: Duration,
) -> anyhow::Result<Vec<PlaybackSmokeFrame>> {
    let initial_snapshot = application.snapshot();
    render_first_streamed_frame_for_smoke(
        &initial_snapshot,
        dataset,
        render,
        analysis,
        ui,
        gpu_renderer,
        timeout_per_step,
    )?;
    let timepoint_count = active_timepoint_count(&initial_snapshot)?;
    if timepoint_count <= 1 || steps == 0 {
        return Ok(Vec::new());
    }
    if render.diagnostics.nonzero_pixels == 0 {
        anyhow::bail!("playback smoke cannot start from a blank rendered frame");
    }

    let pool = take_smoke_pool(dataset)?;
    let result = (|| {
        let mut frames = Vec::with_capacity(steps);
        for _ in 0..steps {
            let before = application.snapshot();
            let previous_view = view_for_snapshot(&before).clone();
            let target = stepped_timepoint(previous_view.timepoint(), timepoint_count, 1);
            let previous_frame = render.frame.clone();
            let previous_frame_f32 = render.frame_f32.clone();
            if !previous_frame.pixels().iter().any(|value| *value > 0) {
                anyhow::bail!(
                    "playback smoke reached a blank frame before requesting timepoint {}",
                    target.get()
                );
            }

            application
                .dispatch(ApplicationCommand::SetTimepoint(target))
                .map_err(|fault| anyhow::anyhow!("playback smoke timepoint rejected: {fault:?}"))?;
            let snapshot = application.snapshot();
            if !reconcile_view_runtime(&previous_view, &snapshot, dataset, render, analysis)? {
                anyhow::bail!(
                    "playback smoke did not observe a source-selection change for timepoint {}",
                    target.get()
                );
            }
            if render.frame != previous_frame || render.frame_f32 != previous_frame_f32 {
                anyhow::bail!(
                    "playback smoke did not preserve the displayed frame while loading timepoint {}",
                    target.get()
                );
            }

            let started = Instant::now();
            let submission = submit_visible_bricks_to_pool_with_options(
                &snapshot,
                dataset,
                analysis,
                render,
                &pool,
                BrickSubmissionOptions::PLAYBACK,
            );
            install_submission_tickets(dataset, submission);
            wait_for_smoke_frame(
                &snapshot,
                dataset,
                render,
                analysis,
                ui,
                &pool,
                gpu_renderer,
                timeout_per_step,
                Some(target),
            )?;

            if view_for_snapshot(&snapshot).timepoint() != target {
                anyhow::bail!(
                    "playback smoke rendered timepoint {}, expected {}",
                    view_for_snapshot(&snapshot).timepoint().get(),
                    target.get()
                );
            }
            if render.diagnostics.nonzero_pixels == 0 {
                anyhow::bail!("playback smoke rendered blank timepoint {}", target.get());
            }
            frames.push(PlaybackSmokeFrame {
                timepoint: target.get(),
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                nonzero_pixels: render.diagnostics.nonzero_pixels,
                max_value: render.diagnostics.max_value,
                displayed_scale_level: render.frame_fidelity.displayed_scale_level.unwrap_or(0),
                target_scale_level: render.frame_fidelity.target_scale_level,
            });
        }
        Ok(frames)
    })();
    dataset.brick_read_pool = Some(pool);
    result
}

#[allow(clippy::too_many_arguments)]
fn wait_for_smoke_frame(
    snapshot: &ApplicationSnapshot,
    dataset: &mut CurrentDatasetRuntime,
    render: &mut CurrentRenderRuntime,
    analysis: &mut CurrentAnalysisRuntime,
    ui: &CurrentUiRuntime,
    pool: &BrickReadPool,
    gpu_renderer: Option<&GpuRenderer>,
    timeout: Duration,
    expected_timepoint: Option<TimeIndex>,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if current_resident_frame_ready(snapshot, dataset, render) {
            render_state_from_resident_bricks_with_backend(
                snapshot,
                dataset,
                render,
                analysis,
                ui,
                gpu_renderer,
            )?;
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            let timepoint = expected_timepoint
                .map(|timepoint| format!(" for timepoint {}", timepoint.get()))
                .unwrap_or_default();
            anyhow::bail!(
                "timed out waiting for streamed frame{timepoint}: {}/{} current bricks",
                dataset.brick_stream_completed,
                dataset.brick_stream_requested
            );
        }
        let remaining = deadline.saturating_duration_since(now);
        if let Some(outcome) = pool.recv_timeout(remaining.min(Duration::from_millis(250))) {
            apply_brick_read_outcome(snapshot, dataset, analysis, render, outcome);
        }
    }
}

fn take_smoke_pool(dataset: &mut CurrentDatasetRuntime) -> anyhow::Result<BrickReadPool> {
    dataset
        .brick_read_pool
        .take()
        .or_else(|| create_brick_read_pool(dataset))
        .ok_or_else(|| anyhow::anyhow!("failed to create smoke brick worker pool"))
}

fn install_submission_tickets(
    dataset: &mut CurrentDatasetRuntime,
    submission: BrickSubmissionResult,
) {
    if !submission.current_changed {
        return;
    }
    cancel_brick_tickets(&mut dataset.current_brick_tickets);
    cancel_brick_tickets(&mut dataset.prefetch_brick_tickets);
    cancel_brick_tickets(&mut dataset.warm_brick_tickets);
    dataset.current_brick_tickets = submission.current_tickets;
    dataset.prefetch_brick_tickets = submission.prefetch_tickets;
    dataset.warm_brick_tickets = submission.warm_tickets;
}

fn active_render_mode(
    snapshot: &ApplicationSnapshot,
) -> anyhow::Result<mirante4d_domain::RenderMode> {
    let view = view_for_snapshot(snapshot);
    view.layer(view.active_layer())
        .map(|layer| layer.render_state().mode())
        .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the canonical view"))
}

fn active_timepoint_count(snapshot: &ApplicationSnapshot) -> anyhow::Result<u64> {
    let view = view_for_snapshot(snapshot);
    snapshot
        .catalog()
        .layer(view.active_layer())
        .map(|layer| layer.shape().t())
        .ok_or_else(|| anyhow::anyhow!("active logical layer is absent from the catalog"))
}
