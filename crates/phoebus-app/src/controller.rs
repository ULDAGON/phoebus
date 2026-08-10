//! Everything that plays, remembers or persists: the [`PlayQueue`], the audio engine
//! handle, the playlist store, the persisted [`AppState`], and the now-playing readout the
//! player bar draws.
//!
//! Views never call in here directly — the app translates [`Action`]s into these methods,
//! so there is exactly one place where "the user pressed play" turns into a `Load`.

use std::time::{Duration, Instant};

use phoebus_audio::{Event, EventKind, PlayerHandle};
use phoebus_core::{
    AdvanceReason, AppState, Dirs, Favorites, Library, PlayQueue, PlaylistStore, Repeat, ThemeMode,
    TrackId,
};

use crate::nav::{Action, Now};
use crate::theme;

/// Environment flag that starts the app silent (used by automated screenshot runs).
pub const ENV_START_MUTED: &str = "PHOEBUS_START_MUTED";

/// How many broken tracks in a row the controller will skip before giving up.
///
/// Caps both the per-load library-index skip loop and the consecutive-failure counter, so
/// neither can spin for more than a moment.
const MAX_SKIPS: usize = 32;

/// Owns playback and persistence.
pub struct Controller {
    /// Where `state.json` lives. The library root is not this type's business: it never
    /// writes there, and the paths it plays come out of the [`Library`].
    dirs: Dirs,
    player: Option<PlayerHandle>,
    /// Apple-Music queue semantics (context, shuffle, repeat, manual queue).
    pub queue: PlayQueue,
    /// User playlists, backed by `playlists.json`.
    pub playlists: PlaylistStore,
    /// Hearted albums and tracks, backed by `favorites.json` in the same directory and on
    /// the same terms (UI-SPEC v1.3 §Favorites). It lives beside the playlists rather than
    /// in the app because both are "the user's own lists, persisted here" — and because
    /// `Dirs` is already this type's, so nothing else has to know where the file goes.
    pub favorites: Favorites,
    state: AppState,
    /// Live UI volume — equals `state.volume` unless the muted-start flag is on.
    volume: f32,
    /// True while the start-muted flag is still in force (volume 0 is not persisted).
    muted_start: bool,
    /// True while `PHOEBUS_THEME` is in force: the palette still changes, it just never
    /// reaches `state.json` — the same deal [`ENV_START_MUTED`] gets for the volume.
    theme_locked: bool,
    /// The loaded track.
    current: Option<TrackId>,
    /// True while audio is actually running.
    playing: bool,
    /// Playback position, as last reported by the engine (or by a seek).
    pos: Duration,
    /// Duration of the loaded track.
    duration: Duration,
    /// Whether the engine will accept a seek on the loaded track. The engine refuses to
    /// seek a file whose decoder reports no total duration; it says so in `Loaded`.
    seekable: bool,
    /// While the seek knob is held, the position the user is pointing at.
    scrub: Option<Duration>,
    last_live_seek: Option<Instant>,
    save_due: Option<Instant>,
    /// Stamped on every `Load` we send; the engine echoes it on every event it emits for
    /// that track. Events from an older generation are dropped, which is what keeps an
    /// `Ended`/`Progress` for the *outgoing* track from double-advancing the queue or
    /// repainting the incoming track's readout. Bumped by `stop` too, so nothing in
    /// flight can revive a track we just unloaded.
    generation: u64,
    /// Load failures in a row with no successful `Loaded` in between. Counted here rather
    /// than inferred from the queue because with `Repeat::All`/`One` the queue never runs
    /// out, so "advance returned None" is not an escape hatch.
    consecutive_failures: usize,
    /// Why playback stopped, when it stopped for a reason worth telling the user about.
    playback_error: Option<String>,
}

impl Controller {
    /// Wire up the audio engine and adopt the persisted state.
    ///
    /// A failure to open the audio device is logged, not fatal: the UI stays fully usable
    /// and every transport command becomes a no-op.
    ///
    /// `force_muted` starts at volume 0 regardless of the environment; the `--shot` tour
    /// sets it so a screenshot run can never be audible.
    pub fn new(
        dirs: Dirs,
        state: AppState,
        playlists: PlaylistStore,
        force_muted: bool,
    ) -> Controller {
        let player = match PlayerHandle::spawn() {
            Ok(p) => Some(p),
            Err(e) => {
                log::error!("audio: no output device, playback is disabled: {e:#}");
                None
            }
        };
        Controller::with_player(dirs, state, playlists, force_muted, player)
    }

    /// The real constructor. `player` is `None` when there is no audio device — every
    /// transport command then becomes a logged no-op and the UI stays usable.
    fn with_player(
        dirs: Dirs,
        state: AppState,
        playlists: PlaylistStore,
        force_muted: bool,
        player: Option<PlayerHandle>,
    ) -> Controller {
        let mut queue = PlayQueue::new();
        queue.set_shuffle(state.shuffle);
        queue.set_repeat(state.repeat);

        // Read before the struct literal moves `dirs`. Never an error — an unreadable
        // `favorites.json` becomes a read-only store, not a failure to start.
        let favorites = Favorites::load_from(&dirs.favorites_path());
        let muted_start = force_muted || std::env::var(ENV_START_MUTED).is_ok_and(|v| v == "1");
        let volume = if muted_start { 0.0 } else { state.volume };
        if muted_start {
            log::info!("starting at volume 0 (not persisted); see {ENV_START_MUTED}");
        }

        let controller = Controller {
            dirs,
            player,
            queue,
            favorites,
            playlists,
            state,
            volume,
            muted_start,
            theme_locked: theme::env_override().is_some(),
            current: None,
            playing: false,
            pos: Duration::ZERO,
            duration: Duration::ZERO,
            seekable: false,
            scrub: None,
            last_live_seek: None,
            save_due: None,
            generation: 0,
            consecutive_failures: 0,
            playback_error: None,
        };
        controller.send_volume();
        controller
    }

    // ---- readouts --------------------------------------------------------------------

    /// Now-playing summary for the views.
    pub fn now(&self) -> Now {
        Now {
            track: self.current,
            playing: self.playing,
        }
    }

    /// True while audio is running.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Position to draw: the scrub target while dragging, else the engine's position.
    pub fn display_pos(&self) -> Duration {
        self.scrub.unwrap_or(self.pos)
    }

    /// Duration of the loaded track (zero when nothing is loaded).
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Whether the loaded track can be seeked at all.
    ///
    /// False when nothing is loaded, and false for the rare file whose decoder reports no
    /// total duration: the engine refuses to seek those, so the scrubber is drawn dead
    /// rather than pretending to work (`player_bar::BarState::seekable`).
    pub fn seekable(&self) -> bool {
        self.current.is_some() && self.seekable
    }

    /// Why playback stopped, when it stopped for a reason the user should be told about
    /// (a run of unplayable tracks, or the audio engine dying). Cleared by any successful
    /// load and by any transport command the user issues.
    ///
    /// Painted by the player-bar LCD while nothing is loaded.
    pub fn playback_error(&self) -> Option<&str> {
        self.playback_error.as_deref()
    }

    /// UI volume, 0.0..=1.0.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Shuffle state.
    pub fn shuffle(&self) -> bool {
        self.queue.shuffle()
    }

    /// Repeat mode.
    pub fn repeat(&self) -> Repeat {
        self.queue.repeat()
    }

    // ---- engine events ---------------------------------------------------------------

    /// Drain the audio engine's events: position updates, track ends, failures.
    pub fn poll(&mut self, library: &Library) {
        let Some(player) = self.player.as_ref() else {
            return;
        };
        // Sample liveness *before* draining: events already in the channel outlive the
        // thread that sent them, and the last of them usually says why it died.
        let alive = player.is_alive();
        let events: Vec<Event> = player.events().try_iter().collect();
        if !alive {
            self.engine_died(&events);
            return;
        }
        for event in events {
            self.apply_event(library, event);
        }
    }

    /// Handle one engine event, ignoring anything that belongs to a track we replaced.
    fn apply_event(&mut self, library: &Library, event: Event) {
        if event.generation != self.generation {
            log::trace!(
                "audio: dropping {:?} from generation {} (now {})",
                event.kind,
                event.generation,
                self.generation
            );
            return;
        }
        match event.kind {
            EventKind::Loaded { duration, seekable } => {
                self.consecutive_failures = 0;
                self.playback_error = None;
                self.seekable = seekable;
                if !duration.is_zero() {
                    self.duration = duration;
                }
            }
            EventKind::Progress { pos } => {
                if self.scrub.is_none() {
                    self.pos = pos;
                }
            }
            EventKind::Ended => {
                log::debug!("audio: track ended, advancing");
                match self.queue.advance(AdvanceReason::Ended) {
                    Some(_) => self.load_current(library, true),
                    None => self.stop(),
                }
            }
            EventKind::SeekFailed { pos, message } => {
                // The track is fine — only the seek was refused. Snap the readout back to
                // where the engine actually is instead of leaving the optimistic value up.
                log::warn!("audio: {message}; the track keeps playing at {pos:?}");
                self.scrub = None;
                self.last_live_seek = None;
                if self.current.is_some() {
                    self.pos = pos;
                }
            }
            EventKind::Error(message) => self.load_failed(library, message),
        }
    }

    /// A load or decode failed. Skip the track — but count the failures, because with
    /// `Repeat::All`/`One` the queue wraps forever and skipping would never end.
    fn load_failed(&mut self, library: &Library, message: String) {
        self.consecutive_failures += 1;
        let limit = self.skip_limit();
        if self.consecutive_failures >= limit {
            log::error!("audio: {message}; {limit} tracks in a row failed to play, stopping");
            let detail = format!("{limit} tracks in a row failed to play: {message}");
            self.stop();
            self.playback_error = Some(detail);
            return;
        }
        log::warn!("audio: {message}; skipping to the next track");
        // UserNext, not Ended: with Repeat::One an `Ended` would retry the same broken
        // file forever.
        match self.queue.advance(AdvanceReason::UserNext) {
            Some(_) => self.load_current(library, true),
            None => self.stop(),
        }
    }

    /// How many failures in a row to tolerate: one pass over everything queued, so a
    /// single bad file is skipped but a dead volume cannot loop, capped at [`MAX_SKIPS`].
    fn skip_limit(&self) -> usize {
        let queued = self.queue.context().len() + self.queue.manual_len();
        queued.clamp(1, MAX_SKIPS)
    }

    /// The engine thread is gone (device lost, or rodio panicked inside `catch_unwind`).
    /// Stop pretending to play: the UI would otherwise sit at a frozen 0:00 forever,
    /// repainting at 4 Hz, with every transport button a silent no-op.
    fn engine_died(&mut self, tail: &[Event]) {
        let reason = tail
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::Error(message) => Some(message.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "the audio engine thread exited".to_string());
        log::error!("audio: {reason}; playback is disabled for the rest of this session");
        // Dropping the handle joins the finished thread and turns every later command into
        // the same harmless no-op as the "no output device" path.
        self.player = None;
        self.stop();
        self.playback_error = Some(format!("audio stopped: {reason}"));
    }

    // ---- transport -------------------------------------------------------------------

    /// Make `tracks` the context and start playing at the track the user pointed at
    /// (`index`) — a double-clicked row, or the screenshot tour's deliberate track 1.
    ///
    /// The named track plays first even with shuffle on; when nobody named one, call
    /// [`Controller::play_collection`] instead.
    pub fn play_context(&mut self, library: &Library, tracks: Vec<TrackId>, index: usize) {
        if tracks.is_empty() {
            return;
        }
        self.forgive_failures();
        self.queue.set_context(tracks, index);
        self.load_current(library, true);
    }

    /// Play a whole album / playlist / result list with no particular start track named:
    /// the `▶ PLAY` buttons, the grid card's hover badge and the context menu's `Play`.
    ///
    /// PLAY means "this collection, from the top": it CLEARS shuffle (the player-bar
    /// toggle follows) and plays linearly from track 1. Shuffle survives only a
    /// double-clicked row (the user named a song inside a shuffled session) or the
    /// explicit `SHUFFLE` button.
    pub fn play_collection(&mut self, library: &Library, tracks: Vec<TrackId>) {
        if tracks.is_empty() {
            return;
        }
        self.set_shuffle(false);
        self.play_context(library, tracks, 0);
    }

    /// The `SHUFFLE` button: turn shuffle on and play `tracks` in a uniformly random order,
    /// re-rolled on every press, with no track pinned to the front.
    pub fn shuffle_context(&mut self, library: &Library, tracks: Vec<TrackId>) {
        if tracks.is_empty() {
            return;
        }
        self.forgive_failures();
        self.queue.shuffle_play(tracks);
        self.state.shuffle = self.queue.shuffle();
        self.mark_dirty();
        self.load_current(library, true);
    }

    /// Play / pause. Does nothing when nothing is loaded.
    pub fn toggle_play(&mut self) {
        if self.current.is_none() {
            return;
        }
        self.playing = !self.playing;
        let result = match (&self.player, self.playing) {
            (Some(p), true) => p.play(),
            (Some(p), false) => p.pause(),
            (None, _) => Ok(()),
        };
        if let Err(e) = result {
            log::warn!("audio: {e:#}");
        }
    }

    /// Skip forward.
    pub fn next(&mut self, library: &Library) {
        self.forgive_failures();
        match self.queue.advance(AdvanceReason::UserNext) {
            Some(_) => self.load_current(library, true),
            None => self.stop(),
        }
    }

    /// Skip back: restart the track when more than three seconds have elapsed, otherwise
    /// step to the previous one (and restart if there is no history).
    pub fn prev(&mut self, library: &Library) {
        self.forgive_failures();
        let elapsed = self.pos.as_secs_f32();
        if self.current.is_some() && elapsed > theme::PREV_RESTART_SECS {
            self.seek(Duration::ZERO);
            return;
        }
        match self.queue.previous() {
            Some(_) => self.load_current(library, true),
            None => {
                if self.current.is_some() {
                    self.seek(Duration::ZERO);
                }
            }
        }
    }

    /// Seek to `target` (clamped to the track), ending any scrub.
    pub fn seek(&mut self, target: Duration) {
        self.scrub = None;
        if self.current.is_none() {
            return;
        }
        let target = clamp_seek(target, self.duration);
        if self.seekable {
            // Optimistic snap so the readout does not lag the click. If the engine ends up
            // refusing the seek anyway, its `SeekFailed` puts the real position back.
            self.pos = target;
        }
        if let Some(p) = &self.player
            && let Err(e) = p.seek_to(target)
        {
            log::warn!("audio: seek failed: {e:#}");
        }
    }

    /// Live seek while the knob is held: the readout follows the pointer every frame, the
    /// engine is only told twice a second.
    ///
    /// A track the engine will not seek gets no phantom readout at all — dragging it does
    /// nothing, which is the truth.
    pub fn seek_live(&mut self, target: Duration) {
        if self.current.is_none() || !self.seekable {
            return;
        }
        let target = clamp_seek(target, self.duration);
        self.scrub = Some(target);
        let now = Instant::now();
        let due = self
            .last_live_seek
            .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(theme::LIVE_SEEK_MS));
        if !due {
            return;
        }
        self.last_live_seek = Some(now);
        if let Some(p) = &self.player
            && let Err(e) = p.seek_to(target)
        {
            log::warn!("audio: live seek failed: {e:#}");
        }
    }

    /// Play the `idx`-th row of the Up Next drawer, consuming everything above it.
    pub fn queue_jump(&mut self, library: &Library, idx: usize) {
        self.forgive_failures();
        if self.queue.jump_to_upcoming(idx).is_some() {
            self.load_current(library, true);
        }
    }

    /// Pause without touching the position — used by the `--shot` tour, which wants a
    /// loaded-but-silent player bar.
    pub fn pause(&mut self) {
        if !self.playing {
            return;
        }
        self.playing = false;
        if let Some(p) = &self.player
            && let Err(e) = p.pause()
        {
            log::warn!("audio: {e:#}");
        }
    }

    /// Unload everything and go idle.
    pub fn stop(&mut self) {
        self.playing = false;
        self.current = None;
        self.pos = Duration::ZERO;
        self.duration = Duration::ZERO;
        self.seekable = false;
        self.scrub = None;
        self.playback_error = None;
        // Nothing is current any more: retire the generation so a `Progress` or `Ended`
        // still in flight for the track we just unloaded cannot revive it.
        self.generation += 1;
        if let Some(p) = &self.player
            && let Err(e) = p.stop()
        {
            log::warn!("audio: {e:#}");
        }
    }

    // ---- modes and volume ------------------------------------------------------------

    /// Set the UI volume and persist it (this also cancels a muted start).
    pub fn set_volume(&mut self, volume: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if (volume - self.volume).abs() < f32::EPSILON && !self.muted_start {
            return;
        }
        self.volume = volume;
        self.muted_start = false;
        self.state.volume = volume;
        self.mark_dirty();
        self.send_volume();
    }

    /// Nudge the volume by `delta`.
    pub fn volume_by(&mut self, delta: f32) {
        self.set_volume(self.volume + delta);
    }

    /// Toggle shuffle and persist it.
    pub fn set_shuffle(&mut self, on: bool) {
        self.queue.set_shuffle(on);
        self.state.shuffle = self.queue.shuffle();
        self.mark_dirty();
    }

    /// Toggle shuffle and persist it.
    pub fn toggle_shuffle(&mut self) {
        self.set_shuffle(!self.queue.shuffle());
    }

    /// Cycle repeat Off → All → One and persist it.
    pub fn cycle_repeat(&mut self) {
        self.queue.cycle_repeat();
        self.state.repeat = self.queue.repeat();
        self.mark_dirty();
    }

    /// Remember the palette the user picked.
    ///
    /// Painting is not this type's business — [`crate::app::Phoebus::set_theme`] publishes
    /// the palette and rebuilds egui's style; this only decides whether the choice survives
    /// the run. It does not while `PHOEBUS_THEME` is set: a screenshot run must be able to
    /// photograph the light theme without rewriting the user's settings.
    pub fn set_theme(&mut self, mode: ThemeMode, accent: [u8; 3]) {
        if self.theme_locked {
            log::debug!(
                "theme: {} is set, not persisting the theme this run",
                theme::ENV_THEME
            );
            return;
        }
        let accent = phoebus_core::format_hex_color(accent);
        if self.state.theme_mode == mode && self.state.accent == accent {
            return;
        }
        self.state.theme_mode = mode;
        self.state.accent = accent;
        self.mark_dirty();
    }

    /// The library root the Settings view has configured, as the user typed it (`None` for
    /// "the default"). Not necessarily the *active* root — `$PHOEBUS_LIBRARY` outranks it.
    pub fn configured_library_root(&self) -> Option<&str> {
        self.state.configured_library_root()
    }

    /// Remember a new library root and write it out at once.
    ///
    /// Not debounced, unlike every other setting: the app is about to stop playback and
    /// rescan, which is long enough that a crash in between would otherwise lose the choice
    /// the user just made.
    pub fn set_library_root(&mut self, root: Option<String>) {
        let root = root.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if self.state.library_root == root {
            return;
        }
        self.state.library_root = root;
        self.save_now();
    }

    // ---- persistence -----------------------------------------------------------------

    /// Remember the current route.
    pub fn set_last_view(&mut self, view: String) {
        if self.state.last_view != view {
            self.state.last_view = view;
            self.mark_dirty();
        }
    }

    /// Remember the window size (ignoring sub-pixel jitter).
    pub fn set_window(&mut self, size: (f32, f32)) {
        let changed = match self.state.window {
            Some((w, h)) => (w - size.0).abs() > 1.0 || (h - size.1).abs() > 1.0,
            None => true,
        };
        if changed && size.0 >= 1.0 && size.1 >= 1.0 {
            self.state.window = Some(size);
            self.mark_dirty();
        }
    }

    /// The sidebar width to lay the panel out at, already clamped by `AppState::sanitize`.
    pub fn sidebar_w(&self) -> f32 {
        self.state.sidebar_w
    }

    /// The Up Next drawer width to lay the panel out at.
    pub fn queue_w(&self) -> f32 {
        self.state.queue_w
    }

    /// The Artists view's list width, seeded into `ViewState` at startup.
    pub fn artist_list_w(&self) -> f32 {
        self.state.artist_list_w
    }

    /// Remember the sidebar width the user just dragged the divider to.
    pub fn set_sidebar_w(&mut self, w: f32) {
        if remember_width(&mut self.state.sidebar_w, w, phoebus_core::SIDEBAR_WIDTH) {
            self.mark_dirty();
        }
    }

    /// Remember the Up Next drawer width.
    pub fn set_queue_w(&mut self, w: f32) {
        if remember_width(&mut self.state.queue_w, w, phoebus_core::QUEUE_WIDTH) {
            self.mark_dirty();
        }
    }

    /// Remember the Artists view's list width.
    pub fn set_artist_list_w(&mut self, w: f32) {
        if remember_width(
            &mut self.state.artist_list_w,
            w,
            phoebus_core::ARTIST_LIST_WIDTH,
        ) {
            self.mark_dirty();
        }
    }

    /// Write `state.json` if the debounce window has elapsed. Call once per frame.
    pub fn tick_save(&mut self) {
        let due = self.save_due.is_some_and(|t| Instant::now() >= t);
        if due {
            self.save_now();
        }
    }

    /// Write `state.json` immediately (used on exit and after a Settings change).
    pub fn save_now(&mut self) {
        self.save_due = None;
        if let Err(e) = self.state.save_to(&self.dirs.state_path()) {
            log::warn!("state: could not save: {e:#}");
        }
    }

    // ---- keyboard --------------------------------------------------------------------

    /// Map the UI-SPEC shortcuts to [`Action`]s.
    ///
    /// Bare keys are gated on `egui_wants_keyboard_input()` because egui still reports
    /// `Space` while a text field has focus (API-FACTS §3.6).
    pub fn shortcuts(&self, ctx: &egui::Context, out: &mut Vec<Action>) {
        let typing = ctx.egui_wants_keyboard_input();
        let (space, escape, rename, find, next, prev, louder, quieter) = ctx.input_mut(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::F2),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::F),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowDown),
            )
        });
        if space && !typing {
            out.push(Action::TogglePlay);
        }
        if rename && !typing {
            out.push(Action::RenameShortcut);
        }
        if escape {
            out.push(Action::Escape);
        }
        if find {
            out.push(Action::FocusSearch);
        }
        if next {
            out.push(Action::Next);
        }
        if prev {
            out.push(Action::Prev);
        }
        if louder {
            out.push(Action::VolumeBy(theme::VOLUME_STEP));
        }
        if quieter {
            out.push(Action::VolumeBy(-theme::VOLUME_STEP));
        }
    }

    // ---- internals -------------------------------------------------------------------

    /// Load whatever the queue says is current, skipping tracks that are not in the
    /// library (a rescan can drop files out from under a queue).
    fn load_current(&mut self, library: &Library, autoplay: bool) {
        for _ in 0..MAX_SKIPS {
            let Some(id) = self.queue.current() else {
                self.stop();
                return;
            };
            let Some(path) = library.track_path(id) else {
                log::warn!("queue: {id} is not in the library any more, skipping");
                if self.queue.advance(AdvanceReason::UserNext).is_none() {
                    self.stop();
                    return;
                }
                continue;
            };
            self.current = Some(id);
            self.pos = Duration::ZERO;
            self.scrub = None;
            self.duration = library.track(id).map_or(Duration::ZERO, |t| t.duration);
            // Assume seekable until `Loaded` says otherwise: only the decoder knows, and
            // waiting for it would make the first seek after a load a no-op.
            self.seekable = true;
            self.playing = autoplay;
            // One generation per Load, so every event can be traced back to the track it
            // describes. Bumped even without a device to keep the accounting uniform.
            self.generation += 1;
            match &self.player {
                Some(p) => {
                    if let Err(e) = p.load(path, autoplay, self.generation) {
                        log::warn!("audio: {e:#}");
                    }
                }
                None => log::debug!("audio: no device, pretending to play {id}"),
            }
            return;
        }
        log::warn!("queue: too many unplayable tracks in a row, stopping");
        self.stop();
        self.playback_error = Some(format!(
            "{MAX_SKIPS} tracks in a row are missing from the library; stopping"
        ));
    }

    /// Forget the failure history, so a user who asks for playback again after a run of
    /// broken files gets a fresh set of skips instead of an instant stop.
    fn forgive_failures(&mut self) {
        self.consecutive_failures = 0;
        self.playback_error = None;
    }

    fn send_volume(&self) {
        if let Some(p) = &self.player
            && let Err(e) = p.set_volume(self.volume)
        {
            log::warn!("audio: {e:#}");
        }
    }

    fn mark_dirty(&mut self) {
        if self.save_due.is_none() {
            self.save_due = Some(Instant::now() + Duration::from_millis(theme::SAVE_DEBOUNCE_MS));
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        if self.save_due.is_some() {
            self.save_now();
        }
    }
}

/// Store a dragged panel width, clamped, and answer whether that was a real change.
///
/// The width setters are called once a *frame* with whatever egui actually laid the panel
/// out at, so this has to ignore the sub-pixel difference `Rect::round_ui` leaves between
/// what we asked for and what came back — otherwise a window sitting perfectly still would
/// re-arm the save debounce sixty times a second. Half a point is well under the smallest
/// visible move and well over any rounding.
fn remember_width(slot: &mut f32, w: f32, range: phoebus_core::PanelWidth) -> bool {
    let w = range.clamp(w);
    if (*slot - w).abs() <= 0.5 {
        return false;
    }
    *slot = w;
    true
}

/// Keep a seek inside the track: past the end rodio silently drains the queue and reports
/// a bogus position (API-FACTS §1).
fn clamp_seek(target: Duration, duration: Duration) -> Duration {
    if duration.is_zero() {
        return target;
    }
    let limit = duration.saturating_sub(Duration::from_millis(1200));
    target.min(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    use phoebus_core::Track;

    const REL: [&str; 3] = [
        "Band/Album/01 One.mp3",
        "Band/Album/02 Two.mp3",
        "Band/Album/03 Three.mp3",
    ];

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn library() -> Library {
        Library::build("/music", REL.iter().map(|rel| Track::new(rel)).collect())
    }

    fn ids() -> Vec<TrackId> {
        REL.iter().map(|rel| TrackId::for_rel_path(rel)).collect()
    }

    /// A controller with no audio device: every command is a no-op, so `apply_event` can
    /// be driven by hand exactly as `poll` would drive it.
    fn controller() -> Controller {
        // A data dir of its own, so nothing here can reach the real `~/.phoebus/.phoebus`.
        let dirs = Dirs::at(std::env::temp_dir().join("phoebus-controller-tests"));
        let playlists = PlaylistStore::load_from(&dirs.playlists_path());
        Controller::with_player(dirs, AppState::default(), playlists, true, None)
    }

    /// A controller playing the three-track album from the top.
    fn playing() -> (Library, Vec<TrackId>, Controller) {
        let library = library();
        let ids = ids();
        let mut controller = controller();
        controller.play_context(&library, ids.clone(), 0);
        assert_eq!(controller.now().track, Some(ids[0]));
        assert!(controller.is_playing());
        (library, ids, controller)
    }

    fn event(controller: &Controller, kind: EventKind) -> Event {
        Event::new(controller.generation, kind)
    }

    #[test]
    fn a_run_of_failures_stops_playback_even_with_repeat_all() {
        let (library, _, mut controller) = playing();
        // Repeat::All means `advance` never returns None, so "the queue ran out" cannot be
        // the thing that ends a skip storm.
        controller.queue.set_repeat(Repeat::All);

        let mut errors = 0;
        while controller.is_playing() && errors < 64 {
            let e = event(&controller, EventKind::Error("no such file".to_string()));
            controller.apply_event(&library, e);
            errors += 1;
        }

        assert!(!controller.is_playing(), "the skip storm never ended");
        assert_eq!(
            errors, 3,
            "one pass over the three-track context, then stop"
        );
        assert_eq!(controller.now().track, None);
        assert!(
            controller
                .playback_error()
                .is_some_and(|m| m.contains("no such file")),
            "the failure is not surfaced: {:?}",
            controller.playback_error()
        );
    }

    #[test]
    fn a_successful_load_forgives_the_failures_before_it() {
        let (library, _, mut controller) = playing();
        controller.queue.set_repeat(Repeat::All);

        for _ in 0..2 {
            let e = event(&controller, EventKind::Error("flaky".to_string()));
            controller.apply_event(&library, e);
        }
        assert!(controller.is_playing(), "two of three is not a storm yet");
        assert_eq!(controller.consecutive_failures, 2);

        let loaded = event(
            &controller,
            EventKind::Loaded {
                duration: secs(200),
                seekable: true,
            },
        );
        controller.apply_event(&library, loaded);
        assert_eq!(controller.consecutive_failures, 0);
        assert_eq!(controller.duration(), secs(200));

        // The count started over, so the next two failures are skips, not a stop.
        for _ in 0..2 {
            let e = event(&controller, EventKind::Error("flaky".to_string()));
            controller.apply_event(&library, e);
        }
        assert!(controller.is_playing());
        assert!(controller.playback_error().is_none());
    }

    #[test]
    fn an_ended_from_the_outgoing_track_cannot_double_advance() {
        let (library, ids, mut controller) = playing();
        // Track one has finished and the engine has queued its `Ended`...
        let stale = event(&controller, EventKind::Ended);
        // ...but the user pressed Next in the same frame, so the app got there first.
        controller.next(&library);
        assert_eq!(controller.now().track, Some(ids[1]));

        controller.apply_event(&library, stale);
        assert_eq!(
            controller.now().track,
            Some(ids[1]),
            "the stale Ended advanced a second time"
        );
        assert!(!controller.queue.has_previous() || controller.queue.current() == Some(ids[1]));
    }

    #[test]
    fn a_stale_progress_cannot_repaint_the_incoming_track() {
        let (library, ids, mut controller) = playing();
        let live = event(&controller, EventKind::Progress { pos: secs(120) });
        controller.apply_event(&library, live.clone());
        assert_eq!(controller.display_pos(), secs(120));

        controller.next(&library);
        assert_eq!(controller.now().track, Some(ids[1]));
        assert_eq!(controller.display_pos(), Duration::ZERO);

        // Still in the channel from the track we just left.
        controller.apply_event(&library, live);
        assert_eq!(
            controller.display_pos(),
            Duration::ZERO,
            "the outgoing track's position leaked onto the new one"
        );
    }

    #[test]
    fn stopping_retires_the_generation() {
        let (library, _, mut controller) = playing();
        let stale = event(&controller, EventKind::Progress { pos: secs(90) });
        controller.stop();
        controller.apply_event(&library, stale);
        assert_eq!(controller.display_pos(), Duration::ZERO);
        assert!(!controller.is_playing());
    }

    #[test]
    fn a_failed_seek_never_skips_the_track() {
        let (library, ids, mut controller) = playing();
        let refused = event(
            &controller,
            EventKind::SeekFailed {
                pos: secs(42),
                message: "seek to 90s failed: demuxer error".to_string(),
            },
        );
        controller.apply_event(&library, refused);

        assert_eq!(
            controller.now().track,
            Some(ids[0]),
            "a refused seek skipped the track"
        );
        assert!(controller.is_playing());
        assert_eq!(
            controller.display_pos(),
            secs(42),
            "the readout did not snap back to the engine's position"
        );
        assert_eq!(controller.consecutive_failures, 0, "not a load failure");
    }

    #[test]
    fn an_unseekable_track_shows_the_engine_position_not_the_drag() {
        let (library, _, mut controller) = playing();
        assert!(
            controller.seekable(),
            "seekable until the decoder says otherwise"
        );

        // What the engine sends for a file whose decoder reports no total duration.
        let loaded = event(
            &controller,
            EventKind::Loaded {
                duration: Duration::ZERO,
                seekable: false,
            },
        );
        controller.apply_event(&library, loaded);
        assert!(!controller.seekable());

        let progress = event(&controller, EventKind::Progress { pos: secs(11) });
        controller.apply_event(&library, progress);

        // Dragging invents nothing, and a committed seek waits for the engine's answer.
        controller.seek_live(secs(120));
        assert_eq!(controller.display_pos(), secs(11));
        controller.seek(secs(120));
        assert_eq!(controller.display_pos(), secs(11));

        let refused = event(
            &controller,
            EventKind::SeekFailed {
                pos: secs(12),
                message: "this track reports no duration".to_string(),
            },
        );
        controller.apply_event(&library, refused);
        assert_eq!(controller.display_pos(), secs(12));
    }

    #[test]
    fn a_seekable_track_still_snaps_optimistically() {
        let (library, _, mut controller) = playing();
        let loaded = event(
            &controller,
            EventKind::Loaded {
                duration: secs(200),
                seekable: true,
            },
        );
        controller.apply_event(&library, loaded);

        controller.seek_live(secs(60));
        assert_eq!(controller.display_pos(), secs(60));
        controller.seek(secs(90));
        assert_eq!(controller.display_pos(), secs(90));
    }

    #[test]
    fn a_dead_engine_stops_playback_instead_of_faking_it() {
        let (_, _, mut controller) = playing();
        let last = event(
            &controller,
            EventKind::Error("the audio engine crashed: device disappeared".to_string()),
        );

        controller.engine_died(&[last]);

        assert!(!controller.is_playing(), "still pretending to play");
        assert!(controller.now().track.is_none());
        assert!(controller.player.is_none(), "the dead handle is still held");
        assert!(
            controller
                .playback_error()
                .is_some_and(|m| m.contains("device disappeared")),
            "no reason surfaced: {:?}",
            controller.playback_error()
        );
    }

    #[test]
    fn seeks_are_clamped_inside_the_track() {
        let d = Duration::from_secs(100);
        assert_eq!(
            clamp_seek(Duration::from_secs(10), d),
            Duration::from_secs(10)
        );
        assert_eq!(
            clamp_seek(Duration::from_secs(999), d),
            Duration::from_millis(98_800)
        );
        // Unknown duration: pass the target through, the engine clamps too.
        assert_eq!(
            clamp_seek(Duration::from_secs(5), Duration::ZERO),
            Duration::from_secs(5)
        );
    }

    /// The reported bug: `SHUFFLE` on a 16-track album only ever opened on track 1 or 9,
    /// because the start index came from `SystemTime::now().subsec_nanos() % len` while
    /// macOS's realtime clock ticks in whole microseconds. Every track must be reachable.
    #[test]
    fn shuffle_opens_on_every_track_of_the_album() {
        let library = library();
        let ids = ids();
        let mut controller = controller();

        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            controller.shuffle_context(&library, ids.clone());
            seen.insert(controller.now().track.expect("something is loaded"));
        }
        assert!(controller.shuffle(), "SHUFFLE turns shuffle on");
        assert_eq!(
            seen.len(),
            ids.len(),
            "every track must be able to open the album, saw {seen:?}"
        );
    }

    /// `▶ PLAY` clears shuffle and starts from the top; a double-clicked row keeps the
    /// shuffle session; the `SHUFFLE` button turns it on with a fresh uniform order.
    #[test]
    fn play_clears_shuffle_but_a_named_row_and_the_shuffle_button_keep_it() {
        let library = library();
        let ids = ids();
        let mut controller = controller();

        controller.set_shuffle(true);
        for _ in 0..8 {
            controller.play_collection(&library, ids.clone());
            assert!(!controller.shuffle(), "PLAY clears the shuffle toggle");
            assert_eq!(
                controller.now().track,
                Some(ids[0]),
                "PLAY starts at the top"
            );
        }

        controller.set_shuffle(true);
        controller.play_context(&library, ids.clone(), 2);
        assert!(
            controller.shuffle(),
            "a named row keeps the shuffle session"
        );
        assert_eq!(
            controller.now().track,
            Some(ids[2]),
            "an explicit start survives shuffle"
        );

        controller.set_shuffle(false);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            controller.shuffle_context(&library, ids.clone());
            assert!(controller.shuffle(), "SHUFFLE turns the toggle on");
            seen.insert(controller.now().track.expect("something is loaded"));
        }
        assert_eq!(seen.len(), ids.len(), "SHUFFLE re-rolls uniformly");
    }
}
