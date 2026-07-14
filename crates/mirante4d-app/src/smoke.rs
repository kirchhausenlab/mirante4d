//! Explicit headless support smoke for the unified dataset runtime.
//!
//! This is not the interactive product-validation gate. It decodes the same
//! semantic lease requirements and inspects valid samples without installing a
//! CPU product-rendering fallback.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use mirante4d_application::{ApplicationCommand, ApplicationState, SourceSessionGeneration};
use mirante4d_dataset::{CpuLedgerCategory, ResourceLease, ResourcePayloadView};
use mirante4d_dataset_runtime::RequestPriority;
use mirante4d_domain::IntensityDType;
use mirante4d_render_api::MAX_RENDER_REQUIREMENTS;
use mirante4d_renderer::gpu::GpuRenderer;
use mirante4d_settings::recommended_for_current_system;

use crate::{
    application_view,
    dataset_demand_plan::{
        DatasetDemandPlanLimits, plan_current_3d, render_extent_from_dimensions,
    },
    dataset_requests::SCOPE_CURRENT_3D,
    playback::stepped_timepoint,
    unified_source_open,
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

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackSmokeFrame {
    pub timepoint: u64,
    pub elapsed_ms: f64,
    pub nonzero_pixels: u64,
    pub max_value: u16,
    pub displayed_scale_level: u32,
    pub target_scale_level: u32,
}

pub fn run_headless_smoke(
    path: impl AsRef<Path>,
    options: AppSmokeOptions,
) -> anyhow::Result<AppSmokeReport> {
    let resource_policy = recommended_for_current_system(None)?;
    let mut opened = unified_source_open::open(
        path,
        resource_policy,
        mirante4d_dataset::DatasetSourceId::new(1),
    )?;
    let mut application = ApplicationState::new_unbound(
        SourceSessionGeneration::new(1),
        opened.catalog.as_ref().clone(),
        opened.workspace.clone(),
        resource_policy,
    )
    .map_err(|code| anyhow::anyhow!("smoke application state rejected: {code:?}"))?;

    let gpu_renderer = if options.disable_gpu {
        None
    } else {
        let policy = resource_policy.current_runtime_adapter();
        Some(GpuRenderer::new_with_cache_budgets_blocking(
            policy.gpu_dense_cache_budget_bytes(),
            policy.gpu_brick_cache_budget_bytes(),
        )?)
    };
    let gpu_adapter_summary = gpu_renderer.as_ref().map(|renderer| {
        let adapter = renderer.adapter_diagnostics();
        format!(
            "{} {} {} driver={} {}",
            adapter.backend, adapter.device_type, adapter.name, adapter.driver, adapter.driver_info
        )
    });

    load_current_requirements(&application, &mut opened, options.timeout)?;
    let (nonzero_pixels, max_value) = retained_sample_summary(&opened.render_runtime.lease_bridge)?;
    if nonzero_pixels == 0 {
        anyhow::bail!("unified runtime smoke decoded only zero or invalid visible samples");
    }

    let mut playback = Vec::with_capacity(options.playback_steps);
    let timepoints = application
        .snapshot()
        .catalog()
        .layers()
        .map(|layer| layer.shape().t())
        .min()
        .unwrap_or(1);
    for _ in 0..options.playback_steps {
        if timepoints <= 1 {
            break;
        }
        let before = application.snapshot();
        let next = stepped_timepoint(application_view(&before).timepoint(), timepoints, 1);
        application
            .dispatch(ApplicationCommand::SetTimepoint(next))
            .map_err(|fault| anyhow::anyhow!("smoke timepoint rejected: {fault:?}"))?;
        let started = Instant::now();
        load_current_requirements(&application, &mut opened, options.timeout)?;
        let (nonzero_pixels, max_value) =
            retained_sample_summary(&opened.render_runtime.lease_bridge)?;
        if nonzero_pixels == 0 {
            anyhow::bail!(
                "unified runtime smoke decoded a blank timepoint {}",
                next.get()
            );
        }
        playback.push(PlaybackSmokeFrame {
            timepoint: next.get(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
            nonzero_pixels,
            max_value,
            displayed_scale_level: opened.dataset.current_scale().get(),
            target_scale_level: opened.dataset.current_scale().get(),
        });
    }

    let snapshot = application.snapshot();
    let view = application_view(&snapshot);
    let render_mode = view
        .layer(view.active_layer())
        .expect("application view contains its active layer")
        .render_state()
        .mode();
    let report = AppSmokeReport {
        dataset_label: snapshot.catalog().label().to_owned(),
        layer_count: snapshot.catalog().len(),
        frame_width: opened.render_runtime.render_viewport.width,
        frame_height: opened.render_runtime.render_viewport.height,
        nonzero_pixels,
        max_value,
        displayed_scale_level: Some(opened.dataset.current_scale().get()),
        target_scale_level: opened.dataset.current_scale().get(),
        render_mode,
        gpu_adapter_summary,
        playback,
    };
    opened.dataset.request_shutdown()?;
    Ok(report)
}

fn load_current_requirements(
    application: &ApplicationState,
    opened: &mut unified_source_open::UnifiedOpenedSource,
    timeout: Duration,
) -> anyhow::Result<()> {
    let snapshot = application.snapshot();
    let diagnostics = opened.dataset.dispatcher().diagnostics()?;
    let plan = plan_current_3d(
        snapshot.catalog(),
        application_view(&snapshot),
        opened.render_runtime.presentation_viewport,
        render_extent_from_dimensions(
            opened.render_runtime.render_viewport.width,
            opened.render_runtime.render_viewport.height,
        )?,
        DatasetDemandPlanLimits::new(
            MAX_RENDER_REQUIREMENTS,
            MAX_RENDER_REQUIREMENTS,
            diagnostics.category_cap_bytes(CpuLedgerCategory::DecodedResidency),
        ),
        false,
    )?;
    opened.dataset.install_current_plan(plan, false)?;
    opened
        .render_runtime
        .lease_bridge
        .replace_current_requirements(opened.dataset.renderer_requirements())?;
    opened.dataset.submit_scope(
        SCOPE_CURRENT_3D,
        RequestPriority::CurrentView,
        &opened.render_runtime.lease_bridge,
    )?;

    let deadline = Instant::now() + timeout;
    while !opened
        .dataset
        .scope_complete(SCOPE_CURRENT_3D, &opened.render_runtime.lease_bridge)
    {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for unified runtime leases: {} retained, {} missing",
                opened.render_runtime.lease_bridge.retained_len(),
                opened.render_runtime.lease_bridge.missing_len()
            );
        }
        let (dataset, render) = (&mut opened.dataset, &mut opened.render_runtime);
        let bridge = &mut render.lease_bridge;
        dataset.dispatcher_mut().drain(32, |ticket, outcome| {
            if let mirante4d_dataset_runtime::RuntimeOutcome::Ready(lease) = outcome
                && bridge.requires(ticket.resource())
            {
                let lease: Arc<dyn ResourceLease> = Arc::new(lease);
                bridge
                    .install(lease)
                    .expect("a current smoke completion matches its installed requirements");
            }
        })?;
        std::thread::yield_now();
    }
    Ok(())
}

fn retained_sample_summary(
    bridge: &mirante4d_renderer::CurrentLeaseBridge,
) -> anyhow::Result<(u64, u16)> {
    let mut nonzero = 0_u64;
    let mut maximum = 0_u16;
    for (_, payload) in bridge.retained_payloads() {
        summarize_payload(payload, &mut nonzero, &mut maximum)?;
    }
    Ok((nonzero, maximum))
}

fn summarize_payload(
    payload: ResourcePayloadView<'_>,
    nonzero: &mut u64,
    maximum: &mut u16,
) -> anyhow::Result<()> {
    let width = usize::from(payload.dtype().bytes_per_sample());
    for index in 0..payload.sample_count() {
        if !payload.sample_is_valid(index)? {
            continue;
        }
        let offset = usize::try_from(index)? * width;
        let value = match payload.dtype() {
            IntensityDType::Uint8 => u16::from(payload.value_bytes()[offset]),
            IntensityDType::Uint16 => u16::from_le_bytes(
                payload.value_bytes()[offset..offset + 2]
                    .try_into()
                    .expect("validated u16 payload contains a complete sample"),
            ),
            IntensityDType::Float32 => {
                let value = f32::from_le_bytes(
                    payload.value_bytes()[offset..offset + 4]
                        .try_into()
                        .expect("validated f32 payload contains a complete sample"),
                );
                value.clamp(0.0, f32::from(u16::MAX)).round() as u16
            }
        };
        if value != 0 {
            *nonzero = nonzero.saturating_add(1);
        }
        *maximum = (*maximum).max(value);
    }
    Ok(())
}
