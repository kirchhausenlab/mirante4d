use std::time::{Duration, Instant};

use eframe::egui;
use mirante4d_domain::ViewerLayout;

use crate::{
    MiranteWorkbenchApp,
    brick_streaming::{
        BrickSubmissionOptions, BrickSubmissionResult, apply_brick_read_outcomes,
        cancel_brick_tickets, current_resident_frame_ready,
        submit_visible_bricks_to_pool_with_options, view_for_snapshot,
    },
    cross_section_streaming::apply_cross_section_brick_read_outcomes,
    current_runtime::dataset::CurrentDatasetRuntime,
    display_refresh::duration_ms,
    viewport::resident_brick_render_supported,
};

#[cfg(not(test))]
const BRICK_RESULT_DRAIN_LIMIT: usize = 32;
#[cfg(test)]
const BRICK_RESULT_DRAIN_LIMIT: usize = 2;
const BRICK_RESULT_DRAIN_TIME_BUDGET: Duration = Duration::from_millis(8);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleBrickRequestOutcome {
    pub(crate) current_changed: bool,
    pub(crate) resident_changed: bool,
    pub(crate) current_frame_ready: bool,
}

impl MiranteWorkbenchApp {
    pub(crate) fn request_visible_bricks(&mut self) -> VisibleBrickRequestOutcome {
        let snapshot = self.application.snapshot();
        let view = view_for_snapshot(&snapshot);
        let options = if snapshot.transient().playback_active() {
            BrickSubmissionOptions::PLAYBACK
        } else if view.layout() == ViewerLayout::FourPanel {
            BrickSubmissionOptions::CURRENT_ONLY
        } else {
            BrickSubmissionOptions::DEFAULT
        };
        let Some(pool) = self.dataset_runtime.brick_read_pool.take() else {
            self.dataset_runtime.brick_stream_last_error =
                Some("brick worker unavailable".to_owned());
            return VisibleBrickRequestOutcome::default();
        };
        let submission = submit_visible_bricks_to_pool_with_options(
            &snapshot,
            &mut self.dataset_runtime,
            &mut self.analysis_runtime,
            &self.render_runtime,
            &pool,
            options,
        );
        self.dataset_runtime.brick_read_pool = Some(pool);

        let current_changed = submission.current_changed;
        let resident_changed = submission.resident_changed;
        if current_changed {
            self.cancel_runtime_brick_tickets();
            install_submission_tickets(&mut self.dataset_runtime, submission);
        }
        let current_frame_ready =
            current_resident_frame_ready(&snapshot, &self.dataset_runtime, &self.render_runtime);
        if (resident_changed || current_changed) && current_frame_ready {
            self.render_runtime.lod_replan_pending = true;
        }
        VisibleBrickRequestOutcome {
            current_changed,
            resident_changed,
            current_frame_ready,
        }
    }

    fn cancel_runtime_brick_tickets(&mut self) {
        cancel_brick_tickets(&mut self.dataset_runtime.current_brick_tickets);
        cancel_brick_tickets(&mut self.dataset_runtime.prefetch_brick_tickets);
        cancel_brick_tickets(&mut self.dataset_runtime.warm_brick_tickets);
    }

    pub(crate) fn drain_brick_results(&mut self, ctx: &egui::Context) {
        if self.dataset_runtime.brick_read_pool.is_none()
            && self.dataset_runtime.cross_section_read_pool.is_none()
        {
            record_drain_state(&mut self.dataset_runtime, 0, false, None);
            return;
        }

        let snapshot = self.application.snapshot();
        let view = view_for_snapshot(&snapshot);
        let drain_started = Instant::now();
        let mut brick_outcomes = Vec::new();
        let mut cross_section_outcomes = Vec::new();
        while brick_outcomes
            .len()
            .saturating_add(cross_section_outcomes.len())
            < BRICK_RESULT_DRAIN_LIMIT
            && drain_started.elapsed() < BRICK_RESULT_DRAIN_TIME_BUDGET
        {
            if let Some(pool) = self.dataset_runtime.cross_section_read_pool.as_ref()
                && let Some(outcome) = pool.try_recv()
            {
                cross_section_outcomes.push(outcome);
                continue;
            }
            if let Some(pool) = self.dataset_runtime.brick_read_pool.as_ref()
                && let Some(outcome) = pool.try_recv()
            {
                brick_outcomes.push(outcome);
                continue;
            }
            break;
        }

        let drained_count = brick_outcomes
            .len()
            .saturating_add(cross_section_outcomes.len());
        let drain_limited = drained_count == BRICK_RESULT_DRAIN_LIMIT
            || drain_started.elapsed() >= BRICK_RESULT_DRAIN_TIME_BUDGET;
        record_drain_state(
            &mut self.dataset_runtime,
            drained_count,
            drain_limited,
            None,
        );

        let cross_section_partition = apply_cross_section_brick_read_outcomes(
            &self.dataset_runtime,
            &mut self.render_runtime,
            view,
            cross_section_outcomes,
        );
        let brick_cross_section_partition = apply_cross_section_brick_read_outcomes(
            &self.dataset_runtime,
            &mut self.render_runtime,
            view,
            brick_outcomes,
        );
        let changed = apply_brick_read_outcomes(
            &snapshot,
            &mut self.dataset_runtime,
            &mut self.analysis_runtime,
            &self.render_runtime,
            brick_cross_section_partition.unhandled,
        );
        let cross_section_resident_changed = cross_section_partition.resident_changed
            || brick_cross_section_partition.resident_changed;
        if cross_section_resident_changed {
            self.render_runtime
                .cross_section_runtime
                .clear_render_failures_after_residency_change();
        }
        let active_mode = view
            .layer(view.active_layer())
            .map(|layer| layer.render_state().mode());

        if changed
            && current_resident_frame_ready(&snapshot, &self.dataset_runtime, &self.render_runtime)
            && active_mode.is_some_and(resident_brick_render_supported)
        {
            self.render_runtime.lod_replan_pending = true;
            self.dataset_runtime.brick_result_drain_last_repaint_reason =
                Some("resident_frame_pending".to_owned());
            ctx.request_repaint();
        } else if cross_section_resident_changed {
            self.dataset_runtime.brick_result_drain_last_repaint_reason =
                Some("cross_section_panel_resident_pending".to_owned());
            ctx.request_repaint();
        } else if drain_limited {
            self.dataset_runtime.brick_result_drain_last_repaint_reason =
                Some("drain_budget_limited".to_owned());
            ctx.request_repaint();
        }
    }
}

fn install_submission_tickets(
    dataset: &mut CurrentDatasetRuntime,
    submission: BrickSubmissionResult,
) {
    dataset.current_brick_tickets = submission.current_tickets;
    dataset.prefetch_brick_tickets = submission.prefetch_tickets;
    dataset.warm_brick_tickets = submission.warm_tickets;
}

fn record_drain_state(
    dataset: &mut CurrentDatasetRuntime,
    drained_count: usize,
    drain_limited: bool,
    repaint_reason: Option<String>,
) {
    dataset.brick_result_drain_limit = BRICK_RESULT_DRAIN_LIMIT;
    dataset.brick_result_drain_time_budget_ms = duration_ms(BRICK_RESULT_DRAIN_TIME_BUDGET);
    dataset.brick_result_drain_last_count = drained_count;
    dataset.brick_result_drain_last_budget_limited = drain_limited;
    dataset.brick_result_drain_last_repaint_reason = repaint_reason;
    dataset.brick_result_drain_total_drained = dataset
        .brick_result_drain_total_drained
        .saturating_add(drained_count as u64);
    if drain_limited {
        dataset.brick_result_drain_budget_hit_count = dataset
            .brick_result_drain_budget_hit_count
            .saturating_add(1);
    }
}
