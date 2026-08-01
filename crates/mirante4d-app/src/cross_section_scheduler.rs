//! Generation-scoped cross-section presentation scheduling.
//!
//! This scheduler observes semantic lease cohorts. It does not plan storage
//! chunks, own payloads, submit reads, or maintain a second residency model.

use std::time::Instant;

use mirante4d_application::{
    CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
    CrossSectionPanelScheduleStatus, RenderCoordinationState,
};
use mirante4d_dataset::BrickKey;
use mirante4d_domain::{ScaleLevel, ViewerLayout};
use mirante4d_project_model::ViewState;

use crate::{retained_leases::RetainedLeases, viewer_layout::PanelId};

pub(crate) const CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelSchedulePlan {
    pub(crate) schedule: CrossSectionPanelScheduleState,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrossSectionScheduleInput<'a> {
    pub(crate) view: &'a ViewState,
    pub(crate) active_layer_target: Option<ScaleLevel>,
    /// The frame-visible required prefix in ranked order. Resident-navigation
    /// guard resources are prefetch state and must not downgrade a rendered
    /// current schedule when their asynchronous planning completes.
    pub(crate) requirements: &'a [BrickKey],
    /// Complete geometry-independent navigation prefix. When resident, the
    /// Plane shader can render every current pixel even while selected target
    /// requirements are still refining.
    pub(crate) first_useful_requirements: usize,
    pub(crate) first_useful_available: bool,
    pub(crate) retained_leases: &'a RetainedLeases,
    /// Monotone dataset + renderer readiness proof for this exact immutable
    /// requirement body. Once true, counting the body is sufficient and the
    /// scheduler must not revisit individual keys.
    pub(crate) all_requirements_available: bool,
    pub(crate) dataset_failed: bool,
    #[cfg(test)]
    pub(crate) requirement_visit_counter: Option<&'a std::cell::Cell<u64>>,
}

pub(crate) fn schedule_cross_section_panel(
    coordination: &mut RenderCoordinationState,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelSchedulePlan> {
    let schedule_start = Instant::now();
    let schedule =
        build_cross_section_panel_schedule(coordination, input, panel_id, gpu_display_available)?;
    let schedule_ms = schedule_start.elapsed().as_secs_f64() * 1000.0;
    if schedule_ms > CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS {
        tracing::debug!(
            panel = panel_id.label(),
            schedule_ms,
            budget_ms = CROSS_SECTION_PANEL_SCHEDULER_CPU_BUDGET_MS,
            "cross-section panel scheduling exceeded its per-panel CPU budget"
        );
    }
    coordination.set_cross_section_schedule(panel_id.presentation_slot(), schedule);
    Ok(CrossSectionPanelSchedulePlan { schedule })
}

fn build_cross_section_panel_schedule(
    coordination: &RenderCoordinationState,
    input: CrossSectionScheduleInput<'_>,
    panel_id: PanelId,
    gpu_display_available: bool,
) -> anyhow::Result<CrossSectionPanelScheduleState> {
    if input.view.layout() != ViewerLayout::FourPanel || panel_id.cross_section_panel().is_none() {
        return Ok(CrossSectionPanelScheduleState {
            status: CrossSectionPanelScheduleStatus::Unavailable,
            reason: CrossSectionPanelScheduleReason::GpuUnavailable,
            ..CrossSectionPanelScheduleState::missing_viewport(0)
        });
    }

    let panel_runtime = coordination.surface(panel_id.presentation_slot());
    let generation = panel_runtime.generation();
    if panel_runtime.presentation_viewport().is_none() || panel_runtime.render_viewport().is_none()
    {
        return Ok(CrossSectionPanelScheduleState::missing_viewport(generation));
    }

    let active_layer_scale_level = input.active_layer_target.map(ScaleLevel::get);
    let required = input.requirements.len();
    let retained = if input.all_requirements_available {
        required
    } else {
        let retained_cpu = input
            .requirements
            .iter()
            .filter(|key| {
                #[cfg(test)]
                if let Some(counter) = input.requirement_visit_counter {
                    counter.set(counter.get().saturating_add(1));
                }
                input.retained_leases.payload(**key).is_some()
            })
            .count();
        if input.first_useful_available {
            retained_cpu.max(input.first_useful_requirements.min(required))
        } else {
            retained_cpu
        }
    };
    let missing = required.saturating_sub(retained);

    let (status, reason) = classify_schedule(
        input.dataset_failed,
        gpu_display_available,
        required,
        missing,
    );

    Ok(CrossSectionPanelScheduleState {
        generation,
        target_scale_level: active_layer_scale_level,
        render_scale_level: active_layer_scale_level,
        fallback_scale_level: None,
        selected_bricks: required,
        occupied_selected_bricks: retained,
        missing_occupied_bricks: missing,
        estimated_decoded_bytes: 0,
        decoded_budget_bytes: 0,
        status,
        reason,
    })
}

fn classify_schedule(
    dataset_failed: bool,
    gpu_display_available: bool,
    required: usize,
    missing: usize,
) -> (
    CrossSectionPanelScheduleStatus,
    CrossSectionPanelScheduleReason,
) {
    if dataset_failed {
        (
            CrossSectionPanelScheduleStatus::Unavailable,
            CrossSectionPanelScheduleReason::RenderFailed,
        )
    } else if !gpu_display_available {
        (
            CrossSectionPanelScheduleStatus::Unavailable,
            CrossSectionPanelScheduleReason::GpuUnavailable,
        )
    } else if required == 0 {
        (
            CrossSectionPanelScheduleStatus::Empty,
            CrossSectionPanelScheduleReason::NoSelectedData,
        )
    } else if missing > 0 {
        (
            CrossSectionPanelScheduleStatus::Loading,
            CrossSectionPanelScheduleReason::MissingSelectedBricks,
        )
    } else {
        (
            CrossSectionPanelScheduleStatus::Ready,
            CrossSectionPanelScheduleReason::TargetScaleReady,
        )
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_application::{FrameFidelityStatus, PresentationSlot};
    use mirante4d_dataset::{DatasetResourceIdentity, DatasetSourceId, ResourceRegion};
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, IsoLightState, LayerTransfer, LogicalLayerKey,
        Opacity, Projection, RenderState, RgbColor, SamplingPolicy, Shape3D, TimeIndex,
        TransferCurve, UnitQuaternion, WorldPoint3,
    };
    use mirante4d_project_model::LayerViewState;
    use mirante4d_render_api::{PresentationViewport, RenderExtent};

    use super::*;

    #[test]
    fn empty_cross_section_demand_is_terminal_not_loading() {
        let (status, reason) = classify_schedule(false, true, 0, 0);
        assert_eq!(status, CrossSectionPanelScheduleStatus::Empty);
        assert_eq!(reason, CrossSectionPanelScheduleReason::NoSelectedData);
    }

    #[test]
    fn complete_cross_section_demand_has_no_coarse_fallback_classification() {
        let (status, reason) = classify_schedule(false, true, 4, 0);
        assert_eq!(status, CrossSectionPanelScheduleStatus::Ready);
        assert_eq!(reason, CrossSectionPanelScheduleReason::TargetScaleReady);
    }

    #[test]
    fn warm_renderer_complete_large_body_visits_no_requirements_and_records_current_schedule() {
        const LARGE_BODY_LEN: usize = 65_536;

        let layer_key = LogicalLayerKey::new(0);
        let layer = LayerViewState::new(
            layer_key,
            true,
            LayerTransfer::new(
                DisplayWindow::new(0.0, 1.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::VoxelExact),
        );
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::origin(),
            UnitQuaternion::identity(),
            1.0,
            1.0,
            10.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![layer],
            layer_key,
            TimeIndex::new(0),
            camera,
            ViewerLayout::FourPanel,
            CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let mut coordination = RenderCoordinationState::new(
            FrameFidelityStatus::new_with_presentation(extent, presentation),
        );
        assert!(coordination.record_viewports(PresentationSlot::Xy, presentation, extent,));
        let identity = DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(91));
        let requirements = (0..LARGE_BODY_LEN)
            .map(|index| {
                BrickKey::new(
                    identity,
                    layer_key,
                    TimeIndex::new(0),
                    ScaleLevel::BASE,
                    ResourceRegion::new(
                        [0, 0, u64::try_from(index).unwrap()],
                        Shape3D::new(1, 1, 1).unwrap(),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        // No CPU leases are retained: the warm scalar models the real
        // renderer-resident half of the combined all-complete authority.
        let retained_leases = RetainedLeases::default();
        let requirement_visits = std::cell::Cell::new(0_u64);

        let cold = schedule_cross_section_panel(
            &mut coordination,
            CrossSectionScheduleInput {
                view: &view,
                active_layer_target: Some(ScaleLevel::BASE),
                requirements: &requirements,
                first_useful_requirements: 1,
                first_useful_available: false,
                retained_leases: &retained_leases,
                all_requirements_available: false,
                dataset_failed: false,
                requirement_visit_counter: Some(&requirement_visits),
            },
            PanelId::Xy,
            true,
        )
        .unwrap()
        .schedule;
        assert_eq!(
            requirement_visits.get(),
            u64::try_from(LARGE_BODY_LEN).unwrap(),
            "the incomplete fallback must retain its exact per-key availability check"
        );
        assert_eq!(cold.status, CrossSectionPanelScheduleStatus::Loading);
        assert_eq!(cold.occupied_selected_bricks, 0);
        assert_eq!(cold.missing_occupied_bricks, LARGE_BODY_LEN);

        requirement_visits.set(0);
        let warm = schedule_cross_section_panel(
            &mut coordination,
            CrossSectionScheduleInput {
                view: &view,
                active_layer_target: Some(ScaleLevel::BASE),
                requirements: &requirements,
                first_useful_requirements: 1,
                first_useful_available: true,
                retained_leases: &retained_leases,
                all_requirements_available: true,
                dataset_failed: false,
                requirement_visit_counter: Some(&requirement_visits),
            },
            PanelId::Xy,
            true,
        )
        .unwrap()
        .schedule;
        assert_eq!(
            requirement_visits.get(),
            0,
            "a warm authoritative body must not revisit any requirement"
        );
        assert_eq!(warm.status, CrossSectionPanelScheduleStatus::Ready);
        assert_eq!(
            warm.reason,
            CrossSectionPanelScheduleReason::TargetScaleReady
        );
        assert_eq!(warm.selected_bricks, LARGE_BODY_LEN);
        assert_eq!(warm.occupied_selected_bricks, LARGE_BODY_LEN);
        assert_eq!(warm.missing_occupied_bricks, 0);
        assert_eq!(
            coordination
                .surface(PresentationSlot::Xy)
                .cross_section_schedule(),
            Some(warm)
        );

        assert!(coordination.record_cross_section_presentation(
            PresentationSlot::Xy,
            warm.generation,
            warm,
        ));
        let current = coordination
            .surface(PresentationSlot::Xy)
            .cross_section_schedule()
            .unwrap();
        assert_eq!(current.status, CrossSectionPanelScheduleStatus::Current);
        assert_eq!(current.reason, CrossSectionPanelScheduleReason::Rendered);
        assert_eq!(current.selected_bricks, LARGE_BODY_LEN);
        assert_eq!(current.occupied_selected_bricks, LARGE_BODY_LEN);
        assert_eq!(current.missing_occupied_bricks, 0);

        let repeated = schedule_cross_section_panel(
            &mut coordination,
            CrossSectionScheduleInput {
                view: &view,
                active_layer_target: Some(ScaleLevel::BASE),
                requirements: &requirements,
                first_useful_requirements: 1,
                first_useful_available: true,
                retained_leases: &retained_leases,
                all_requirements_available: true,
                dataset_failed: false,
                requirement_visit_counter: Some(&requirement_visits),
            },
            PanelId::Xy,
            true,
        )
        .unwrap()
        .schedule;
        assert_eq!(repeated.status, CrossSectionPanelScheduleStatus::Ready);
        assert_eq!(
            coordination
                .surface(PresentationSlot::Xy)
                .cross_section_schedule(),
            Some(current),
            "an identical prefetch-only Ready observation must not downgrade rendered currentness"
        );

        let changed = CrossSectionPanelScheduleState {
            selected_bricks: warm.selected_bricks + 1,
            occupied_selected_bricks: warm.occupied_selected_bricks + 1,
            ..warm
        };
        assert!(coordination.set_cross_section_schedule(PresentationSlot::Xy, changed));
        assert_eq!(
            coordination
                .surface(PresentationSlot::Xy)
                .cross_section_schedule(),
            Some(changed),
            "changed visible requirements must still replace the rendered schedule"
        );
    }
}
