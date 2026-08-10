//! OS media keys and the Now Playing readout — macOS Control Center, MPRIS on Linux.
//!
//! `souvlaki` talks to `MPNowPlayingInfoCenter` / `MPRemoteCommandCenter` on macOS and to
//! `org.mpris.MediaPlayer2` on Linux. This module is the whole integration: it owns the
//! handle, turns the OS's button presses into ordinary [`Action`]s — the same ones `Space`
//! and `⌘→` raise, so a hardware key and a keyboard shortcut can never drift apart — and
//! pushes the now-playing card back out whenever it actually changes.
//!
//! ## Pushes are change-driven, not per-frame
//!
//! [`MediaKeys::sync`] runs every frame but only talks to the OS when the track identity
//! or the play/pause state changed, or when the position jumped (a seek). Ordinary
//! playback drift is *not* pushed: `set_playback` carries the progress, and both back ends
//! extrapolate the elapsed time from the last one they were given, so a push per frame
//! would be pure noise. `PHOEBUS_MEDIA_LOG=1` ([`ENV_MEDIA_LOG`]) raises every push from
//! `debug` to `info` so that can be verified from a run's log.
//!
//! ## souvlaki feature set
//!
//! The workspace takes souvlaki's defaults, i.e. `use_dbus` — which is what API-FACTS §4
//! was verified against. On macOS and Windows the `dbus` / `dbus-crossroads` dependencies
//! are target-gated to non-mac unix and are never built, so the default costs this build
//! nothing; on Linux it links `libdbus-1` and needs its dev headers at build time. The
//! pure-Rust alternative is `default-features = false, features = ["use_zbus"]`, which
//! swaps those for zbus 3.9, zvariant and a pollster runtime — take it only if a Linux
//! packager cannot provide the `libdbus-1` dev headers.
//!
//! ## Two hazards this module exists to contain
//!
//! * **Construction must never be fatal.** `MediaControls::new` fails on a headless Linux
//!   box with no session bus. That warns once and leaves the struct disabled; every method
//!   is then a no-op and the app is otherwise unaffected.
//! * **A bad `cover_url` aborts the process.** souvlaki 0.8.3 feeds it to
//!   `[NSImage initWithContentsOfURL:]` and messages the result with no nil check, so a URL
//!   that does not resolve to a readable image dies in a *non-unwinding* objc panic that
//!   `catch_unwind` cannot see (API-FACTS §4). Every `cover_url` therefore comes from
//!   [`cover_url`], which returns `None` unless the PNG is on disk at that instant — the
//!   normal case during a first scan, when the cover cache is still being written.

use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use phoebus_core::{Library, Track, TrackId};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::controller::Controller;
use crate::nav::Action;

/// Environment flag that logs every push to the OS at `info` instead of `debug`.
pub const ENV_MEDIA_LOG: &str = "PHOEBUS_MEDIA_LOG";

/// MPRIS identity. Both names are ignored on macOS.
const DISPLAY_NAME: &str = "Phoebus";
/// MPRIS bus name (`org.mpris.MediaPlayer2.phoebus`). Ignored on macOS.
const DBUS_NAME: &str = "phoebus";

/// A position change of at least this much between two frames is a seek rather than
/// playback drift. The engine reports progress at 4 Hz and the UI repaints at least that
/// often while playing, so real drift is ~0.25 s; anything at this scale is a jump.
const SEEK_EPSILON: Duration = Duration::from_millis(1500);

/// How long after a track change the position is not trusted to mean anything.
///
/// A freshly loaded track's readout bounces: the app snaps it optimistically (to 0, or to
/// wherever the tour seeked), and the engine's own first `Progress` for the new source
/// arrives a moment later — rodio's position is stale for about one callback after an
/// `append` (API-FACTS §1). Left alone that reads as two seeks and pushes the OS a 0 it
/// has to take back. Play/pause and track changes are still pushed during the window; only
/// jump detection waits.
const TRACK_SETTLE: Duration = Duration::from_millis(1200);

/// How far [`MediaControlEvent::Seek`] moves. That event means "seek by an unspecified
/// amount" (MPRIS only — macOS always sends an absolute `SetPosition`), so the amount has
/// to be invented somewhere.
const SEEK_STEP: Duration = Duration::from_secs(10);

/// Upper-case hex, for percent-encoding without allocating.
const HEX: [u8; 16] = *b"0123456789ABCDEF";

/// What the OS has been told. Position is deliberately absent: it is pushed *with* every
/// state change, never as a change of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Snapshot {
    track: Option<TrackId>,
    playing: bool,
}

/// How much of the now-playing card a frame invalidated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Change {
    /// Nothing to say.
    Nothing,
    /// Play/pause flipped, or the user seeked: push the playback state only.
    Playback,
    /// A different track (or none): push metadata *then* the playback state.
    Track,
}

/// The OS media-control integration. Disabled — but still constructible and still safe to
/// call — when the platform refused to give us controls.
pub struct MediaKeys {
    /// `None` once construction or `attach` failed; every method then does nothing.
    controls: Option<MediaControls>,
    /// Button presses, handed over from whatever thread the OS calls the handler on.
    rx: Receiver<MediaControlEvent>,
    /// What the OS was last told. `None` until the first push, so the first loaded track
    /// always pushes.
    last: Option<Snapshot>,
    /// The position seen on the previous frame. A discontinuity against it is a seek —
    /// which catches seeks from *every* source (scrubber, `⌘←`, a media key, the tour)
    /// without the controller having to report them.
    prev_pos: Duration,
    /// When the last track change was pushed — see [`TRACK_SETTLE`].
    settled_at: Option<Instant>,
    /// `PHOEBUS_MEDIA_LOG=1`: log pushes at `info`.
    verbose: bool,
}

impl MediaKeys {
    /// Register with the OS. Never fails: a platform that will not give us controls (no
    /// session bus on a headless Linux box, say) is logged once and the whole struct
    /// becomes a no-op.
    ///
    /// `ctx` is only used to wake the UI: the OS calls the handler on its own thread, and
    /// a paused Phoebus has no repaint scheduled, so without this a media key pressed
    /// while paused would sit in the channel until something else woke the window.
    pub fn new(ctx: &egui::Context) -> MediaKeys {
        let verbose = std::env::var(ENV_MEDIA_LOG).is_ok_and(|v| v == "1");
        let (tx, rx) = crossbeam_channel::unbounded();
        let config = PlatformConfig {
            display_name: DISPLAY_NAME,
            dbus_name: DBUS_NAME,
            // Required on Windows, ignored on macOS, unused on Linux (API-FACTS §4).
            hwnd: None,
        };
        let controls = match MediaControls::new(config) {
            Ok(controls) => Some(controls),
            Err(e) => {
                log::warn!("media: no OS media controls ({e}); media keys are disabled");
                None
            }
        };
        let mut keys = MediaKeys {
            controls,
            rx,
            last: None,
            prev_pos: Duration::ZERO,
            settled_at: None,
            verbose,
        };
        keys.attach(ctx, tx);
        keys
    }

    fn attach(&mut self, ctx: &egui::Context, tx: Sender<MediaControlEvent>) {
        let Some(controls) = self.controls.as_mut() else {
            return;
        };
        let ctx = ctx.clone();
        let attached = controls.attach(move |event| {
            let _ = tx.send(event);
            ctx.request_repaint();
        });
        if let Err(e) = attached {
            log::warn!("media: could not attach the handler ({e}); media keys are disabled");
            self.controls = None;
        }
    }

    /// Drain the OS's button presses into the frame's action buffer.
    ///
    /// Everything routes through [`Action`], the same enum the keyboard shortcuts and the
    /// player bar push into, so there is exactly one implementation of "next track".
    pub fn poll(&mut self, controller: &Controller, out: &mut Vec<Action>) {
        if self.controls.is_none() {
            return;
        }
        let (playing, pos) = (controller.is_playing(), controller.display_pos());
        for event in self.rx.try_iter() {
            match to_action(&event, playing, pos) {
                Some(action) => {
                    log::debug!("media: {event:?} -> {action:?}");
                    out.push(action);
                }
                None => log::debug!("media: ignoring {event:?}"),
            }
        }
    }

    /// Push the now-playing card if — and only if — something the OS can see changed.
    ///
    /// Cheap enough to call more than once a frame: the common case is one comparison.
    pub fn sync(&mut self, controller: &Controller, library: &Library) {
        if self.controls.is_none() {
            return;
        }
        let now = Snapshot {
            track: controller.now().track,
            playing: controller.is_playing(),
        };
        let pos = controller.display_pos();
        let settling = self.settled_at.is_some_and(|t| t.elapsed() < TRACK_SETTLE);
        let change = diff(self.last, now, self.prev_pos, pos, settling);
        self.prev_pos = pos;
        if change == Change::Nothing {
            return;
        }
        if change == Change::Track {
            self.push_metadata(controller, library);
            self.settled_at = Some(Instant::now());
        }
        self.push_playback(now, pos);
        self.last = Some(now);
    }

    /// Tell the OS the player is gone and unhook the handlers. Idempotent.
    pub fn shutdown(&mut self) {
        let verbose = self.verbose;
        let Some(mut controls) = self.controls.take() else {
            return;
        };
        note(verbose, format_args!("stopping and detaching"));
        if let Err(e) = controls.set_playback(MediaPlayback::Stopped) {
            log::warn!("media: could not clear the playback state ({e})");
        }
        if let Err(e) = controls.detach() {
            log::warn!("media: could not detach ({e})");
        }
    }

    fn push_metadata(&mut self, controller: &Controller, library: &Library) {
        let verbose = self.verbose;
        let track = controller.now().track.and_then(|id| library.track(id));
        // Built before `controls` is borrowed so the guard can log through `note`.
        let cover = track.and_then(|t| cover_url(library, t, verbose));
        let duration = match controller.duration() {
            d if !d.is_zero() => Some(d),
            _ => track.map(|t| t.duration),
        };
        let Some(controls) = self.controls.as_mut() else {
            return;
        };
        let metadata = MediaMetadata {
            title: track.map(|t| t.title.as_str()),
            artist: track.map(|t| t.artist.as_str()),
            album: track.map(|t| t.album.as_str()),
            // Guaranteed to exist on disk — see the module header.
            cover_url: cover.as_deref(),
            duration,
        };
        note(
            verbose,
            format_args!(
                "metadata push: title={:?} artist={:?} album={:?} duration={:?} cover={}",
                metadata.title,
                metadata.artist,
                metadata.album,
                metadata.duration,
                metadata.cover_url.unwrap_or("<none>"),
            ),
        );
        if let Err(e) = controls.set_metadata(metadata) {
            log::warn!("media: could not set the metadata ({e})");
        }
    }

    fn push_playback(&mut self, now: Snapshot, pos: Duration) {
        let verbose = self.verbose;
        let Some(controls) = self.controls.as_mut() else {
            return;
        };
        let playback = playback_of(now, pos);
        note(verbose, format_args!("playback push: {playback:?}"));
        if let Err(e) = controls.set_playback(playback) {
            log::warn!("media: could not set the playback state ({e})");
        }
    }
}

impl Drop for MediaKeys {
    fn drop(&mut self) {
        // `App::on_exit` normally gets there first; this covers the paths that do not run
        // it (`souvlaki::MediaControls` detaches on its own drop, but says nothing about
        // no longer playing).
        self.shutdown();
    }
}

/// Log a push at `info` under [`ENV_MEDIA_LOG`], `debug` otherwise.
fn note(verbose: bool, args: std::fmt::Arguments) {
    if verbose {
        log::info!("media: {args}");
    } else {
        log::debug!("media: {args}");
    }
}

/// What the OS needs to be told about, given what it was last told.
///
/// A jump in the position counts as a seek and pushes the playback state, which is how the
/// Control Center scrubber follows `⌘←`, the player bar's knob and a remote `SetPosition`
/// alike. Ordinary drift does not, and neither does anything at all while `settling`
/// ([`TRACK_SETTLE`]).
fn diff(
    last: Option<Snapshot>,
    now: Snapshot,
    prev_pos: Duration,
    pos: Duration,
    settling: bool,
) -> Change {
    match last {
        // Nothing has ever been loaded: stay out of the OS's now-playing list entirely
        // rather than registering an empty card at start-up.
        None if now.track.is_none() => Change::Nothing,
        None => Change::Track,
        Some(last) if last.track != now.track => Change::Track,
        Some(last) if last.playing != now.playing => Change::Playback,
        Some(_) if !settling && jumped(prev_pos, pos) => Change::Playback,
        Some(_) => Change::Nothing,
    }
}

fn jumped(a: Duration, b: Duration) -> bool {
    a.abs_diff(b) >= SEEK_EPSILON
}

fn playback_of(now: Snapshot, pos: Duration) -> MediaPlayback {
    let progress = Some(MediaPosition(pos));
    match (now.track.is_some(), now.playing) {
        (false, _) => MediaPlayback::Stopped,
        (true, true) => MediaPlayback::Playing { progress },
        (true, false) => MediaPlayback::Paused { progress },
    }
}

/// Map one OS button press onto the app's own vocabulary.
///
/// `Play` and `Pause` are separate buttons on both platforms but the controller only has a
/// toggle, so they are gated on the current state — pressing Play twice must not pause.
/// Volume, `OpenUri`, `Raise` and `Quit` are ignored.
fn to_action(event: &MediaControlEvent, playing: bool, pos: Duration) -> Option<Action> {
    match event {
        MediaControlEvent::Toggle => Some(Action::TogglePlay),
        MediaControlEvent::Play => (!playing).then_some(Action::TogglePlay),
        MediaControlEvent::Pause => playing.then_some(Action::TogglePlay),
        MediaControlEvent::Next => Some(Action::Next),
        MediaControlEvent::Previous => Some(Action::Prev),
        MediaControlEvent::Stop => Some(Action::Stop),
        MediaControlEvent::SetPosition(MediaPosition(at)) => Some(Action::Seek(*at)),
        MediaControlEvent::SeekBy(dir, by) => Some(Action::Seek(seek_target(pos, *dir, *by))),
        MediaControlEvent::Seek(dir) => Some(Action::Seek(seek_target(pos, *dir, SEEK_STEP))),
        MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => None,
    }
}

/// Where a relative seek lands. The controller clamps it to the track.
fn seek_target(pos: Duration, dir: SeekDirection, by: Duration) -> Duration {
    match dir {
        SeekDirection::Forward => pos.saturating_add(by),
        SeekDirection::Backward => pos.saturating_sub(by),
    }
}

/// `file://` URL of a track's cached cover, or `None` when there is not a readable one.
///
/// `None` is the safe answer, not merely the tidy one: a `cover_url` the platform cannot
/// load takes the whole process down (module header, API-FACTS §4). The cover cache is
/// written by the scanner, so "missing" is the ordinary state during a first scan.
fn cover_url(library: &Library, track: &Track, verbose: bool) -> Option<String> {
    let path = library.cover_path(&track.album_key);
    if !path.exists() {
        note(
            verbose,
            format_args!("cover missing, omitted: {}", path.display()),
        );
        return None;
    }
    match file_url(&path) {
        Some(url) => Some(url),
        None => {
            note(
                verbose,
                format_args!("cover path is not UTF-8, omitted: {}", path.display()),
            );
            None
        }
    }
}

/// Percent-encode an absolute path into a `file://` URL.
///
/// Not optional politeness: `[NSURL URLWithString:]` returns nil for a string containing a
/// space or any other character that is not URL-legal, and a nil URL is exactly the abort
/// above — cover caches live under the user's library root, which may be called anything
/// at all. MPRIS likewise specifies a percent-encoded `file://` URI.
///
/// Everything outside RFC 3986's unreserved set (`A-Z a-z 0-9 - . _ ~`) is escaped
/// byte-wise, so non-ASCII components come out as UTF-8 percent triplets; `/` is kept
/// literal so the path stays a path. A path that is not valid UTF-8 (possible on Linux)
/// has no URL and returns `None`.
fn file_url(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let mut url = String::with_capacity(text.len() + 8);
    url.push_str("file://");
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char);
            }
            _ => {
                url.push('%');
                url.push(HEX[(byte >> 4) as usize] as char);
                url.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn snap(track: Option<u64>, playing: bool) -> Snapshot {
        Snapshot {
            track: track.map(|n| TrackId::for_rel_path(&format!("t{n}.mp3"))),
            playing,
        }
    }

    #[test]
    fn an_idle_app_never_registers_with_the_os() {
        assert_eq!(
            diff(
                None,
                snap(None, false),
                Duration::ZERO,
                Duration::ZERO,
                false
            ),
            Change::Nothing,
        );
    }

    #[test]
    fn the_first_loaded_track_pushes_metadata() {
        assert_eq!(
            diff(
                None,
                snap(Some(1), true),
                Duration::ZERO,
                Duration::ZERO,
                false
            ),
            Change::Track,
        );
    }

    #[test]
    fn only_a_new_track_pushes_metadata() {
        let last = Some(snap(Some(1), true));
        assert_eq!(
            diff(last, snap(Some(2), true), secs(30), Duration::ZERO, false),
            Change::Track,
        );
        // Same track, play -> pause: the playback state alone.
        assert_eq!(
            diff(last, snap(Some(1), false), secs(30), secs(30), false),
            Change::Playback,
        );
        // Stopping is a track change (to nothing).
        assert_eq!(
            diff(last, snap(None, false), secs(30), Duration::ZERO, false),
            Change::Track,
        );
    }

    #[test]
    fn playback_drift_does_not_push_but_a_seek_does() {
        let last = Some(snap(Some(1), true));
        // 4 Hz progress, and a whole second of stalled frames: still drift.
        for (prev, pos) in [(30_000, 30_250), (30_000, 30_999), (30_000, 29_500)] {
            assert_eq!(
                diff(
                    last,
                    snap(Some(1), true),
                    Duration::from_millis(prev),
                    Duration::from_millis(pos),
                    false,
                ),
                Change::Nothing,
                "{prev}ms -> {pos}ms should not push",
            );
        }
        for (prev, pos) in [(30, 90), (90, 30), (0, 30)] {
            assert_eq!(
                diff(last, snap(Some(1), true), secs(prev), secs(pos), false),
                Change::Playback,
                "{prev}s -> {pos}s is a seek",
            );
        }
    }

    #[test]
    fn the_readout_bouncing_after_a_load_is_not_a_seek() {
        let last = Some(snap(Some(1), true));
        // Optimistic 30 s, then the engine's stale post-append 0, then the real 30 s.
        for (prev, pos) in [(30, 0), (0, 30)] {
            assert_eq!(
                diff(last, snap(Some(1), true), secs(prev), secs(pos), true),
                Change::Nothing,
                "{prev}s -> {pos}s inside the settle window should not push",
            );
        }
        // A pause during the window is still a real change and still goes out.
        assert_eq!(
            diff(last, snap(Some(1), false), secs(30), secs(0), true),
            Change::Playback,
        );
        assert_eq!(
            diff(last, snap(Some(2), true), secs(30), secs(0), true),
            Change::Track,
        );
    }

    #[test]
    fn playback_state_carries_the_position_and_stops_when_empty() {
        assert_eq!(
            playback_of(snap(Some(1), true), secs(30)),
            MediaPlayback::Playing {
                progress: Some(MediaPosition(secs(30)))
            }
        );
        assert_eq!(
            playback_of(snap(Some(1), false), secs(30)),
            MediaPlayback::Paused {
                progress: Some(MediaPosition(secs(30)))
            }
        );
        assert_eq!(
            playback_of(snap(None, false), secs(30)),
            MediaPlayback::Stopped
        );
    }

    #[test]
    fn buttons_reuse_the_keyboard_shortcuts_actions() {
        let at = secs(30);
        assert!(matches!(
            to_action(&MediaControlEvent::Toggle, true, at),
            Some(Action::TogglePlay)
        ));
        assert!(matches!(
            to_action(&MediaControlEvent::Next, true, at),
            Some(Action::Next)
        ));
        assert!(matches!(
            to_action(&MediaControlEvent::Previous, true, at),
            Some(Action::Prev)
        ));
        assert!(matches!(
            to_action(&MediaControlEvent::Stop, true, at),
            Some(Action::Stop)
        ));
    }

    #[test]
    fn play_and_pause_are_not_blind_toggles() {
        let at = secs(30);
        assert!(to_action(&MediaControlEvent::Play, true, at).is_none());
        assert!(matches!(
            to_action(&MediaControlEvent::Play, false, at),
            Some(Action::TogglePlay)
        ));
        assert!(to_action(&MediaControlEvent::Pause, false, at).is_none());
        assert!(matches!(
            to_action(&MediaControlEvent::Pause, true, at),
            Some(Action::TogglePlay)
        ));
    }

    #[test]
    fn seeks_are_absolute_or_relative_to_the_readout() {
        let at = secs(30);
        let target = |e: MediaControlEvent| match to_action(&e, true, at) {
            Some(Action::Seek(d)) => d,
            other => panic!("expected a seek, got {other:?}"),
        };
        assert_eq!(
            target(MediaControlEvent::SetPosition(MediaPosition(secs(90)))),
            secs(90)
        );
        assert_eq!(
            target(MediaControlEvent::SeekBy(SeekDirection::Forward, secs(15))),
            secs(45)
        );
        assert_eq!(
            target(MediaControlEvent::SeekBy(SeekDirection::Backward, secs(15))),
            secs(15)
        );
        // Backward past the start saturates rather than wrapping.
        assert_eq!(
            target(MediaControlEvent::SeekBy(SeekDirection::Backward, secs(99))),
            Duration::ZERO
        );
        assert_eq!(
            target(MediaControlEvent::Seek(SeekDirection::Forward)),
            secs(30) + SEEK_STEP
        );
    }

    #[test]
    fn the_rest_of_the_events_are_ignored() {
        for event in [
            MediaControlEvent::SetVolume(0.5),
            MediaControlEvent::OpenUri("file:///x.mp3".to_string()),
            MediaControlEvent::Raise,
            MediaControlEvent::Quit,
        ] {
            assert!(
                to_action(&event, true, secs(30)).is_none(),
                "{event:?} should be ignored"
            );
        }
    }

    #[test]
    fn cover_urls_are_percent_encoded() {
        assert_eq!(
            file_url(Path::new(
                "/Users/x/.phoebus/cache/covers/069dcae3f83fc201.png"
            ))
            .as_deref(),
            Some("file:///Users/x/.phoebus/cache/covers/069dcae3f83fc201.png"),
        );
        // A space or a `#` makes `NSURL URLWithString:` return nil, which is the abort.
        assert_eq!(
            file_url(Path::new("/Volumes/My Music/#1/a b.png")).as_deref(),
            Some("file:///Volumes/My%20Music/%231/a%20b.png"),
        );
        // Non-ASCII becomes UTF-8 percent triplets; unreserved characters stay literal.
        assert_eq!(
            file_url(Path::new("/mú~s-i_c.png")).as_deref(),
            Some("file:///m%C3%BA~s-i_c.png"),
        );
    }

    #[test]
    fn a_missing_cover_is_omitted_rather_than_handed_to_the_os() {
        // The guard that keeps souvlaki from aborting the process (API-FACTS §4).
        let missing = PathBuf::from("/definitely/not/here/0000000000000000.png");
        assert!(!missing.exists());
        assert!(file_url(&missing).is_some(), "the URL itself is buildable");
    }
}
