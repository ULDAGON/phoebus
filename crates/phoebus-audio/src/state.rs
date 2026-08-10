//! The device-free half of the playback engine.
//!
//! Everything in here is pure: it never touches rodio, so it is unit-testable on a
//! machine with no audio device. The engine thread owns one [`EngineState`], feeds it
//! commands and one observation per tick (`player.empty()`), and performs whatever the
//! returned decisions say against the real `rodio::Player`.

use std::time::Duration;

/// A seek target is never allowed to land closer than this to the end of the track.
///
/// rodio's `try_seek` accepts a past-the-end target, returns `Ok(())`, silently drains
/// the queue and then reports the bogus target from `get_pos()` forever after
/// (docs/API-FACTS.md §1).
pub(crate) const SEEK_END_GUARD: Duration = Duration::from_secs(1);

/// What the engine believes the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Nothing loaded (fresh, stopped, failed to load, or the track ended).
    Idle,
    Playing,
    Paused,
}

/// UI volume (0.0..=1.0) with the junk filtered out.
pub(crate) fn clamp_ui_volume(ui_volume: f32) -> f32 {
    if ui_volume.is_nan() {
        0.0
    } else {
        ui_volume.clamp(0.0, 1.0)
    }
}

/// Perceptual volume curve: the amplitude handed to `Player::set_volume` is the square
/// of the UI volume, so the fader feels linear to a human ear.
pub(crate) fn amplitude_for(ui_volume: f32) -> f32 {
    let v = clamp_ui_volume(ui_volume);
    v * v
}

/// Clamp a seek target into the safe range for a track of `duration`.
pub(crate) fn clamp_seek(requested: Duration, duration: Duration) -> Duration {
    requested.min(duration.saturating_sub(SEEK_END_GUARD))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EngineState {
    phase: Phase,
    /// `None` while idle, and also for a loaded track whose decoder would not report a
    /// total duration (seeking is then refused rather than risking the past-end bug).
    duration: Option<Duration>,
    ui_volume: f32,
    /// Ended-detection arming. Set by a successful load, cleared by Stop, by the next
    /// Load, and by emitting `Ended` — `empty()` on its own cannot tell "the track
    /// finished" from "we called stop()".
    ended_armed: bool,
    /// Whether the player has actually been observed non-empty since it was armed.
    /// Guards against reading `empty()` in the gap around a fresh `append`.
    saw_audio: bool,
    /// Generation of the `Load` currently being served. Stamped on every event so the app
    /// can drop what belongs to a track it has already replaced. Survives Stop/Ended: the
    /// engine keeps reporting under the last generation it was given.
    generation: u64,
}

impl EngineState {
    pub(crate) fn new(ui_volume: f32) -> Self {
        Self {
            phase: Phase::Idle,
            duration: None,
            ui_volume: clamp_ui_volume(ui_volume),
            ended_armed: false,
            saw_audio: false,
            generation: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    /// The generation every event is stamped with right now.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// A `Load` has arrived: tear the old track down and adopt the new generation, so
    /// even the `Error` of a failed load is attributed to the load that caused it.
    pub(crate) fn begin_load(&mut self, generation: u64) {
        self.on_stop();
        self.generation = generation;
    }

    /// Whether a track is loaded (playing or paused).
    pub(crate) fn is_loaded(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Whether the loaded track can be seeked at all — false while idle, and false for a
    /// track whose decoder reports no total duration.
    pub(crate) fn seekable(&self) -> bool {
        self.duration.is_some()
    }

    pub(crate) fn amplitude(&self) -> f32 {
        amplitude_for(self.ui_volume)
    }

    /// Store a new UI volume; returns the amplitude to hand to rodio.
    pub(crate) fn set_volume(&mut self, ui_volume: f32) -> f32 {
        self.ui_volume = clamp_ui_volume(ui_volume);
        self.amplitude()
    }

    /// A track was decoded and appended successfully.
    pub(crate) fn on_loaded(&mut self, duration: Option<Duration>, autoplay: bool) {
        self.duration = duration;
        self.phase = if autoplay {
            Phase::Playing
        } else {
            Phase::Paused
        };
        self.ended_armed = true;
        self.saw_audio = false;
    }

    /// Back to idle: Stop, a failed load, or the teardown that precedes a new load.
    /// Disarms ended detection so the drain we are about to cause stays silent.
    pub(crate) fn on_stop(&mut self) {
        self.phase = Phase::Idle;
        self.duration = None;
        self.ended_armed = false;
        self.saw_audio = false;
    }

    /// Returns whether there is anything to resume.
    pub(crate) fn on_play(&mut self) -> bool {
        if self.phase == Phase::Idle {
            return false;
        }
        self.phase = Phase::Playing;
        true
    }

    /// Returns whether there was anything to pause.
    pub(crate) fn on_pause(&mut self) -> bool {
        if self.phase == Phase::Idle {
            return false;
        }
        self.phase = Phase::Paused;
        true
    }

    /// The clamped position to actually seek to, or `None` if seeking is not allowed
    /// right now (nothing loaded, or a track of unknown duration).
    pub(crate) fn seek_target(&self, requested: Duration) -> Option<Duration> {
        if self.phase == Phase::Idle {
            return None;
        }
        Some(clamp_seek(requested, self.duration?))
    }

    /// One observation of `player.empty()`. Returns `true` exactly once per track that
    /// drains on its own, i.e. when `Ended` should be emitted.
    pub(crate) fn observe_queue(&mut self, empty: bool) -> bool {
        if !self.ended_armed {
            return false;
        }
        if !empty {
            self.saw_audio = true;
            return false;
        }
        if !self.saw_audio {
            // Freshly appended and not yet picked up by the mixer; not an end.
            return false;
        }
        self.on_stop();
        true
    }

    /// Progress events are only interesting while audio is actually moving.
    pub(crate) fn wants_progress(&self) -> bool {
        self.phase == Phase::Playing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Duration = Duration::from_secs(180);

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn volume_curve_is_squared_and_clamped() {
        assert_eq!(amplitude_for(0.0), 0.0);
        assert_eq!(amplitude_for(1.0), 1.0);
        assert_eq!(amplitude_for(0.5), 0.25);
        assert!((amplitude_for(0.25) - 0.0625).abs() < 1e-6);
        assert_eq!(amplitude_for(-3.0), 0.0);
        assert_eq!(amplitude_for(9.0), 1.0);
        assert_eq!(amplitude_for(f32::NAN), 0.0);
        assert_eq!(amplitude_for(f32::INFINITY), 1.0);
        assert_eq!(amplitude_for(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn volume_curve_is_monotonic() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let amp = amplitude_for(i as f32 / 100.0);
            assert!(amp > prev, "not monotonic at {i}");
            prev = amp;
        }
    }

    #[test]
    fn set_volume_stores_clamped_ui_volume() {
        let mut s = EngineState::new(1.0);
        assert_eq!(s.set_volume(0.5), 0.25);
        assert_eq!(s.amplitude(), 0.25);
        assert_eq!(s.set_volume(2.0), 1.0);
        assert_eq!(s.set_volume(-1.0), 0.0);
    }

    #[test]
    fn volume_survives_a_track_switch() {
        let mut s = EngineState::new(1.0);
        s.set_volume(0.5);
        s.on_loaded(Some(D), true);
        assert_eq!(s.amplitude(), 0.25);
        s.on_stop();
        s.on_loaded(Some(D), true);
        assert_eq!(s.amplitude(), 0.25);
    }

    #[test]
    fn seek_is_clamped_away_from_the_end() {
        assert_eq!(clamp_seek(secs(10), D), secs(10));
        assert_eq!(clamp_seek(secs(179), D), secs(179));
        assert_eq!(clamp_seek(D, D), secs(179));
        assert_eq!(clamp_seek(secs(9999), D), secs(179));
        // A track shorter than the guard band can only be seeked to zero.
        assert_eq!(
            clamp_seek(secs(5), Duration::from_millis(400)),
            Duration::ZERO
        );
        assert_eq!(clamp_seek(Duration::ZERO, D), Duration::ZERO);
    }

    #[test]
    fn seek_is_refused_when_idle_or_duration_unknown() {
        let mut s = EngineState::new(1.0);
        assert_eq!(s.seek_target(secs(5)), None);

        s.on_loaded(None, true);
        assert_eq!(
            s.seek_target(secs(5)),
            None,
            "unknown duration cannot be clamped"
        );

        s.on_loaded(Some(D), true);
        assert_eq!(s.seek_target(secs(5)), Some(secs(5)));
        assert_eq!(s.seek_target(secs(500)), Some(secs(179)));

        s.on_stop();
        assert_eq!(s.seek_target(secs(5)), None);
    }

    #[test]
    fn seek_is_allowed_while_paused() {
        let mut s = EngineState::new(1.0);
        s.on_loaded(Some(D), false);
        assert_eq!(s.phase(), Phase::Paused);
        assert_eq!(s.seek_target(secs(5)), Some(secs(5)));
    }

    #[test]
    fn play_pause_transitions() {
        let mut s = EngineState::new(1.0);
        assert!(!s.on_play(), "nothing to play when idle");
        assert!(!s.on_pause(), "nothing to pause when idle");
        assert_eq!(s.phase(), Phase::Idle);

        s.on_loaded(Some(D), false);
        assert_eq!(s.phase(), Phase::Paused);
        assert!(!s.wants_progress());

        assert!(s.on_play());
        assert_eq!(s.phase(), Phase::Playing);
        assert!(s.wants_progress());

        assert!(s.on_pause());
        assert_eq!(s.phase(), Phase::Paused);
        assert!(!s.wants_progress());

        s.on_stop();
        assert_eq!(s.phase(), Phase::Idle);
        assert!(!s.wants_progress());
    }

    #[test]
    fn autoplay_starts_playing() {
        let mut s = EngineState::new(1.0);
        s.on_loaded(Some(D), true);
        assert_eq!(s.phase(), Phase::Playing);
        assert!(s.wants_progress());
    }

    #[test]
    fn ended_fires_once_when_the_queue_drains() {
        let mut s = EngineState::new(1.0);
        s.on_loaded(Some(D), true);

        // The mixer has not picked the source up yet: an empty read is not an end.
        assert!(!s.observe_queue(true));
        assert!(!s.observe_queue(false));
        assert!(!s.observe_queue(false));

        assert!(s.observe_queue(true), "drain after audio -> Ended");
        assert_eq!(s.phase(), Phase::Idle);
        assert!(!s.observe_queue(true), "Ended must not repeat");
        assert!(!s.observe_queue(true));
    }

    #[test]
    fn ended_is_suppressed_after_stop() {
        let mut s = EngineState::new(1.0);
        s.on_loaded(Some(D), true);
        assert!(!s.observe_queue(false));
        s.on_stop();
        // rodio drains asynchronously, so empty() flips a few ticks later.
        assert!(!s.observe_queue(false));
        assert!(!s.observe_queue(true));
        assert!(!s.observe_queue(true));
    }

    #[test]
    fn ended_is_suppressed_across_a_track_switch() {
        let mut s = EngineState::new(1.0);
        s.on_loaded(Some(D), true);
        assert!(!s.observe_queue(false));

        // What the engine does for `Load`: tear down, then arm the new track.
        s.on_stop();
        s.on_loaded(Some(secs(200)), true);
        assert!(
            !s.observe_queue(true),
            "leftover drain of the old track is not an end"
        );
        assert!(!s.observe_queue(false));
        assert!(
            s.observe_queue(true),
            "the new track still reports its own end"
        );
    }

    #[test]
    fn ended_is_suppressed_when_never_armed() {
        let mut s = EngineState::new(1.0);
        assert!(!s.observe_queue(true));
        assert!(!s.observe_queue(false));
        assert!(!s.observe_queue(true));
    }

    #[test]
    fn a_failed_load_leaves_a_sane_idle_state() {
        let mut s = EngineState::new(0.5);
        s.on_loaded(Some(D), true);
        assert!(!s.observe_queue(false));

        s.on_stop(); // what the engine does when decoding fails
        assert_eq!(s.phase(), Phase::Idle);
        assert_eq!(s.seek_target(secs(1)), None);
        assert!(!s.on_play());
        assert!(!s.observe_queue(true));
        assert_eq!(
            s.amplitude(),
            0.25,
            "volume is not disturbed by a failed load"
        );
    }

    #[test]
    fn generation_follows_the_load_that_set_it() {
        let mut s = EngineState::new(1.0);
        assert_eq!(s.generation(), 0, "nothing loaded yet");

        s.begin_load(7);
        // Adopted before the decode can fail, so a failed load's Error carries it too.
        assert_eq!(s.generation(), 7);
        s.on_loaded(Some(D), true);
        assert_eq!(s.generation(), 7);

        // Stop and a natural end keep reporting under the last load's generation; only
        // the next Load moves it on.
        s.on_stop();
        assert_eq!(s.generation(), 7);
        s.begin_load(8);
        assert_eq!(s.generation(), 8);
        s.on_loaded(Some(D), true);
        assert!(!s.observe_queue(false));
        assert!(s.observe_queue(true));
        assert_eq!(s.generation(), 8, "Ended belongs to the track that ended");
    }

    #[test]
    fn seekable_tracks_whether_a_duration_is_known() {
        let mut s = EngineState::new(1.0);
        assert!(!s.seekable(), "nothing loaded is not seekable");
        assert!(!s.is_loaded());

        s.begin_load(1);
        s.on_loaded(Some(D), true);
        assert!(s.is_loaded());
        assert!(s.seekable());
        assert_eq!(s.seek_target(secs(5)), Some(secs(5)));

        // The duration-less file the engine refuses to seek: loaded, plays, not seekable.
        s.begin_load(2);
        s.on_loaded(None, true);
        assert!(s.is_loaded(), "it still plays");
        assert!(!s.seekable());
        assert_eq!(s.seek_target(secs(5)), None);

        s.on_stop();
        assert!(!s.is_loaded());
        assert!(!s.seekable());
    }

    #[test]
    fn ended_track_can_be_replaced() {
        let mut s = EngineState::new(1.0);
        s.on_loaded(Some(D), true);
        assert!(!s.observe_queue(false));
        assert!(s.observe_queue(true));

        s.on_loaded(Some(D), true);
        assert_eq!(s.phase(), Phase::Playing);
        assert!(!s.observe_queue(false));
        assert!(s.observe_queue(true));
    }
}
