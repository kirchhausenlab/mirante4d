use std::time::Duration;

use mirante4d_application::PlaybackFps;

pub(crate) fn playback_frame_interval(fps: PlaybackFps) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.get()))
}

pub(crate) fn playback_tick_for_ui_time(time_seconds: f64, fps: PlaybackFps) -> u64 {
    if !time_seconds.is_finite() || time_seconds <= 0.0 {
        return 0;
    }
    (time_seconds * f64::from(fps.get())).floor() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_time_maps_to_stable_playback_ticks() {
        let fps = PlaybackFps::new(24).unwrap();
        assert_eq!(playback_tick_for_ui_time(f64::NAN, fps), 0);
        assert_eq!(playback_tick_for_ui_time(-1.0, fps), 0);
        assert_eq!(playback_tick_for_ui_time(0.0, fps), 0);
        assert_eq!(playback_tick_for_ui_time(0.041, fps), 0);
        assert_eq!(playback_tick_for_ui_time(0.042, fps), 1);
        assert_eq!(playback_tick_for_ui_time(0.084, fps), 2);
        assert_eq!(
            playback_frame_interval(fps),
            Duration::from_secs_f64(1.0 / 24.0)
        );

        let slow = PlaybackFps::new(1).unwrap();
        assert_eq!(playback_tick_for_ui_time(0.999, slow), 0);
        assert_eq!(playback_tick_for_ui_time(1.0, slow), 1);
        assert_eq!(playback_frame_interval(slow), Duration::from_secs(1));
    }
}
