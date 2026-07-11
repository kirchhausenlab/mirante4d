use std::time::{Duration, Instant};

use mirante4d_renderer::gpu::GpuRenderer;

use crate::brick_streaming::{
    BrickSubmissionOptions, apply_brick_read_outcome, current_resident_frame_ready,
    submit_visible_bricks_to_pool, submit_visible_bricks_to_pool_with_options,
};
use crate::{
    AppState, create_brick_read_pool, layer_state::activate_streaming_timepoint_preserving_frame,
    render_state::rerender_state_with_backend, render_state_from_resident_bricks_with_backend,
    stepped_timepoint, viewport::resident_brick_render_supported,
};

pub fn render_first_streamed_frame_for_smoke(
    state: &mut AppState,
    gpu_renderer: Option<&GpuRenderer>,
    timeout: Duration,
) -> anyhow::Result<()> {
    rerender_state_with_backend(state, gpu_renderer)?;
    if state.active_volume_u8.is_some()
        || state.active_volume.is_some()
        || state.active_volume_f32.is_some()
    {
        return Ok(());
    }
    if !resident_brick_render_supported(state.active_render_mode) {
        anyhow::bail!(
            "smoke streaming render does not support {:?}",
            state.active_render_mode
        );
    }
    let pool = create_brick_read_pool(state)
        .ok_or_else(|| anyhow::anyhow!("failed to create smoke brick worker pool"))?;
    submit_visible_bricks_to_pool(state, &pool);
    let deadline = Instant::now() + timeout;
    loop {
        if current_resident_frame_ready(state) {
            render_current_resident_frame_for_smoke(state, gpu_renderer, timeout)?;
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "timed out waiting for streamed frame: {}/{} current bricks",
                state.brick_stream_completed,
                state.brick_stream_requested
            );
        }
        let remaining = deadline.saturating_duration_since(now);
        if let Some(outcome) = pool.recv_timeout(remaining.min(Duration::from_millis(250))) {
            apply_brick_read_outcome(state, outcome);
        }
    }
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

pub fn render_playback_steps_for_smoke(
    state: &mut AppState,
    gpu_renderer: Option<&GpuRenderer>,
    steps: usize,
    timeout_per_step: Duration,
) -> anyhow::Result<Vec<PlaybackSmokeFrame>> {
    render_first_streamed_frame_for_smoke(state, gpu_renderer, timeout_per_step)?;
    if state.timepoint_count <= 1 || steps == 0 {
        return Ok(Vec::new());
    }
    if state.diagnostics.nonzero_pixels == 0 {
        anyhow::bail!("playback smoke cannot start from a blank rendered frame");
    }
    let pool = create_brick_read_pool(state)
        .ok_or_else(|| anyhow::anyhow!("failed to create playback smoke brick worker pool"))?;
    let mut frames = Vec::with_capacity(steps);
    for _ in 0..steps {
        let target = stepped_timepoint(state.active_timepoint, state.timepoint_count, 1);
        let previous_pixels = state.frame.pixels().to_vec();
        if !previous_pixels.iter().any(|value| *value > 0) {
            anyhow::bail!(
                "playback smoke reached a blank frame before requesting timepoint {}",
                target.0
            );
        }
        activate_streaming_timepoint_preserving_frame(state, target)?;
        if state.frame.pixels() != previous_pixels.as_slice() {
            anyhow::bail!(
                "playback smoke did not preserve the displayed frame while loading timepoint {}",
                target.0
            );
        }

        let started = Instant::now();
        submit_visible_bricks_to_pool_with_options(state, &pool, BrickSubmissionOptions::PLAYBACK);
        let deadline = started + timeout_per_step;
        loop {
            if current_resident_frame_ready(state) {
                render_current_resident_frame_for_smoke(state, gpu_renderer, timeout_per_step)?;
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                anyhow::bail!(
                    "timed out waiting for playback timepoint {}: {}/{} current bricks",
                    target.0,
                    state.brick_stream_completed,
                    state.brick_stream_requested
                );
            }
            let remaining = deadline.saturating_duration_since(now);
            if let Some(outcome) = pool.recv_timeout(remaining.min(Duration::from_millis(250))) {
                apply_brick_read_outcome(state, outcome);
            }
        }
        if state.active_timepoint != target {
            anyhow::bail!(
                "playback smoke rendered timepoint {}, expected {}",
                state.active_timepoint.0,
                target.0
            );
        }
        if state.diagnostics.nonzero_pixels == 0 {
            anyhow::bail!("playback smoke rendered blank timepoint {}", target.0);
        }
        frames.push(PlaybackSmokeFrame {
            timepoint: target.0,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            nonzero_pixels: state.diagnostics.nonzero_pixels,
            max_value: state.diagnostics.max_value,
            displayed_scale_level: state.frame_fidelity.displayed_scale_level.unwrap_or(0),
            target_scale_level: state.frame_fidelity.target_scale_level,
        });
    }
    Ok(frames)
}

fn render_current_resident_frame_for_smoke(
    state: &mut AppState,
    gpu_renderer: Option<&GpuRenderer>,
    _timeout: Duration,
) -> anyhow::Result<()> {
    render_state_from_resident_bricks_with_backend(state, gpu_renderer)
}
