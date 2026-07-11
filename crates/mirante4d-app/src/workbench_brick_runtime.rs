use std::time::{Duration, Instant};

use eframe::egui;

use crate::{
    MiranteWorkbenchApp,
    brick_streaming::{
        BrickSubmissionOptions, apply_brick_read_outcomes, cancel_brick_tickets,
        current_resident_frame_ready, reset_warm_state, submit_visible_bricks_to_pool_with_options,
    },
    cross_section_streaming::apply_cross_section_brick_read_outcomes,
    display_refresh::duration_ms,
    lod_scheduler::update_visible_brick_plan,
    viewer_layout::ViewerLayout,
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
        let submission = match &self.brick_read_pool {
            Some(pool) => {
                let options = if self.playback.playing {
                    BrickSubmissionOptions::PLAYBACK
                } else if self.state.viewer_layout.layout() == ViewerLayout::FourPanel {
                    BrickSubmissionOptions::CURRENT_ONLY
                } else {
                    BrickSubmissionOptions::DEFAULT
                };
                submit_visible_bricks_to_pool_with_options(&mut self.state, pool, options)
            }
            None => {
                self.state.brick_stream_last_error = Some("brick worker unavailable".to_owned());
                return VisibleBrickRequestOutcome::default();
            }
        };
        if submission.current_changed {
            self.cancel_runtime_brick_tickets();
            self.current_brick_tickets = submission.current_tickets;
            self.prefetch_brick_tickets = submission.prefetch_tickets;
            self.warm_brick_tickets = submission.warm_tickets;
        }
        let current_frame_ready = current_resident_frame_ready(&self.state);
        if (submission.resident_changed || submission.current_changed) && current_frame_ready {
            self.state.lod_replan_pending = true;
        }
        VisibleBrickRequestOutcome {
            current_changed: submission.current_changed,
            resident_changed: submission.resident_changed,
            current_frame_ready,
        }
    }

    pub(crate) fn cancel_runtime_brick_tickets(&mut self) {
        cancel_brick_tickets(&mut self.current_brick_tickets);
        cancel_brick_tickets(&mut self.prefetch_brick_tickets);
        cancel_brick_tickets(&mut self.warm_brick_tickets);
    }

    pub(crate) fn enter_playback_streaming_mode(&mut self, _ctx: &egui::Context) {
        cancel_brick_tickets(&mut self.warm_brick_tickets);
        reset_warm_state(&mut self.state);
        update_visible_brick_plan(&mut self.state);
        self.state.brick_stream_request_key = None;
        self.request_visible_bricks();
    }

    pub(crate) fn exit_playback_streaming_mode(&mut self, ctx: &egui::Context) {
        update_visible_brick_plan(&mut self.state);
        self.state.brick_stream_request_key = None;
        self.request_visible_bricks();
        ctx.request_repaint();
    }

    pub(crate) fn drain_brick_results(&mut self, ctx: &egui::Context) {
        if self.brick_read_pool.is_none() && self.cross_section_read_pool.is_none() {
            self.state.brick_result_drain_limit = BRICK_RESULT_DRAIN_LIMIT;
            self.state.brick_result_drain_time_budget_ms =
                duration_ms(BRICK_RESULT_DRAIN_TIME_BUDGET);
            self.state.brick_result_drain_last_count = 0;
            self.state.brick_result_drain_last_budget_limited = false;
            self.state.brick_result_drain_last_repaint_reason = None;
            return;
        };
        let drain_started = Instant::now();
        let mut brick_outcomes = Vec::new();
        let mut cross_section_outcomes = Vec::new();
        while brick_outcomes
            .len()
            .saturating_add(cross_section_outcomes.len())
            < BRICK_RESULT_DRAIN_LIMIT
            && drain_started.elapsed() < BRICK_RESULT_DRAIN_TIME_BUDGET
        {
            if let Some(pool) = &self.cross_section_read_pool
                && let Some(outcome) = pool.try_recv()
            {
                cross_section_outcomes.push(outcome);
                continue;
            }
            if let Some(pool) = &self.brick_read_pool
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
        self.state.brick_result_drain_limit = BRICK_RESULT_DRAIN_LIMIT;
        self.state.brick_result_drain_time_budget_ms = duration_ms(BRICK_RESULT_DRAIN_TIME_BUDGET);
        self.state.brick_result_drain_last_count = drained_count;
        self.state.brick_result_drain_last_budget_limited = drain_limited;
        self.state.brick_result_drain_total_drained = self
            .state
            .brick_result_drain_total_drained
            .saturating_add(drained_count as u64);
        if drain_limited {
            self.state.brick_result_drain_budget_hit_count = self
                .state
                .brick_result_drain_budget_hit_count
                .saturating_add(1);
        }
        let cross_section_partition =
            apply_cross_section_brick_read_outcomes(&mut self.state, cross_section_outcomes);
        let brick_cross_section_partition =
            apply_cross_section_brick_read_outcomes(&mut self.state, brick_outcomes);
        let changed =
            apply_brick_read_outcomes(&mut self.state, brick_cross_section_partition.unhandled);
        let cross_section_resident_changed = cross_section_partition.resident_changed
            || brick_cross_section_partition.resident_changed;
        if changed
            && current_resident_frame_ready(&self.state)
            && resident_brick_render_supported(self.state.active_render_mode)
        {
            self.state.lod_replan_pending = true;
            self.state.brick_result_drain_last_repaint_reason =
                Some("resident_frame_pending".to_owned());
            ctx.request_repaint();
        } else if cross_section_resident_changed {
            self.state.brick_result_drain_last_repaint_reason =
                Some("cross_section_panel_resident_pending".to_owned());
            ctx.request_repaint();
        } else if drain_limited {
            self.state.brick_result_drain_last_repaint_reason =
                Some("drain_budget_limited".to_owned());
            ctx.request_repaint();
        } else {
            self.state.brick_result_drain_last_repaint_reason = None;
        }
    }
}
