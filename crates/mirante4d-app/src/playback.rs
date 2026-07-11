use std::time::Duration;

use mirante4d_domain::TimeIndex;

#[cfg(test)]
use crate::LodDecisionReason;

pub(crate) const PLAYBACK_FRAME_INTERVAL: Duration = Duration::from_millis(42);

pub(crate) fn playback_tick_for_ui_time(time_seconds: f64) -> u64 {
    if !time_seconds.is_finite() || time_seconds <= 0.0 {
        return 0;
    }
    (time_seconds / PLAYBACK_FRAME_INTERVAL.as_secs_f64()).floor() as u64
}

pub(crate) fn stepped_timepoint(current: TimeIndex, count: u64, delta: i64) -> TimeIndex {
    if count == 0 {
        return TimeIndex::new(0);
    }
    let count_i128 = i128::from(count);
    let current_i128 = i128::from(current.get().min(count - 1));
    let delta_i128 = i128::from(delta);
    let wrapped = (current_i128 + delta_i128).rem_euclid(count_i128);
    TimeIndex::new(wrapped as u64)
}

#[cfg(test)]
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

pub(crate) fn playback_status_label(playing: bool, active: TimeIndex, count: u64) -> String {
    if count <= 1 {
        return "playback stopped | t 1/1".to_owned();
    }
    let state = if playing { "playing" } else { "stopped" };
    format!("playback {state} | t {}/{}", active.get() + 1, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_time_maps_to_stable_playback_ticks() {
        assert_eq!(playback_tick_for_ui_time(f64::NAN), 0);
        assert_eq!(playback_tick_for_ui_time(-1.0), 0);
        assert_eq!(playback_tick_for_ui_time(0.0), 0);
        assert_eq!(playback_tick_for_ui_time(0.041), 0);
        assert_eq!(playback_tick_for_ui_time(0.042), 1);
        assert_eq!(playback_tick_for_ui_time(0.084), 2);
    }

    #[test]
    fn stepped_timepoint_wraps_forward_backward_and_large_delta() {
        assert_eq!(
            stepped_timepoint(TimeIndex::new(0), 0, 1),
            TimeIndex::new(0)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(0), 3, -1),
            TimeIndex::new(2)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(2), 3, 1),
            TimeIndex::new(0)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(1), 3, 5),
            TimeIndex::new(0)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(7), 3, 1),
            TimeIndex::new(0)
        );
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
        assert_eq!(
            playback_status_label(false, TimeIndex::new(0), 3),
            "playback stopped | t 1/3"
        );

        assert_eq!(
            playback_status_label(true, TimeIndex::new(1), 3),
            "playback playing | t 2/3"
        );
        assert_eq!(
            playback_status_label(true, TimeIndex::new(0), 1),
            "playback stopped | t 1/1"
        );
    }
}
