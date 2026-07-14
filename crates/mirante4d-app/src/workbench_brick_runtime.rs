//! Unified interactive dataset demand and completion delivery.

use std::{collections::BTreeSet, sync::Arc, time::Instant};

use eframe::egui;
use mirante4d_application::ApplicationCommand;
use mirante4d_dataset::{CpuLedgerCategory, DatasetCatalog, DatasetResourceKey, ResourceLease};
use mirante4d_dataset_runtime::{RequestPriority, RuntimeFault, RuntimeFaultCode, RuntimeOutcome};
use mirante4d_domain::{TimeIndex, ViewerLayout};
use mirante4d_render_api::MAX_RENDER_REQUIREMENTS;

use crate::{
    FrameCompleteness, FrameFailureKind, LodDecisionReason, MiranteWorkbenchApp, RenderBackend,
    application_view,
    dataset_demand_plan::{
        DatasetDemandPlanCapacityError, DatasetDemandPlanLimits, plan_cross_section_panel,
        plan_current_3d,
    },
    dataset_requests::{
        SCOPE_ANALYSIS, SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ,
        SCOPE_CURRENT_3D, SCOPE_PLAYBACK,
    },
    viewer_layout::{
        CrossSectionPanelScheduleReason, CrossSectionPanelScheduleState,
        CrossSectionPanelScheduleStatus, PanelId,
    },
};

const SEMANTIC_PLAN_CANDIDATES_PER_LAYER: usize = MAX_RENDER_REQUIREMENTS;

#[cfg(not(test))]
const RESULT_DRAIN_LIMIT: usize = 32;
#[cfg(test)]
const RESULT_DRAIN_LIMIT: usize = 2;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleBrickRequestOutcome {
    pub(crate) current_changed: bool,
    pub(crate) resident_changed: bool,
    pub(crate) current_frame_ready: bool,
}

struct AggregateDemandSeed<'a> {
    current: &'a [DatasetResourceKey],
    current_decoded_bytes: u64,
    playback: &'a [DatasetResourceKey],
    decoded_capacity: u64,
}

impl MiranteWorkbenchApp {
    pub(crate) fn request_visible_bricks(&mut self) -> VisibleBrickRequestOutcome {
        let snapshot = self.application.snapshot();
        if self.dataset.resource_identity()
            != snapshot.catalog().scientific_identity().resource_identity()
        {
            self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Loading;
            self.render_runtime.frame_fidelity.reason = LodDecisionReason::LoadingTargetScale;
            self.render_runtime.frame_fidelity.backend = RenderBackend::Loading;
            return VisibleBrickRequestOutcome::default();
        }
        let view = application_view(&snapshot);
        let four_panel = view.layout() == ViewerLayout::FourPanel;
        let diagnostics = match self.dataset.dispatcher().diagnostics() {
            Ok(diagnostics) => diagnostics,
            Err(fault) => {
                self.record_dataset_fault(&fault);
                return VisibleBrickRequestOutcome::default();
            }
        };
        let playback_active = snapshot.transient().playback_active();
        let demand_cohorts = 1 + usize::from(playback_active) + if four_panel { 3 } else { 0 };
        let current_share_numerator = if playback_active { 2 } else { 1 };
        let decoded_capacity = diagnostics.category_cap_bytes(CpuLedgerCategory::DecodedResidency);
        let current_limits = DatasetDemandPlanLimits::new(
            SEMANTIC_PLAN_CANDIDATES_PER_LAYER,
            budget_share_usize(
                MAX_RENDER_REQUIREMENTS,
                current_share_numerator,
                demand_cohorts,
            ),
            budget_share_u64(decoded_capacity, current_share_numerator, demand_cohorts),
        );
        let plan = match plan_current_3d(
            snapshot.catalog(),
            view,
            self.render_runtime.presentation_viewport,
            self.render_runtime.render_viewport,
            current_limits,
            playback_active,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.record_dataset_plan_error(&error);
                return VisibleBrickRequestOutcome::default();
            }
        };

        let scale = plan.scale;
        let visible_count = plan.resources.len();
        let current_decoded_bytes = plan.decoded_bytes;
        let playback_requirements = playback_requirements(&snapshot, &plan.resources);
        let (cross_requirements, cross_plan_error) = match self.plan_cross_section_requirements(
            &snapshot,
            scale,
            four_panel,
            AggregateDemandSeed {
                current: &plan.resources,
                current_decoded_bytes,
                playback: &playback_requirements,
                decoded_capacity,
            },
        ) {
            Ok(requirements) => (requirements, None),
            Err(error) => (
                [
                    SCOPE_CROSS_SECTION_XY,
                    SCOPE_CROSS_SECTION_XZ,
                    SCOPE_CROSS_SECTION_YZ,
                ]
                .into_iter()
                .map(|scope| (scope, Vec::new()))
                .collect(),
                Some(error),
            ),
        };
        let current_changed = match self.dataset.install_current_plan(plan, four_panel) {
            Ok(changed) => changed,
            Err(fault) => {
                self.record_dataset_fault(&fault);
                return VisibleBrickRequestOutcome::default();
            }
        };
        self.render_runtime.visible_brick_count = visible_count;
        self.render_runtime.visible_brick_plan_error =
            cross_plan_error.as_ref().map(ToString::to_string);
        if let Some(error) = cross_plan_error.as_ref() {
            self.dataset.record_plan_error(error.to_string());
        }
        self.render_runtime.lod_schedule.target_scale_level = scale.get();
        self.render_runtime.lod_schedule.pending_scale_level = Some(scale.get());
        self.render_runtime.frame_fidelity.target_scale_level = scale.get();
        self.render_runtime.frame_fidelity.visible_bricks = visible_count;

        if let Err(fault) = self
            .dataset
            .set_scope_requirements(SCOPE_PLAYBACK, playback_requirements)
        {
            self.record_dataset_fault(&fault);
            return VisibleBrickRequestOutcome::default();
        }
        let mut cross_requirements_changed = false;
        for (scope, resources) in cross_requirements {
            match self.dataset.set_scope_requirements(scope, resources) {
                Ok(changed) => cross_requirements_changed |= changed,
                Err(fault) => {
                    self.record_dataset_fault(&fault);
                    return VisibleBrickRequestOutcome::default();
                }
            }
        }
        if cross_requirements_changed || cross_plan_error.is_some() {
            self.render_runtime
                .cross_section_runtime
                .mark_cross_section_panels_dirty();
        }
        if let Some(error) = cross_plan_error.as_ref() {
            self.mark_cross_section_plan_failure(error);
        }
        if let Err(error) = self
            .render_runtime
            .lease_bridge
            .replace_current_requirements(self.dataset.renderer_requirements())
        {
            self.dataset.record_plan_error(error.to_string());
            self.render_runtime.visible_brick_plan_error = Some(error.to_string());
            self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Incomplete;
            return VisibleBrickRequestOutcome::default();
        }
        self.dataset.begin_submission_pass();
        let mut submission_fault = None;
        for (scope, priority) in [
            (SCOPE_CURRENT_3D, RequestPriority::CurrentView),
            (SCOPE_CROSS_SECTION_XY, RequestPriority::LinkedView),
            (SCOPE_CROSS_SECTION_XZ, RequestPriority::LinkedView),
            (SCOPE_CROSS_SECTION_YZ, RequestPriority::LinkedView),
            (SCOPE_PLAYBACK, RequestPriority::Playback),
        ] {
            if scope != SCOPE_CURRENT_3D && scope != SCOPE_PLAYBACK && !four_panel {
                continue;
            }
            if let Err(fault) =
                self.dataset
                    .submit_scope(scope, priority, &self.render_runtime.lease_bridge)
            {
                submission_fault = Some(fault);
                break;
            }
        }
        if let Some(fault) = submission_fault {
            self.record_dataset_fault(&fault);
        }

        let ready = self
            .dataset
            .scope_complete(SCOPE_CURRENT_3D, &self.render_runtime.lease_bridge);
        self.update_dataset_fidelity(ready);
        VisibleBrickRequestOutcome {
            current_changed,
            resident_changed: false,
            current_frame_ready: ready,
        }
    }

    pub(crate) fn drain_brick_results(&mut self, ctx: &egui::Context) {
        let started = Instant::now();
        let snapshot = self.application.snapshot();
        if self.dataset.resource_identity()
            != snapshot.catalog().scientific_identity().resource_identity()
        {
            match self
                .dataset
                .dispatcher_mut()
                .drain(RESULT_DRAIN_LIMIT, |_ticket, _outcome| {})
            {
                Ok(drained) => {
                    let _ = self.dataset.dispatcher_mut().take_last_fault();
                    if drained > 0 || self.dataset.dispatcher().has_pending_work() {
                        ctx.request_repaint();
                    }
                }
                Err(fault) => self.dataset.record_plan_error(fault.to_string()),
            }
            return;
        }
        let (dataset, render, analysis) = (
            &mut self.dataset,
            &mut self.render_runtime,
            &mut self.analysis_runtime,
        );
        let bridge = &mut render.lease_bridge;
        let mut installed = false;
        let mut analysis_events = Vec::new();
        let mut analysis_errors = Vec::new();
        let drained = dataset.dispatcher_mut().drain(RESULT_DRAIN_LIMIT, |ticket, outcome| {
            if ticket.generation().scope() == SCOPE_ANALYSIS {
                if let Some(token) = analysis.active_token().cloned() {
                    match analysis.accept_completion(ticket, outcome) {
                        Ok(event) => analysis_events.push((token, event)),
                        Err(error) => analysis_errors.push((token, error)),
                    }
                }
            } else if let RuntimeOutcome::Ready(lease) = outcome
                && bridge.requires(ticket.resource())
            {
                let lease: Arc<dyn ResourceLease> = Arc::new(lease);
                match bridge.install(lease) {
                    Ok(newly_installed) => installed |= newly_installed,
                    Err(error) => tracing::error!(%error, "runtime lease delivery violated the renderer bridge contract"),
                }
            }
        });
        let drained = match drained {
            Ok(drained) => drained,
            Err(fault) => {
                self.record_dataset_fault(&fault);
                return;
            }
        };
        if let Some(fault) = self.dataset.dispatcher_mut().take_last_fault() {
            self.record_dataset_fault(&fault);
            return;
        }

        for (token, error) in analysis_errors {
            self.abort_running_analysis(
                &token,
                mirante4d_application::OperationFailureCode::AnalysisExecutionFailed,
                &error,
            );
        }
        let analysis_changed = !analysis_events.is_empty();
        for (token, event) in analysis_events {
            self.handle_analysis_runtime_event(token, event);
        }
        if self.analysis_runtime.active_token().is_some()
            && let Err(error) = self.pump_analysis_requests()
            && let Some(token) = self.analysis_runtime.active_token().cloned()
        {
            self.abort_running_analysis(
                &token,
                mirante4d_application::OperationFailureCode::AnalysisExecutionFailed,
                &error,
            );
        }

        let ready = self
            .dataset
            .scope_complete(SCOPE_CURRENT_3D, &self.render_runtime.lease_bridge);
        self.update_dataset_fidelity(ready);
        if completion_drain_needs_replan(
            installed,
            drained,
            self.dataset.dispatcher().admission_blocked(),
        ) {
            self.render_runtime.lod_replan_pending = true;
            self.render_runtime.frame_fidelity.frame_time_ms =
                Some(started.elapsed().as_secs_f64() * 1_000.0);
            ctx.request_repaint();
        } else if analysis_changed || drained > 0 || self.dataset.dispatcher().has_pending_work() {
            ctx.request_repaint();
        }
    }

    fn plan_cross_section_requirements(
        &self,
        snapshot: &mirante4d_application::ApplicationSnapshot,
        scale: mirante4d_domain::ScaleLevel,
        four_panel: bool,
        aggregate: AggregateDemandSeed<'_>,
    ) -> anyhow::Result<Vec<(u64, Vec<DatasetResourceKey>)>> {
        let panels = [
            (SCOPE_CROSS_SECTION_XY, crate::viewer_layout::PanelId::Xy),
            (SCOPE_CROSS_SECTION_XZ, crate::viewer_layout::PanelId::Xz),
            (SCOPE_CROSS_SECTION_YZ, crate::viewer_layout::PanelId::Yz),
        ];
        if !four_panel {
            return Ok(panels
                .into_iter()
                .map(|(scope, _)| (scope, Vec::new()))
                .collect());
        }

        let presentations = panels.map(|(scope, panel_id)| {
            (
                scope,
                panel_id,
                self.render_runtime
                    .cross_section_runtime
                    .panel(panel_id)
                    .and_then(|panel| panel.presentation_viewport),
            )
        });
        let mut remaining_panels = presentations
            .iter()
            .filter(|(_, _, presentation)| presentation.is_some())
            .count();
        let mut union = aggregate.current.iter().copied().collect::<BTreeSet<_>>();
        if union.len() != aggregate.current.len() {
            anyhow::bail!("current semantic demand contains duplicate resources");
        }
        let mut union_decoded_bytes = aggregate.current_decoded_bytes;
        extend_planned_union(
            snapshot.catalog(),
            &mut union,
            &mut union_decoded_bytes,
            aggregate.playback,
        )?;

        let mut planned = Vec::with_capacity(presentations.len());
        for (scope, panel_id, presentation) in presentations {
            let Some(presentation) = presentation else {
                planned.push((scope, Vec::new()));
                continue;
            };
            let limits = DatasetDemandPlanLimits::new(
                SEMANTIC_PLAN_CANDIDATES_PER_LAYER,
                MAX_RENDER_REQUIREMENTS
                    .saturating_sub(union.len())
                    .checked_div(remaining_panels)
                    .unwrap_or(0),
                aggregate
                    .decoded_capacity
                    .saturating_sub(union_decoded_bytes)
                    .checked_div(remaining_panels as u64)
                    .unwrap_or(0),
            );
            let panel_plan = plan_cross_section_panel(
                snapshot.catalog(),
                application_view(snapshot),
                panel_id
                    .cross_section_panel()
                    .expect("a cross-section scope has a cross-section panel"),
                presentation,
                scale,
                limits,
            )?;
            extend_planned_union(
                snapshot.catalog(),
                &mut union,
                &mut union_decoded_bytes,
                panel_plan.resources.iter(),
            )?;
            planned.push((scope, panel_plan.resources));
            remaining_panels -= 1;
        }
        Ok(planned)
    }

    fn update_dataset_fidelity(&mut self, ready: bool) {
        let snapshot = self.application.snapshot();
        let view = application_view(&snapshot);
        let status = self.render_runtime.lease_bridge.cohort_status(
            snapshot.catalog().scientific_identity().resource_identity(),
            view.active_layer(),
            view.timepoint(),
            self.dataset.current_scale(),
        );
        self.render_runtime.frame_fidelity.resident_bricks = status.retained;
        self.render_runtime.frame_fidelity.missing_occupied_bricks = status.missing;
        self.render_runtime.frame_fidelity.cpu_cache_bytes = self
            .dataset
            .dispatcher()
            .diagnostics()
            .map(|diagnostics| diagnostics.category_used_bytes(CpuLedgerCategory::DecodedResidency))
            .unwrap_or(0);
        let empty = self.dataset.scope_is_empty(SCOPE_CURRENT_3D);
        self.render_runtime.frame_fidelity.completeness = if empty || ready {
            FrameCompleteness::Complete
        } else {
            FrameCompleteness::Loading
        };
        self.render_runtime.frame_fidelity.backend = if empty {
            RenderBackend::Empty
        } else if ready {
            RenderBackend::GpuResidentBricks
        } else {
            RenderBackend::Loading
        };
        self.render_runtime.lod_schedule.displayed_scale_level =
            (empty || ready).then_some(self.dataset.current_scale().get());
        self.render_runtime.frame_fidelity.displayed_scale_level =
            self.render_runtime.lod_schedule.displayed_scale_level;
        if empty {
            self.render_runtime.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
        }
    }

    fn mark_cross_section_plan_failure(&mut self, error: &anyhow::Error) {
        let capacity = error
            .downcast_ref::<DatasetDemandPlanCapacityError>()
            .is_some();
        tracing::warn!(%error, "cross-section demand planning failed");
        for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let generation = self
                .render_runtime
                .cross_section_runtime
                .panel(panel_id)
                .map_or(0, |panel| panel.generation);
            let mut schedule = CrossSectionPanelScheduleState::missing_viewport(generation);
            schedule.status = if capacity {
                CrossSectionPanelScheduleStatus::BudgetLimited
            } else {
                CrossSectionPanelScheduleStatus::Unavailable
            };
            schedule.reason = if capacity {
                CrossSectionPanelScheduleReason::PlanningBudgetExceeded
            } else {
                CrossSectionPanelScheduleReason::PlanningFailed
            };
            self.render_runtime
                .cross_section_runtime
                .set_panel_schedule(panel_id, schedule);
        }
    }

    pub(crate) fn record_dataset_fault(&mut self, fault: &RuntimeFault) {
        if runtime_fault_invalidates_verified_source(fault.code()) {
            let snapshot = self.application.snapshot();
            if snapshot.catalog().scientific_identity().is_verified()
                && let Err(application_fault) = crate::current_egui_shell_bridge::dispatch(
                    &mut self.application,
                    ApplicationCommand::InvalidateSourceVerification {
                        source_generation: snapshot.source_generation(),
                    },
                )
            {
                tracing::warn!(
                    ?application_fault,
                    "observed source fault could not invalidate the verified binding"
                );
            }
        }
        let message = fault.to_string();
        self.dataset.record_plan_error(message.clone());
        self.render_runtime.visible_brick_plan_error = Some(message.clone());
        self.render_runtime.frame_fidelity.last_capacity_error = Some(message);
        self.render_runtime.frame_fidelity.completeness = FrameCompleteness::Incomplete;
    }

    fn record_dataset_plan_error(&mut self, error: &anyhow::Error) {
        let message = error.to_string();
        let capacity = error
            .downcast_ref::<DatasetDemandPlanCapacityError>()
            .is_some();
        self.dataset.record_plan_error(message.clone());
        self.render_runtime.visible_brick_plan_error = Some(message.clone());
        self.render_runtime.frame_fidelity.last_failure_kind = Some(if capacity {
            FrameFailureKind::BudgetExceeded
        } else {
            FrameFailureKind::InvalidModeParameter
        });
        self.render_runtime.frame_fidelity.last_capacity_error = Some(message);
        self.render_runtime.frame_fidelity.completeness = if capacity {
            FrameCompleteness::BudgetLimited
        } else {
            FrameCompleteness::Incomplete
        };
        self.render_runtime.frame_fidelity.reason = if capacity {
            LodDecisionReason::CpuBudgetLimited
        } else {
            LodDecisionReason::BackendLimit
        };
    }
}

pub(crate) const fn runtime_fault_invalidates_verified_source(code: RuntimeFaultCode) -> bool {
    matches!(
        code,
        RuntimeFaultCode::SourceRejected
            | RuntimeFaultCode::CorruptResource
            | RuntimeFaultCode::UnsupportedResource
            | RuntimeFaultCode::DecodeFailed
    )
}

fn budget_share_usize(total: usize, numerator: usize, denominator: usize) -> usize {
    total.checked_div(denominator).unwrap_or(0) * numerator
}

const fn completion_drain_needs_replan(
    installed: bool,
    drained: usize,
    admission_blocked: bool,
) -> bool {
    installed || (drained > 0 && admission_blocked)
}

fn budget_share_u64(total: u64, numerator: usize, denominator: usize) -> u64 {
    total.checked_div(denominator as u64).unwrap_or(0) * numerator as u64
}

fn playback_requirements(
    snapshot: &mirante4d_application::ApplicationSnapshot,
    current: &[DatasetResourceKey],
) -> Vec<DatasetResourceKey> {
    if !snapshot.transient().playback_active() {
        return Vec::new();
    }
    let view = application_view(snapshot);
    let timepoints = snapshot
        .catalog()
        .layers()
        .map(|layer| layer.shape().t())
        .min()
        .unwrap_or(1);
    if timepoints <= 1 {
        return Vec::new();
    }
    let next = TimeIndex::new((view.timepoint().get() + 1) % timepoints);
    current
        .iter()
        .map(|key| resource_at_timepoint(*key, next))
        .collect()
}

fn extend_planned_union<'a>(
    catalog: &DatasetCatalog,
    union: &mut BTreeSet<DatasetResourceKey>,
    decoded_bytes: &mut u64,
    resources: impl IntoIterator<Item = &'a DatasetResourceKey>,
) -> anyhow::Result<()> {
    for resource in resources {
        if union.insert(*resource) {
            *decoded_bytes = decoded_bytes
                .checked_add(catalog.resource_payload_descriptor(*resource)?.byte_len())
                .ok_or_else(|| anyhow::anyhow!("planned decoded-byte union overflows"))?;
        }
    }
    Ok(())
}

fn resource_at_timepoint(key: DatasetResourceKey, timepoint: TimeIndex) -> DatasetResourceKey {
    DatasetResourceKey::new(
        key.identity(),
        key.layer(),
        timepoint,
        key.scale(),
        key.region(),
    )
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset_runtime::RuntimeFaultCode;

    use super::{completion_drain_needs_replan, runtime_fault_invalidates_verified_source};

    #[test]
    fn draining_cancelled_backlog_retries_queue_blocked_demand() {
        assert!(completion_drain_needs_replan(false, 1, true));
        assert!(!completion_drain_needs_replan(false, 1, false));
        assert!(completion_drain_needs_replan(true, 1, false));
    }

    #[test]
    fn only_observed_source_integrity_faults_invalidate_a_verified_binding() {
        for code in [
            RuntimeFaultCode::SourceRejected,
            RuntimeFaultCode::CorruptResource,
            RuntimeFaultCode::UnsupportedResource,
            RuntimeFaultCode::DecodeFailed,
        ] {
            assert!(runtime_fault_invalidates_verified_source(code));
        }
        for code in [
            RuntimeFaultCode::QueueFull,
            RuntimeFaultCode::Cancelled,
            RuntimeFaultCode::ShuttingDown,
            RuntimeFaultCode::InvariantViolation,
        ] {
            assert!(!runtime_fault_invalidates_verified_source(code));
        }
    }
}
