use std::time::Duration;

#[cfg(test)]
use crate::LodDecisionReason;

pub(crate) const PLAYBACK_FRAME_INTERVAL: Duration = Duration::from_millis(42);

pub(crate) fn playback_tick_for_ui_time(time_seconds: f64) -> u64 {
    if !time_seconds.is_finite() || time_seconds <= 0.0 {
        return 0;
    }
    (time_seconds / PLAYBACK_FRAME_INTERVAL.as_secs_f64()).floor() as u64
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
}
