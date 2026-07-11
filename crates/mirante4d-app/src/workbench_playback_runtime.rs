use std::time::Instant;

use eframe::egui;

use crate::{
    BACKGROUND_WORK_REPAINT_INTERVAL, MiranteWorkbenchApp,
    brick_streaming::{brick_runtime_work_active, playback_timepoint_finished_loading},
    commands::WorkbenchCommand,
    cross_section_scheduler::cross_section_refinement_work_pending,
    cross_section_streaming::cross_section_runtime_work_active,
};

impl MiranteWorkbenchApp {
    pub(crate) fn background_work_active(&self) -> bool {
        self.tiff_import_setup_task.is_some()
            || self.import_task.is_some()
            || self.analysis_task.is_some()
            || self.playback.playing
            || brick_runtime_work_active(&self.state)
            || cross_section_runtime_work_active(&self.state)
            || cross_section_refinement_work_pending(&self.state)
    }

    pub(crate) fn enqueue_playback_command_if_due(
        &mut self,
        commands: &mut Vec<WorkbenchCommand>,
        ctx: &egui::Context,
    ) {
        if !self.playback.playing {
            return;
        }
        if self.state.timepoint_count <= 1 {
            self.playback.playing = false;
            self.playback.last_step_at = None;
            self.playback.waiting_for_timepoint = None;
            self.state.playback_lod_downshift_active = false;
            return;
        }
        let now = Instant::now();
        if let Some(timepoint) = self.playback.waiting_for_timepoint {
            if self.state.active_timepoint != timepoint
                || playback_timepoint_finished_loading(&self.state, timepoint)
            {
                self.playback.waiting_for_timepoint = None;
                self.playback.last_step_at = Some(now);
            } else {
                ctx.request_repaint_after(BACKGROUND_WORK_REPAINT_INTERVAL);
                return;
            }
        }
        let Some(last_step_at) = self.playback.last_step_at else {
            self.playback.last_step_at = Some(now);
            ctx.request_repaint_after(self.playback.frame_interval);
            return;
        };
        let elapsed = now.saturating_duration_since(last_step_at);
        if elapsed >= self.playback.frame_interval {
            commands.push(WorkbenchCommand::StepTimepoint { delta: 1 });
            self.playback.last_step_at = Some(now);
            ctx.request_repaint_after(self.playback.frame_interval);
        } else {
            ctx.request_repaint_after(self.playback.frame_interval - elapsed);
        }
    }
}
