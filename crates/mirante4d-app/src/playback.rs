use std::time::{Duration, Instant};

use mirante4d_core::TimeIndex;

use crate::LodDecisionReason;

const PLAYBACK_FRAME_INTERVAL: Duration = Duration::from_millis(42);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackState {
    pub(crate) playing: bool,
    pub(crate) frame_interval: Duration,
    pub(crate) last_step_at: Option<Instant>,
    pub(crate) waiting_for_timepoint: Option<TimeIndex>,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: false,
            frame_interval: PLAYBACK_FRAME_INTERVAL,
            last_step_at: None,
            waiting_for_timepoint: None,
        }
    }
}

pub(crate) fn stepped_timepoint(current: TimeIndex, count: u64, delta: i64) -> TimeIndex {
    if count == 0 {
        return TimeIndex(0);
    }
    let count_i128 = i128::from(count);
    let current_i128 = i128::from(current.0.min(count - 1));
    let delta_i128 = i128::from(delta);
    let wrapped = (current_i128 + delta_i128).rem_euclid(count_i128);
    TimeIndex(wrapped as u64)
}

pub(crate) fn playback_effective_lod_target(
    normal_target_scale_level: u32,
    scale_count: usize,
    playback_active: bool,
) -> (u32, LodDecisionReason) {
    if playback_active && normal_target_scale_level == 0 && scale_count > 1 {
        (1, LodDecisionReason::PlaybackDownshift)
    } else if normal_target_scale_level == 0 {
        (0, LodDecisionReason::ExactS0)
    } else {
        (
            normal_target_scale_level,
            LodDecisionReason::ScreenEquivalentCoarserScale,
        )
    }
}

pub(crate) fn playback_status_label(
    playback: PlaybackState,
    active: TimeIndex,
    count: u64,
) -> String {
    if count <= 1 {
        return "playback stopped | t 1/1".to_owned();
    }
    let state = if playback.playing {
        "playing"
    } else {
        "stopped"
    };
    format!("playback {state} | t {}/{}", active.0 + 1, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_playback_state_is_stopped_with_frame_interval() {
        let playback = PlaybackState::default();

        assert!(!playback.playing);
        assert_eq!(playback.frame_interval, PLAYBACK_FRAME_INTERVAL);
        assert_eq!(playback.last_step_at, None);
        assert_eq!(playback.waiting_for_timepoint, None);
    }

    #[test]
    fn stepped_timepoint_wraps_forward_backward_and_large_delta() {
        assert_eq!(stepped_timepoint(TimeIndex(0), 0, 1), TimeIndex(0));
        assert_eq!(stepped_timepoint(TimeIndex(0), 3, -1), TimeIndex(2));
        assert_eq!(stepped_timepoint(TimeIndex(2), 3, 1), TimeIndex(0));
        assert_eq!(stepped_timepoint(TimeIndex(1), 3, 5), TimeIndex(0));
        assert_eq!(stepped_timepoint(TimeIndex(7), 3, 1), TimeIndex(0));
    }

    #[test]
    fn playback_effective_lod_target_downshifts_only_normal_s0_when_possible() {
        assert_eq!(
            playback_effective_lod_target(0, 2, true),
            (1, LodDecisionReason::PlaybackDownshift)
        );
        assert_eq!(
            playback_effective_lod_target(0, 1, true),
            (0, LodDecisionReason::ExactS0)
        );
        assert_eq!(
            playback_effective_lod_target(1, 2, true),
            (1, LodDecisionReason::ScreenEquivalentCoarserScale)
        );
        assert_eq!(
            playback_effective_lod_target(0, 2, false),
            (0, LodDecisionReason::ExactS0)
        );
    }

    #[test]
    fn playback_status_label_reports_playing_stopped_and_single_timepoint() {
        let mut playback = PlaybackState::default();

        assert_eq!(
            playback_status_label(playback, TimeIndex(0), 3),
            "playback stopped | t 1/3"
        );

        playback.playing = true;
        assert_eq!(
            playback_status_label(playback, TimeIndex(1), 3),
            "playback playing | t 2/3"
        );
        assert_eq!(
            playback_status_label(playback, TimeIndex(0), 1),
            "playback stopped | t 1/1"
        );
    }
}
