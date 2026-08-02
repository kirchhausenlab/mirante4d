//! Explicit headless support smoke for the unified dataset runtime.
//!
//! This is not the interactive product-validation gate. It decodes the same
//! semantic lease requirements and inspects valid samples without installing a
//! CPU product-rendering fallback.

use std::{
    future::Future,
    path::Path,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use mirante4d_application::{
    ApplicationCommand, ApplicationState, RenderIntentMailbox, SourceSessionGeneration,
    stepped_timepoint,
};
use mirante4d_dataset::{CpuLedgerCategory, ResourcePayloadView};
use mirante4d_domain::IntensityDType;
use mirante4d_render_api::MAX_RENDER_REQUIREMENTS;
use mirante4d_render_wgpu::{qualify_adapter, renderer_device_descriptor};
use mirante4d_settings::recommended_for_current_system;

use crate::{
    application_view,
    camera_demand_cache::{
        CameraDemandRequest, Current3dDemandBaselines, Current3dDemandRequest,
        PreparedRendererRequirementUpdate, PreparedVisibleDemand, ScopeDemandBaseline,
    },
    dataset_demand_plan::DatasetDemandPlanLimits,
    dataset_requests::{
        SCOPE_ANALYSIS, SCOPE_CURRENT_3D, SCOPE_CURRENT_3D_REFINEMENT, SCOPE_PLAYBACK,
        ScopeReconciliationTargets,
    },
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
    pub target_scale_level: Option<u32>,
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
        opened.source_reference.clone(),
        opened.content_address_origin,
        opened.workspace.clone(),
        resource_policy,
    )
    .map_err(|code| anyhow::anyhow!("smoke application state rejected: {code:?}"))?;

    let gpu_adapter_summary = if options.disable_gpu {
        None
    } else {
        Some(gpu_adapter_summary()?)
    };

    load_current_requirements(&application, &mut opened, options.timeout)?;
    let (nonzero_pixels, max_value) = retained_sample_summary(opened.dataset.retained_leases())?;
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
            retained_sample_summary(opened.dataset.retained_leases())?;
        if nonzero_pixels == 0 {
            anyhow::bail!(
                "unified runtime smoke decoded a blank timepoint {}",
                next.get()
            );
        }
        let uniform_scale = opened.dataset.current_uniform_scale().ok_or_else(|| {
            anyhow::anyhow!("playback smoke requires one uniform visible-layer scale")
        })?;
        playback.push(PlaybackSmokeFrame {
            timepoint: next.get(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
            nonzero_pixels,
            max_value,
            displayed_scale_level: uniform_scale.get(),
            target_scale_level: uniform_scale.get(),
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
        frame_width: u64::from(opened.render_coordination.render_viewport.width_pixels()),
        frame_height: u64::from(opened.render_coordination.render_viewport.height_pixels()),
        nonzero_pixels,
        max_value,
        displayed_scale_level: opened
            .dataset
            .current_uniform_scale()
            .map(mirante4d_domain::ScaleLevel::get),
        target_scale_level: opened
            .dataset
            .current_uniform_scale()
            .map(mirante4d_domain::ScaleLevel::get),
        render_mode,
        gpu_adapter_summary,
        playback,
    };
    opened.dataset.request_shutdown()?;
    Ok(report)
}

fn gpu_adapter_summary() -> anyhow::Result<String> {
    wait_for_future(async {
        let instance = eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor {
            backends: eframe::wgpu::Backends::VULKAN,
            ..eframe::wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&eframe::wgpu::RequestAdapterOptions {
                power_preference: eframe::wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|error| anyhow::anyhow!("no Vulkan GPU adapter is available: {error}"))?;
        qualify_adapter(&adapter)?;
        let descriptor = renderer_device_descriptor(&adapter, "mirante4d-headless-smoke-device")?;
        let (_device, _queue) = adapter.request_device(&descriptor).await?;
        let info = adapter.get_info();
        Ok(format!(
            "{:?} {:?} {} driver={} {}",
            info.backend, info.device_type, info.name, info.driver, info.driver_info
        ))
    })
}

fn wait_for_future<F: Future>(future: F) -> F::Output {
    struct ThreadWake(thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

fn load_current_requirements(
    application: &ApplicationState,
    opened: &mut unified_source_open::UnifiedOpenedSource,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let snapshot = application.snapshot();
    let diagnostics = opened.dataset.dispatcher().diagnostics()?;
    let baseline = |scope| {
        ScopeDemandBaseline::new(
            opened.dataset.scope_prepared_body_handle(scope),
            opened.dataset.scope_admitted_prefix_len(scope),
        )
    };
    let intent_revision = RenderIntentMailbox::new().snapshot().latest_revision;
    let global_limits = DatasetDemandPlanLimits::new(
        MAX_RENDER_REQUIREMENTS,
        MAX_RENDER_REQUIREMENTS,
        diagnostics.category_cap_bytes(CpuLedgerCategory::DecodedResidency),
    );
    let request = CameraDemandRequest::new(
        intent_revision,
        Arc::clone(snapshot.catalog()),
        opened.dataset.cpu_ledger_arc(),
        application_view(&snapshot).clone(),
        global_limits,
        Some(Current3dDemandRequest::new(
            opened.render_coordination.presentation_viewport,
            opened.render_coordination.render_viewport,
            global_limits,
            Current3dDemandBaselines::new(
                baseline(SCOPE_CURRENT_3D),
                baseline(SCOPE_CURRENT_3D_REFINEMENT),
                baseline(SCOPE_PLAYBACK),
            ),
        )),
        Vec::new(),
        opened.dataset.renderer_requirement_handle(),
        vec![opened.dataset.scope_prepared_body_handle(SCOPE_ANALYSIS)],
        None,
    );
    if !opened.dataset.submit_visible_demand_plan(request) {
        anyhow::bail!("headless demand used a stale render-intent revision");
    }
    let prepared = loop {
        if let Some(result) = opened.dataset.take_visible_demand_plan_result() {
            break result.outcome?;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for headless camera-demand planning");
        }
        std::thread::yield_now();
    };
    let PreparedVisibleDemand {
        targets,
        renderer_requirement_update,
        renderer_requirement_payload_bytes: _,
        post_refinement_promotion_update,
        candidates_visited: _,
    } = prepared;
    let (current_3d, cross_sections, temporal_frame_contract) = targets.into_parts();
    if temporal_frame_contract.is_some() {
        anyhow::bail!("headless non-playback planning unexpectedly produced a temporal frame");
    }
    if !cross_sections.is_empty() {
        anyhow::bail!("headless single-view planning unexpectedly produced linked-panel demand");
    }
    let current = current_3d
        .ok_or_else(|| anyhow::anyhow!("headless camera planning omitted current 3D demand"))?;
    let playback_layer_scales = current.plan.target.layer_scales.clone();
    let mut scope_targets = ScopeReconciliationTargets::default();
    opened.dataset.prepare_progressive_current_scope_targets(
        &current.plan,
        false,
        false,
        &mut scope_targets,
    );
    scope_targets.replace(SCOPE_PLAYBACK, &current.playback.requirements);
    opened
        .dataset
        .preflight_prepared_renderer_requirement_update(
            &renderer_requirement_update.previous.requirements,
            &renderer_requirement_update.next.requirements,
        )?;
    if let Some(promotion) = post_refinement_promotion_update.as_ref() {
        if !Arc::ptr_eq(
            &promotion.previous.requirements,
            &renderer_requirement_update.next.requirements,
        ) {
            anyhow::bail!("headless promotion union is not bound to the installed worker union");
        }
        opened
            .dataset
            .preflight_prepared_renderer_requirement_union(&promotion.next.requirements)?;
    }
    let reconciliation = opened.dataset.prepare_scope_reconciliation(scope_targets)?;
    opened
        .dataset
        .commit_prepared_scope_reconciliation(reconciliation)?;
    opened
        .dataset
        .commit_preflighted_progressive_current_plan(current.plan, false, false);
    opened.dataset.commit_preflighted_scope_replacement(
        SCOPE_PLAYBACK,
        current.playback.requirements,
        playback_layer_scales,
    );
    let PreparedRendererRequirementUpdate {
        previous,
        next,
        removals,
        removal_charge: _removal_charge,
    } = renderer_requirement_update;
    opened
        .dataset
        .commit_preflighted_renderer_requirement_update(
            previous.requirements,
            next.requirements,
            &removals,
            next.charge,
        );
    let mut promotion_update = if opened.dataset.staging_current_refinement() {
        post_refinement_promotion_update
    } else {
        None
    };
    opened.dataset.pump_interactive_admission()?;

    while opened.dataset.staging_current_refinement()
        || !opened.dataset.scope_complete(SCOPE_CURRENT_3D)
    {
        if Instant::now() >= deadline {
            let retained = opened.dataset.retained_leases();
            anyhow::bail!(
                "timed out waiting for unified runtime leases: {} retained, {} missing",
                retained.retained_len(),
                retained.missing_len()
            );
        }
        opened
            .dataset
            .drain_runtime_results(32, |_ticket, _outcome| {})?;
        opened.dataset.pump_interactive_admission()?;
        if opened.dataset.staging_current_refinement()
            && opened
                .dataset
                .scope_resources_complete(SCOPE_CURRENT_3D_REFINEMENT)
        {
            let update = promotion_update.as_ref().ok_or_else(|| {
                anyhow::anyhow!("a staged headless target has no worker promotion union")
            })?;
            let mut promotion_targets = ScopeReconciliationTargets::default();
            if !opened
                .dataset
                .prepare_staged_current_promotion_scope_targets(&mut promotion_targets)
            {
                anyhow::bail!("a complete headless refinement has no staged dataset plan");
            }
            opened
                .dataset
                .preflight_prepared_renderer_requirement_update(
                    &update.previous.requirements,
                    &update.next.requirements,
                )?;
            let promotion_reconciliation = opened
                .dataset
                .prepare_scope_reconciliation(promotion_targets)?;
            opened
                .dataset
                .commit_prepared_scope_reconciliation(promotion_reconciliation)?;
            opened
                .dataset
                .commit_reconciled_gpu_prefilled_staged_current_plan();
            let PreparedRendererRequirementUpdate {
                previous,
                next,
                removals,
                removal_charge: _removal_charge,
            } = promotion_update
                .take()
                .expect("the checked headless promotion update remains installed");
            opened
                .dataset
                .commit_preflighted_renderer_requirement_update(
                    previous.requirements,
                    next.requirements,
                    &removals,
                    next.charge,
                );
        }
        std::thread::yield_now();
    }
    Ok(())
}

fn retained_sample_summary(
    bridge: &crate::retained_leases::RetainedLeases,
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
