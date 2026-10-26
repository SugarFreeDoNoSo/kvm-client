//! Frame progress, independent of the GUI and of RPC channel readiness.
use std::time::{Duration, Instant};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(20);
const STALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoState {
    Waiting,
    Playing,
    NoSignal,
    Stalled,
}

impl VideoState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Playing => "playing",
            Self::NoSignal => "no_signal",
            Self::Stalled => "stalled",
        }
    }
}

pub struct FrameHealth {
    waiting_since: Instant,
    source_ready: Option<bool>,
    counts: (u64, u64),
    decoded_at: Option<Instant>,
    presented_at: Option<Instant>,
}

impl FrameHealth {
    pub fn new(now: Instant) -> Self {
        Self {
            waiting_since: now,
            source_ready: None,
            counts: (0, 0),
            decoded_at: None,
            presented_at: None,
        }
    }

    pub fn source_ready(&mut self, ready: bool, now: Instant) {
        if self.source_ready != Some(ready) {
            self.waiting_since = now;
            self.decoded_at = None;
            self.presented_at = None;
        }
        self.source_ready = Some(ready);
    }

    /// Counts are cumulative decoded and sink-rendered frames, never FPS samples.
    pub fn observe(&mut self, now: Instant, counts: (u64, u64)) -> VideoState {
        if counts.0 != self.counts.0 {
            self.decoded_at = Some(now);
        }
        if counts.1 != self.counts.1 {
            self.presented_at = Some(now);
        }
        self.counts = counts;
        if self.source_ready == Some(false) {
            return VideoState::NoSignal;
        }
        match (self.decoded_at, self.presented_at) {
            (Some(decoded), Some(presented)) => {
                if now.duration_since(decoded) >= STALL_TIMEOUT
                    || now.duration_since(presented) >= STALL_TIMEOUT
                {
                    VideoState::Stalled
                } else {
                    VideoState::Playing
                }
            }
            _ if now.duration_since(self.waiting_since) >= FIRST_FRAME_TIMEOUT => {
                VideoState::Stalled
            }
            _ => VideoState::Waiting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_without_frames_does_not_mean_video_is_playing() {
        let now = Instant::now();
        let mut health = FrameHealth::new(now);
        assert_eq!(health.observe(now, (0, 0)), VideoState::Waiting);
        assert_eq!(
            health.observe(now + FIRST_FRAME_TIMEOUT, (0, 0)),
            VideoState::Stalled
        );
    }

    #[test]
    fn both_decode_and_presentation_must_progress() {
        let now = Instant::now();
        let mut health = FrameHealth::new(now);
        assert_eq!(health.observe(now, (1, 0)), VideoState::Waiting);
        assert_eq!(health.observe(now, (1, 1)), VideoState::Playing);
        assert_eq!(
            health.observe(now + STALL_TIMEOUT, (20, 1)),
            VideoState::Stalled
        );
        assert_eq!(
            health.observe(now + STALL_TIMEOUT, (20, 2)),
            VideoState::Playing
        );
    }

    #[test]
    fn no_signal_keeps_control_available_without_a_video_timeout() {
        let now = Instant::now();
        let mut health = FrameHealth::new(now);
        health.source_ready(false, now);
        assert_eq!(
            health.observe(now + Duration::from_secs(60), (0, 0)),
            VideoState::NoSignal
        );
        health.source_ready(true, now + Duration::from_secs(61));
        assert_eq!(
            health.observe(now + Duration::from_secs(61), (0, 0)),
            VideoState::Waiting
        );
    }

    #[test]
    fn repeated_hdmi_ready_does_not_restart_first_frame_timeout() {
        let now = Instant::now();
        let mut health = FrameHealth::new(now);
        health.source_ready(true, now);
        health.source_ready(true, now + Duration::from_secs(19));
        assert_eq!(
            health.observe(now + FIRST_FRAME_TIMEOUT, (0, 0)),
            VideoState::Stalled
        );
    }

    #[test]
    fn a_frozen_decode_is_not_hidden_by_presentation_redraws() {
        let now = Instant::now();
        let mut health = FrameHealth::new(now);
        assert_eq!(health.observe(now, (1, 1)), VideoState::Playing);
        assert_eq!(
            health.observe(now + STALL_TIMEOUT, (1, 50)),
            VideoState::Stalled
        );
    }
}
