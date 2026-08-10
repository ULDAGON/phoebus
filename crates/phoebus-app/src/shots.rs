//! Screenshot runs — how the UI gets reviewed without a human in front of the window.
//!
//! * `--shot-once <path>`: run the real app, grab one screenshot, write it, quit ([`Shot`]).
//! * `--shot <outdir>`: walk a fixed [`Tour`] of every view, one PNG per step, then quit.
//!   The tour always runs muted, whatever the environment says. It writes nothing outside
//!   `<outdir>` and the app-data directory (`$PHOEBUS_DATA`).
//!
//! Both wait for the same two things before pressing the shutter: the library scan has
//! finished, and the cover cache has nothing in flight ([`crate::artwork::Artwork::is_idle`]),
//! so a screenshot never catches a half-loaded grid.
//!
//! A step that never settles is still photographed — a PNG of a half-loaded grid is worth
//! more than no PNG at all when something is wrong — but it is recorded as a failure
//! ([`failed_steps`]) and `main` exits non-zero, so `--shot` is a gate that can actually
//! fail rather than one that always reports success.
//!
//! Two environment variables steer the one-shot mode:
//! * `PHOEBUS_START_MUTED=1` — volume 0 at start-up, not persisted (see
//!   [`crate::controller::ENV_START_MUTED`]).
//! * `PHOEBUS_SHOT_PLAYING=1` — auto-play the first album before shooting, so the frame
//!   shows a populated player bar.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Steps that had to be shot before they settled. Written by [`Tour::should_request`],
/// read by `main` once the window is gone — the app itself is owned by `eframe` by then,
/// so there is nothing left to hang the result off.
static FAILED_STEPS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Names of the tour steps that timed out, in tour order. Empty means the run is clean.
pub fn failed_steps() -> Vec<&'static str> {
    FAILED_STEPS
        .lock()
        .map(|failed| failed.clone())
        .unwrap_or_default()
}

fn record_failure(step: Step) {
    if let Ok(mut failed) = FAILED_STEPS.lock() {
        failed.push(step.name());
    }
}

/// Environment flag that makes a `--shot-once` run start playing first.
pub const ENV_SHOT_PLAYING: &str = "PHOEBUS_SHOT_PLAYING";

/// Which kind of screenshot run the command line asked for.
pub enum Capture {
    /// `--shot-once <path>`: one PNG, then quit.
    Once(PathBuf),
    /// `--shot <outdir>`: the whole [`Step`] tour, then quit.
    Tour(PathBuf),
}

/// Where an autoplay run seeks to before shooting, so the seek fill and the elapsed
/// timestamp are actually legible in the PNG.
pub const SHOT_SEEK: Duration = Duration::from_secs(30);
/// The position the run waits for. Deliberately *past* [`SHOT_SEEK`]: only real `Progress`
/// events from the engine can push the readout there, so every autoplay shot is also a
/// proof that load → seek → progress works end to end.
pub const SHOT_MIN_POS: Duration = Duration::from_secs(32);

/// Frames to let the UI settle (textures, layout) before the grab.
const WARMUP_FRAMES: u64 = 5;
/// Hard stop: shoot anyway rather than hang forever waiting for a library or a position.
///
/// This one clock covers the *whole* run, first scan included — a cold scan of a real Apple
/// Music library (3.4k tracks, 290 covers to extract and resize) takes tens of seconds, and
/// at 20 s every such shot came out as a half-loaded grid plus a `giving up waiting` warning.
/// The tour's [`STEP_DEADLINE`] can stay short because its clock only starts once the library
/// has arrived.
const DEADLINE: Duration = Duration::from_secs(120);

/// State of a one-shot screenshot run.
pub struct Shot {
    /// Where the PNG goes.
    pub path: PathBuf,
    /// Auto-play the first album before shooting.
    pub autoplay: bool,
    /// Playback has been kicked off already.
    pub played: bool,
    /// The viewport command has been sent.
    pub requested: bool,
    /// Frames rendered so far.
    pub frames: u64,
    started: Instant,
}

impl Shot {
    /// Prepare a run that writes to `path`, reading [`ENV_SHOT_PLAYING`].
    pub fn new(path: PathBuf) -> Shot {
        let autoplay = std::env::var(ENV_SHOT_PLAYING).is_ok_and(|v| v == "1");
        log::info!("--shot-once {} (autoplay: {autoplay})", path.display());
        Shot {
            path,
            autoplay,
            played: false,
            requested: false,
            frames: 0,
            started: Instant::now(),
        }
    }

    /// True when the screenshot should be requested this frame.
    ///
    /// `library_ready` is "the scan has finished", `playing_ready` is "the player bar has
    /// something to show" (only consulted when [`Shot::autoplay`] is on).
    pub fn should_request(&self, library_ready: bool, playing_ready: bool) -> bool {
        if self.requested || self.frames < WARMUP_FRAMES {
            return false;
        }
        if self.started.elapsed() >= DEADLINE {
            log::warn!("shot: giving up waiting, shooting whatever is on screen");
            return true;
        }
        library_ready && (!self.autoplay || playing_ready)
    }

    /// Write the captured frame to [`Shot::path`].
    pub fn save(&self, image: &egui::ColorImage) -> Result<()> {
        write_png(image, &self.path)
    }
}

// ---------------------------------------------------------------------------------------
// The `--shot` tour
// ---------------------------------------------------------------------------------------

/// The query the Search step types into the sidebar.
pub const TOUR_QUERY: &str = "let";
/// Name of the demo playlist the tour invents when the library has none. It exists in
/// memory only and is never written to `playlists.json`.
pub const DEMO_PLAYLIST: &str = "LATE NIGHT";
/// Id of that demo playlist — deliberately out of the range `PlaylistStore` hands out.
pub const DEMO_PLAYLIST_ID: u64 = u64::MAX;
/// How many tracks the demo playlist gets.
pub const DEMO_PLAYLIST_LEN: usize = 8;
/// How many albums the tour hearts when the library has no favorites of its own, and how
/// many of each album's tracks. Both stay small: the point of `favorites.png` is that the
/// view has rows, not that it is full.
///
/// The albums are hearted from index 1, never index 0 — `albums.png` is shot before
/// `favorites.png`, and hearting the grid's own first album would make its FAVORITES-
/// section copy indistinguishable from the grid's first card in the shot.
pub const DEMO_FAV_ALBUMS: std::ops::Range<usize> = 1..3;
/// Which tracks of each demo-favourite album get a heart. Includes album 0's, which is the
/// tracklist `album.png` and `playing.png` photograph — so the heart column is visible
/// there too, filled and empty in the same shot.
pub const DEMO_FAV_TRACKS: [usize; 2] = [0, 2];
/// How many albums from the top of the library contribute demo-favourite *tracks*.
pub const DEMO_FAV_TRACK_ALBUMS: usize = 3;
/// How many manual-queue items the `playing` step adds, so `◆` markers are visible.
pub const TOUR_MANUAL: usize = 2;
/// The readout the `playing` step waits for. Just under [`SHOT_SEEK`], because the track
/// is *paused* there: the position can only arrive from the engine's post-seek `Progress`,
/// never by playing on.
pub const TOUR_MIN_POS: Duration = Duration::from_secs(28);

/// Frames a step must stay settled (library ready, no cover in flight) before the grab.
const SETTLE_FRAMES: u64 = 4;
/// Per-step hard stop: shoot whatever is on screen rather than hang.
const STEP_DEADLINE: Duration = Duration::from_secs(15);

/// One stop on the `--shot` tour, in the order they are captured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// `recently.png`
    Recently,
    /// `albums.png`
    Albums,
    /// `album.png` — the first album alphabetically.
    Album,
    /// `artists.png`
    Artists,
    /// `songs.png`
    Songs,
    /// `favorites.png` — seeded with ephemeral demo favorites if the user has none.
    Favorites,
    /// `playlist.png`
    Playlist,
    /// `add-songs.png` — the same playlist with the add-songs picker up.
    AddSongs,
    /// `search.png`
    Search,
    /// `settings.png` — the Settings view in its default state.
    Settings,
    /// `playing.png` — loaded, paused, queue drawer open.
    Playing,
}

impl Step {
    /// Every step, in tour order. `Playing` stays last: it is the only step that leaves
    /// audio loaded, so everything photographed before it has an idle player bar.
    pub const ALL: [Step; 11] = [
        Step::Recently,
        Step::Albums,
        Step::Album,
        Step::Artists,
        Step::Songs,
        Step::Favorites,
        Step::Playlist,
        Step::AddSongs,
        Step::Search,
        Step::Settings,
        Step::Playing,
    ];

    /// File stem of this step's PNG.
    pub fn name(self) -> &'static str {
        match self {
            Step::Recently => "recently",
            Step::Albums => "albums",
            Step::Album => "album",
            Step::Artists => "artists",
            Step::Songs => "songs",
            Step::Favorites => "favorites",
            Step::Playlist => "playlist",
            Step::AddSongs => "add-songs",
            Step::Search => "search",
            Step::Settings => "settings",
            Step::Playing => "playing",
        }
    }
}

/// State of a `--shot <outdir>` run.
pub struct Tour {
    dir: PathBuf,
    index: usize,
    /// The current step's app state has been applied.
    applied: bool,
    /// Consecutive settled frames since the step was applied.
    settled: u64,
    /// The viewport command for this step has been sent.
    requested: bool,
    started: Instant,
}

impl Tour {
    /// Prepare a tour that writes into `dir`.
    pub fn new(dir: PathBuf) -> Tour {
        log::info!("--shot {} ({} steps)", dir.display(), Step::ALL.len());
        Tour {
            dir,
            index: 0,
            applied: false,
            settled: 0,
            requested: false,
            started: Instant::now(),
        }
    }

    /// The step being captured, or `None` once the tour is done.
    pub fn current(&self) -> Option<Step> {
        Step::ALL.get(self.index).copied()
    }

    /// True while the current step still needs its app state applied.
    pub fn needs_setup(&self) -> bool {
        !self.applied
    }

    /// Record that the current step's app state is in place.
    pub fn applied(&mut self) {
        self.applied = true;
        self.settled = 0;
        self.started = Instant::now();
    }

    /// Count one frame. `settled` is "the library is ready and no cover is loading".
    pub fn frame(&mut self, settled: bool) {
        if settled {
            self.settled += 1;
        } else {
            self.settled = 0;
        }
    }

    /// True when the screenshot should be requested this frame. `ready` is the step's own
    /// extra precondition (the `playing` step waits for a position on the readout).
    ///
    /// Timing out still shoots — a picture of what went wrong is useful — but the step is
    /// recorded in [`failed_steps`] so the process can exit non-zero.
    pub fn should_request(&mut self, ready: bool) -> bool {
        if self.requested || !self.applied {
            return false;
        }
        let timed_out = self.started.elapsed() >= STEP_DEADLINE;
        if timed_out && let Some(step) = self.current() {
            log::warn!("shot: step {step:?} never settled, shooting anyway — the run FAILS");
            record_failure(step);
        }
        if timed_out || (ready && self.settled >= SETTLE_FRAMES) {
            self.requested = true;
            return true;
        }
        false
    }

    /// Write the captured frame as `<outdir>/<step>.png` and move to the next step.
    /// Returns `true` when the tour is finished.
    pub fn save_and_advance(&mut self, image: &egui::ColorImage) -> bool {
        if let Some(step) = self.current() {
            let path = self.dir.join(format!("{}.png", step.name()));
            if let Err(e) = write_png(image, &path) {
                log::error!("shot: {e:#}");
                record_failure(step);
            }
        }
        self.index += 1;
        self.applied = false;
        self.settled = 0;
        self.requested = false;
        self.started = Instant::now();
        self.current().is_none()
    }
}

fn write_png(image: &egui::ColorImage, path: &std::path::Path) -> Result<()> {
    let (w, h) = (image.width() as u32, image.height() as u32);
    let buffer = image::RgbaImage::from_raw(w, h, image.as_raw().to_vec())
        .context("the screenshot had an unexpected buffer size")?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    buffer
        .save(path)
        .with_context(|| format!("writing {}", path.display()))?;
    log::info!("shot: wrote {}x{} to {}", w, h, path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_has_a_distinct_file_name() {
        let mut names: Vec<&str> = Step::ALL.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique);
        assert_eq!(Step::ALL[0], Step::Recently);
        assert_eq!(Step::ALL[Step::ALL.len() - 1], Step::Playing);
    }

    #[test]
    fn a_tour_waits_for_setup_and_for_settled_frames() {
        let mut tour = Tour::new(std::env::temp_dir().join("phoebus-tour-test"));
        assert_eq!(tour.current(), Some(Step::Recently));
        assert!(tour.needs_setup());
        assert!(!tour.should_request(true), "nothing applied yet");
        tour.applied();
        assert!(!tour.needs_setup());
        assert!(!tour.should_request(true), "not settled yet");
        for _ in 0..SETTLE_FRAMES {
            tour.frame(true);
        }
        assert!(!tour.should_request(false), "step is not ready");
        assert!(tour.should_request(true));
        assert!(!tour.should_request(true), "requested exactly once");
    }

    #[test]
    fn unsettled_frames_reset_the_counter() {
        let mut tour = Tour::new(std::env::temp_dir().join("phoebus-tour-test"));
        tour.applied();
        for _ in 0..SETTLE_FRAMES {
            tour.frame(true);
        }
        tour.frame(false);
        assert!(!tour.should_request(true));
    }

    #[test]
    fn a_step_that_times_out_shoots_but_fails_the_run() {
        let before = failed_steps().len();
        let mut tour = Tour::new(std::env::temp_dir().join("phoebus-tour-test"));
        tour.applied();
        tour.started = Instant::now() - STEP_DEADLINE;
        assert!(
            tour.should_request(false),
            "a timed-out step is photographed anyway"
        );
        let failed = failed_steps();
        assert_eq!(failed.len(), before + 1, "…and recorded as a failure");
        assert_eq!(failed.last().copied(), Some(Step::Recently.name()));
        assert!(
            !tour.should_request(false),
            "the failure is recorded exactly once"
        );
    }
}
