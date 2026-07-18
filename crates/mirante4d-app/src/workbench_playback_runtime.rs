use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, CrossSectionPanelScheduleStatus, OperationKind,
};
use mirante4d_domain::ViewerLayout;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, RenderCoordinationState,
    dataset_requests::{DatasetDemandState, SCOPE_CURRENT_3D},
    import_worker_service::ImportWorkerService,
    native_presentation::{NativePresentationBridge, ProgressiveLeaseProbeState},
    playback::{PLAYBACK_FRAME_INTERVAL, playback_tick_for_ui_time},
    viewer_layout::PanelId,
};

pub(crate) fn background_work_active(
    snapshot: &ApplicationSnapshot,
    import: &ImportWorkerService,
    dataset: &DatasetDemandState,
    render: &RenderCoordinationState,
    presentation: &NativePresentationBridge,
    progressive_render_required: bool,
) -> bool {
    application_service_work_active(snapshot)
        || import.status().is_active()
        || snapshot.transient().playback_active()
        || dataset.dispatcher().has_pending_work()
        || presentation.product_gpu.as_ref().is_some_and(|product| {
            product.targets.values().any(|target| {
                target.pending_capture.is_some() || !target.pending_gpu_timings.is_empty()
            }) || product.staging_3d.as_ref().is_some_and(|target| {
                target.pending_capture.is_some() || !target.pending_gpu_timings.is_empty()
            })
        })
        || progressive_render_required
        || (crate::application_view(snapshot).layout() == ViewerLayout::FourPanel
            && render.iter().any(|(_, panel)| {
                panel.cross_section_schedule().is_some_and(|schedule| {
                    cross_section_schedule_requires_polling(schedule.status)
                })
            }))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressiveRenderSubmissionWork {
    pub(crate) three_d_required: bool,
    pub(crate) any_required: bool,
    #[cfg(test)]
    requirements_visited: usize,
    #[cfg(test)]
    targets_evaluated: usize,
}

/// Renderer-owned progress must not wait for unrelated dataset work to become
/// idle. In particular, a cohort retained during GPU backpressure may be the
/// work that releases CPU-ledger capacity for the still-pending dispatcher.
pub(crate) const fn three_d_progress_refresh_required(
    work: ProgressiveRenderSubmissionWork,
    _dispatcher_has_pending_work: bool,
) -> bool {
    work.three_d_required
}

const fn cross_section_schedule_requires_polling(status: CrossSectionPanelScheduleStatus) -> bool {
    // `Coarse` is a settled, honest display at the selected current scale; no
    // background work can improve it until view/demand changes. Only missing
    // CPU resources require periodic polling.
    matches!(status, CrossSectionPanelScheduleStatus::Loading)
}

/// Returns true only when another renderer execution can admit data that is
/// already CPU-resident. Dataset I/O completion owns refreshes while resources
/// are still pending, so slow I/O cannot cause identical full-frame renders.
pub(crate) fn progressive_render_submission_work(
    dataset: &DatasetDemandState,
    presentation: &NativePresentationBridge,
) -> ProgressiveRenderSubmissionWork {
    let Some(product) = presentation.product_gpu.as_ref() else {
        return ProgressiveRenderSubmissionWork::default();
    };
    let residency_invalidation_epoch = product.renderer.residency_invalidation_epoch();
    let mut work = ProgressiveRenderSubmissionWork::default();
    for panel in [PanelId::ThreeD, PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let Some(target) = product.targets.get(&panel) else {
            continue;
        };
        let Some(request) = target.request.as_ref() else {
            continue;
        };
        #[cfg(test)]
        {
            work.targets_evaluated = work.targets_evaluated.saturating_add(1);
        }
        let limitation = target
            .presented
            .as_ref()
            .and_then(|frame| frame.progress().limitation());
        let presented_matches_request = target.presented.as_ref().is_some_and(|frame| {
            frame.frame() == request.intent.frame() && frame.extent() == request.intent.extent()
        });
        let mut probe_state = target.progressive_lease_probe_state();
        let probe = progressive_render_submission_probe(
            target.lease_priority_keys.as_ref(),
            &mut probe_state,
            target.next_unsatisfied_requirement,
            target.satisfied_requirement_keys.len(),
            |key| target.satisfied_requirement_keys.contains(key),
            target.last_residency_invalidation_epoch == Some(residency_invalidation_epoch),
            presented_matches_request,
            target.last_renderer_available_resources,
            limitation,
            |key| dataset.retained_leases().payload(*key).is_some(),
        );
        target.set_progressive_lease_probe_state(probe_state);
        #[cfg(test)]
        {
            work.requirements_visited = work
                .requirements_visited
                .saturating_add(probe.requirements_visited);
        }
        work.any_required |= probe.required || probe.continuation_required;
        if panel == PanelId::ThreeD {
            work.three_d_required = probe.required;
        }
    }
    if let Some(target) = product
        .staging_3d
        .as_ref()
        .filter(|target| target.request.is_some())
    {
        let request = target
            .request
            .as_ref()
            .expect("the staging target filter checked its request");
        #[cfg(test)]
        {
            work.targets_evaluated = work.targets_evaluated.saturating_add(1);
        }
        let limitation = target
            .presented
            .as_ref()
            .and_then(|frame| frame.progress().limitation());
        let presented_matches_request = target.presented.as_ref().is_some_and(|frame| {
            frame.frame() == request.intent.frame() && frame.extent() == request.intent.extent()
        });
        let mut probe_state = target.progressive_lease_probe_state();
        let probe = progressive_render_submission_probe(
            target.lease_priority_keys.as_ref(),
            &mut probe_state,
            target.next_unsatisfied_requirement,
            target.satisfied_requirement_keys.len(),
            |key| target.satisfied_requirement_keys.contains(key),
            target.last_residency_invalidation_epoch == Some(residency_invalidation_epoch),
            presented_matches_request,
            target.last_renderer_available_resources,
            limitation,
            |key| dataset.retained_leases().payload(*key).is_some(),
        );
        target.set_progressive_lease_probe_state(probe_state);
        #[cfg(test)]
        {
            work.requirements_visited = work
                .requirements_visited
                .saturating_add(probe.requirements_visited);
        }
        work.three_d_required |= probe.required;
        work.any_required |= probe.required || probe.continuation_required;
    }
    work
}

const MAX_PROGRESSIVE_REQUIREMENT_PROBE_VISITS: usize = 1_024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ProgressiveRenderSubmissionProbe {
    required: bool,
    continuation_required: bool,
    requirements_visited: usize,
}

#[allow(clippy::too_many_arguments)]
fn progressive_render_submission_probe<K>(
    requirements: &[K],
    probe_state: &mut ProgressiveLeaseProbeState,
    next_unsatisfied_requirement: usize,
    satisfied_requirement_count: usize,
    mut requirement_is_satisfied: impl FnMut(&K) -> bool,
    residency_epoch_unchanged: bool,
    presented_matches_request: bool,
    renderer_available: u64,
    limitation: Option<mirante4d_render_api::FrameLimitation>,
    mut cpu_lease_available: impl FnMut(&K) -> bool,
) -> ProgressiveRenderSubmissionProbe {
    // Capacity-limited coverage cannot improve without a changed request or
    // configuration. A changed residency epoch needs one renderer turn to
    // reconcile exact target state before the satisfied count is trusted.
    if requirements.is_empty()
        || (presented_matches_request
            && matches!(
                limitation,
                Some(mirante4d_render_api::FrameLimitation::CapacityLimited)
            ))
        || (presented_matches_request
            && satisfied_requirement_count == requirements.len()
            && usize::try_from(renderer_available).is_ok_and(|count| count >= requirements.len()))
    {
        *probe_state = ProgressiveLeaseProbeState::default();
        return ProgressiveRenderSubmissionProbe::default();
    }
    if !residency_epoch_unchanged || satisfied_requirement_count > renderer_available as usize {
        probe_state.render_requested = true;
        return ProgressiveRenderSubmissionProbe {
            required: true,
            continuation_required: false,
            requirements_visited: 0,
        };
    }
    if satisfied_requirement_count == requirements.len() {
        if !presented_matches_request {
            probe_state.render_requested = true;
            return ProgressiveRenderSubmissionProbe {
                required: true,
                continuation_required: false,
                requirements_visited: 0,
            };
        }
        *probe_state = ProgressiveLeaseProbeState::default();
        return ProgressiveRenderSubmissionProbe::default();
    }

    // Only an explicit body/lease/residency event opens this finite pass.
    // Negative UI polls consume at most one body traversal and then remain
    // settled; they never wrap and implicitly reopen themselves.
    if probe_state.requirements_remaining == 0 {
        probe_state.render_requested = false;
        return ProgressiveRenderSubmissionProbe::default();
    }
    probe_state.requirements_remaining = probe_state.requirements_remaining.min(requirements.len());
    if probe_state.next_requirement >= requirements.len() {
        probe_state.next_requirement = next_unsatisfied_requirement % requirements.len();
    }
    probe_state.render_requested = false;
    let visit_limit = probe_state
        .requirements_remaining
        .min(MAX_PROGRESSIVE_REQUIREMENT_PROBE_VISITS);
    for offset in 0..visit_limit {
        let index = probe_state.next_requirement;
        let requirement = &requirements[index];
        if !requirement_is_satisfied(requirement) && cpu_lease_available(requirement) {
            // Retain the positive key at the head until a renderer turn either
            // satisfies it or reports that it remains actionable.
            probe_state.render_requested = true;
            return ProgressiveRenderSubmissionProbe {
                required: true,
                continuation_required: false,
                requirements_visited: offset + 1,
            };
        }
        probe_state.next_requirement = (index + 1) % requirements.len();
        probe_state.requirements_remaining -= 1;
    }
    ProgressiveRenderSubmissionProbe {
        required: false,
        continuation_required: probe_state.requirements_remaining > 0,
        requirements_visited: visit_limit,
    }
}

fn application_service_work_active(snapshot: &ApplicationSnapshot) -> bool {
    // Events emitted after the update's initial service pump still own one
    // subsequent turn. In particular, a terminal source fault can leave the
    // dataset dispatcher idle while its invalidation event is waiting to
    // retire composition-root state.
    snapshot.pending_event_count() != 0
        || pending_application_service_work(
            snapshot
                .active_operations()
                .iter()
                .map(|operation| operation.kind()),
            snapshot.pending_settings_change().is_some(),
        )
}

fn pending_application_service_work(
    operation_kinds: impl IntoIterator<Item = OperationKind>,
    settings_change_pending: bool,
) -> bool {
    settings_change_pending
        || operation_kinds.into_iter().any(|kind| {
            matches!(
                kind,
                OperationKind::DatasetOpen
                    | OperationKind::SourceVerification
                    | OperationKind::ProjectOpen
                    | OperationKind::ProjectSave
                    | OperationKind::Analysis
            )
        })
}

pub(crate) const fn source_verification_polling_required(
    automatic_request_pending: bool,
    worker_active: bool,
) -> bool {
    automatic_request_pending || worker_active
}

pub(crate) fn enqueue_playback_command_if_due(
    snapshot: &ApplicationSnapshot,
    dataset: &DatasetDemandState,
    commands: &mut Vec<ApplicationCommand>,
    ctx: &egui::Context,
) {
    if !snapshot.transient().playback_active() {
        return;
    }

    let timepoint_count = catalog_timepoint_count(snapshot);
    if timepoint_count <= 1 {
        commands.push(ApplicationCommand::SetPlaybackActive(false));
        return;
    }

    if snapshot.transient().last_playback_tick().is_some()
        && !dataset.scope_complete(SCOPE_CURRENT_3D)
    {
        ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
        return;
    }

    let tick = ctx.input(|input| playback_tick_for_ui_time(input.time));
    if snapshot
        .transient()
        .last_playback_tick()
        .is_none_or(|last| tick > last)
    {
        commands.push(ApplicationCommand::AdvancePlaybackTick(tick));
    }
    ctx.request_repaint_after(PLAYBACK_FRAME_INTERVAL);
}

pub(crate) fn catalog_timepoint_count(snapshot: &ApplicationSnapshot) -> u64 {
    snapshot.timepoint_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_owned_progress_is_not_gated_by_pending_dataset_work() {
        let work = ProgressiveRenderSubmissionWork {
            three_d_required: true,
            any_required: true,
            requirements_visited: 0,
            targets_evaluated: 1,
        };

        assert!(three_d_progress_refresh_required(work, true));
        assert!(three_d_progress_refresh_required(work, false));
    }

    fn dirty_probe_state(requirement_count: usize, start: usize) -> ProgressiveLeaseProbeState {
        ProgressiveLeaseProbeState {
            next_requirement: start % requirement_count.max(1),
            requirements_remaining: requirement_count,
            render_requested: false,
        }
    }

    #[test]
    fn settled_and_capacity_limited_frames_stop_while_upload_budget_gets_paced_followup() {
        use mirante4d_render_api::FrameLimitation;

        let requirements = [0_u8, 1, 2, 3];
        let mut probe_state = ProgressiveLeaseProbeState::default();
        assert!(
            !progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                0,
                4,
                |_| true,
                true,
                true,
                4,
                None,
                |_| panic!("a settled target must not probe retained leases"),
            )
            .required
        );
        let mut probe_state = dirty_probe_state(requirements.len(), 2);
        assert!(
            progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                2,
                2,
                |key| *key < 2,
                true,
                true,
                2,
                Some(FrameLimitation::MissingResources),
                |_| true,
            )
            .required
        );
        let mut probe_state = dirty_probe_state(requirements.len(), 2);
        assert!(
            !progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                2,
                2,
                |key| *key < 2,
                true,
                true,
                2,
                Some(FrameLimitation::CapacityLimited),
                |_| true,
            )
            .required
        );
        let mut probe_state = dirty_probe_state(requirements.len(), 2);
        assert!(
            progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                2,
                2,
                |key| *key < 2,
                true,
                true,
                2,
                Some(FrameLimitation::BudgetLimited),
                |_| true,
            )
            .required
        );
    }

    #[test]
    fn settled_65k_targets_and_four_panels_do_constant_work() {
        let requirements = vec![0_u8; 65_536];
        let mut target_calls = 0;
        let mut requirement_visits = 0;
        for _panel in [PanelId::ThreeD, PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            target_calls += 1;
            let mut probe_state = ProgressiveLeaseProbeState::default();
            let probe = progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                0,
                requirements.len(),
                |_| panic!("settled target membership must remain unvisited"),
                true,
                true,
                requirements.len() as u64,
                None,
                |_| panic!("settled target leases must remain unvisited"),
            );
            assert!(!probe.required);
            requirement_visits += probe.requirements_visited;
        }

        assert_eq!(target_calls, 4);
        assert_eq!(requirement_visits, 0);
    }

    #[test]
    fn dormant_prefetch_opens_one_control_submission_then_settles_on_renderer_progress() {
        let requirements = [0_u8, 1, 2];
        let mut probe_state = dirty_probe_state(requirements.len(), 2);

        // The two required resources are already presented. A newly retained
        // dormant guard is the sole actionable key and opens one execution.
        let upload = progressive_render_submission_probe(
            &requirements,
            &mut probe_state,
            2,
            2,
            |key| *key < 2,
            true,
            true,
            2,
            None,
            |key| *key == 2,
        );
        assert!(upload.required);
        assert_eq!(upload.requirements_visited, 1);

        // Its control-only report does not replace PresentedFrame, but its
        // physical renderer-progress scalar includes the guard. UI polling
        // must therefore become idle rather than resubmitting forever.
        let settled = progressive_render_submission_probe(
            &requirements,
            &mut probe_state,
            requirements.len(),
            requirements.len(),
            |_| panic!("settled renderer progress must not rescan membership"),
            true,
            true,
            requirements.len() as u64,
            None,
            |_| panic!("settled renderer progress must not probe leases"),
        );
        assert_eq!(settled, ProgressiveRenderSubmissionProbe::default());
    }

    #[test]
    fn stale_full_renderer_counter_does_not_hide_an_exact_eviction() {
        let requirements = [0_u8, 1, 2];
        let mut probe_state = dirty_probe_state(requirements.len(), 2);

        // Another target's exact eviction delta removes the key from this
        // target's satisfaction set and dirties its finite pass. Its last
        // renderer report can still carry the old full physical count; that
        // stale scalar must not suppress the actionable CPU lease.
        let probe = progressive_render_submission_probe(
            &requirements,
            &mut probe_state,
            2,
            requirements.len() - 1,
            |key| *key < 2,
            true,
            true,
            requirements.len() as u64,
            None,
            |key| *key == 2,
        );

        assert!(probe.required);
        assert_eq!(probe.requirements_visited, 1);
    }

    #[test]
    fn fully_resident_successor_frame_retries_after_renderer_backpressure() {
        let requirements = [0_u8, 1, 2, 3];
        let mut probe_state = ProgressiveLeaseProbeState::default();

        let successor = progressive_render_submission_probe(
            &requirements,
            &mut probe_state,
            0,
            requirements.len(),
            |_| panic!("a fully resident successor must not scan membership"),
            true,
            false,
            requirements.len() as u64,
            None,
            |_| panic!("a fully resident successor must not probe CPU leases"),
        );
        assert!(successor.required);
        assert_eq!(successor.requirements_visited, 0);

        let settled = progressive_render_submission_probe(
            &requirements,
            &mut probe_state,
            0,
            requirements.len(),
            |_| panic!("a settled target must not scan membership"),
            true,
            true,
            requirements.len() as u64,
            None,
            |_| panic!("a settled target must not probe CPU leases"),
        );
        assert_eq!(settled, ProgressiveRenderSubmissionProbe::default());
    }

    #[test]
    fn incomplete_65k_target_consumes_one_negative_pass_then_stays_idle() {
        let requirements = (0_u32..65_536).collect::<Vec<_>>();
        let maximum_scan_turns = requirements
            .len()
            .div_ceil(MAX_PROGRESSIVE_REQUIREMENT_PROBE_VISITS);
        let mut probe_state = dirty_probe_state(requirements.len(), 0);
        let mut scan_turns = 0_usize;
        let mut requirement_visits = 0_usize;
        let mut render_submissions = 0_usize;
        let mut repaint_requests = 0_usize;

        loop {
            let probe = progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                0,
                0,
                |_| false,
                true,
                true,
                0,
                Some(mirante4d_render_api::FrameLimitation::MissingResources),
                |_| false,
            );
            if probe.requirements_visited > 0 {
                scan_turns += 1;
                requirement_visits += probe.requirements_visited;
            }
            render_submissions += usize::from(probe.required);
            let repaint = probe.required || probe.continuation_required;
            repaint_requests += usize::from(repaint);
            if !repaint {
                break;
            }
            assert!(scan_turns <= maximum_scan_turns);
        }

        assert_eq!(scan_turns, maximum_scan_turns);
        assert_eq!(requirement_visits, requirements.len());
        assert_eq!(render_submissions, 0);
        assert_eq!(repaint_requests, maximum_scan_turns - 1);

        for _ in 0..3 {
            let settled = progressive_render_submission_probe(
                &requirements,
                &mut probe_state,
                0,
                0,
                |_| false,
                true,
                true,
                0,
                Some(mirante4d_render_api::FrameLimitation::MissingResources),
                |_| panic!("a completed negative pass must not reopen on UI polling"),
            );
            assert_eq!(settled, ProgressiveRenderSubmissionProbe::default());
        }
    }

    #[test]
    fn only_loading_cross_sections_keep_background_polling() {
        assert!(cross_section_schedule_requires_polling(
            CrossSectionPanelScheduleStatus::Loading,
        ));
        assert!(!cross_section_schedule_requires_polling(
            CrossSectionPanelScheduleStatus::Coarse,
        ));
        assert!(!cross_section_schedule_requires_polling(
            CrossSectionPanelScheduleStatus::Ready,
        ));
    }

    #[test]
    fn source_project_and_analysis_operations_keep_application_services_polling() {
        for kind in [
            OperationKind::DatasetOpen,
            OperationKind::SourceVerification,
            OperationKind::ProjectOpen,
            OperationKind::ProjectSave,
            OperationKind::Analysis,
        ] {
            assert!(pending_application_service_work([kind], false));
        }

        assert!(!pending_application_service_work(
            [OperationKind::Import],
            false,
        ));
    }

    #[test]
    fn pending_settings_keep_application_services_polling_without_an_operation() {
        assert!(pending_application_service_work([], true));
        assert!(!pending_application_service_work([], false));
    }

    #[test]
    fn deferred_or_retiring_source_verification_keeps_ui_polling() {
        assert!(source_verification_polling_required(true, false));
        assert!(source_verification_polling_required(false, true));
        assert!(source_verification_polling_required(true, true));
        assert!(!source_verification_polling_required(false, false));
    }
}
