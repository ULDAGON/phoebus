//! Persisted UI state: `state.json` in the app-data directory ([`crate::paths::Dirs`]).

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::color;
use crate::paths;
use crate::queue::Repeat;

/// The default UI volume (0.0..=1.0) for a fresh install.
pub const DEFAULT_VOLUME: f32 = 0.8;
/// The view the app opens on when nothing has been saved yet.
pub const DEFAULT_VIEW: &str = "recently_added";
/// The default accent color — UI-SPEC v1.2's yellow.
pub const DEFAULT_ACCENT: &str = "#FFFB00";

/// A panel width the user can drag, as `state.json` remembers it.
///
/// The three numbers travel together because they are one decision: what a fresh install
/// gets, and how far a drag — or somebody's text editor — may take it. They live in core
/// rather than in the app's `theme` module because [`AppState::sanitize`] is what has to
/// clamp the value read off disk, and core knows nothing of egui. The app re-exports them
/// under its own names (`theme::SIDEBAR_W` and friends) so its "every layout number is in
/// theme.rs" rule still holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelWidth {
    /// What a fresh install starts at.
    pub default: f32,
    /// The narrowest a drag may leave it.
    pub min: f32,
    /// The widest a drag may leave it.
    pub max: f32,
}

impl PanelWidth {
    /// `w` brought inside the range. A NaN or infinite width is not a width at all and
    /// becomes [`PanelWidth::default`] rather than one of the bounds.
    pub fn clamp(self, w: f32) -> f32 {
        if w.is_finite() {
            w.clamp(self.min, self.max)
        } else {
            self.default
        }
    }
}

/// Sidebar width (UI-SPEC §Layout gives the 230 px default, v1.4 §Panel widths the range).
///
/// The floor is what `RECENTLY ADDED` — the longest nav label — still needs at `SIZE_SMALL`
/// with its letter-spacing, its section indent and the panel's two 14 px paddings; the app
/// pins that with a test that measures the label. The ceiling, together with
/// [`QUEUE_WIDTH`]'s, leaves a whole album card plus its page padding in the content column
/// even at the 980 px minimum window width — also pinned by a test.
pub const SIDEBAR_WIDTH: PanelWidth = PanelWidth {
    default: 230.0,
    min: 180.0,
    max: 340.0,
};

/// Up Next drawer width (UI-SPEC §Layout: 300 px).
///
/// The floor keeps a queue row's title, artist and duration on one line; the ceiling is
/// well under half the minimum window and is the other half of the content-column
/// guarantee described on [`SIDEBAR_WIDTH`].
pub const QUEUE_WIDTH: PanelWidth = PanelWidth {
    default: 300.0,
    min: 220.0,
    max: 400.0,
};

/// Width of the Artists view's left-hand list (UI-SPEC §Artists: 260 px).
///
/// Stored as absolute points, not as a fraction of the split, because that is what the view
/// has always used and because a fraction would make the list creep wider on a big window
/// while the sidebar beside it stayed put. The ceiling here is generous: the view narrows it
/// further at draw time so the album side never loses its last card.
pub const ARTIST_LIST_WIDTH: PanelWidth = PanelWidth {
    default: 260.0,
    min: 180.0,
    max: 520.0,
};

/// Which palette the UI paints with.
///
/// Serialized in snake_case (`"dark"` / `"light"`) so `state.json` stays hand-editable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// The default: near-black surfaces.
    #[default]
    Dark,
    /// Paper-white surfaces.
    Light,
}

impl ThemeMode {
    /// `"dark"` / `"light"` — the same spelling as in `state.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    /// Parse `"dark"` / `"light"` case-insensitively (surrounding whitespace ignored).
    ///
    /// Used for the `PHOEBUS_THEME` one-run override as well as for the file.
    pub fn parse(s: &str) -> Option<ThemeMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ThemeMode::Dark),
            "light" => Some(ThemeMode::Light),
            _ => None,
        }
    }

    /// True for [`ThemeMode::Dark`].
    pub fn is_dark(self) -> bool {
        self == ThemeMode::Dark
    }

    /// The other mode — what the `DARK`/`LIGHT` buttons in Settings toggle between.
    pub fn toggled(self) -> ThemeMode {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }
}

/// Small blob of state that survives a restart.
///
/// `last_view` is an opaque `String` on purpose: core stays UI-agnostic and the app maps it
/// to whatever route enum it likes.
///
/// Every field is `#[serde(default)]`, so a `state.json` written by an older build (no
/// `library_root`, `theme_mode` or `accent`, and none of the three v1.4 panel widths) loads
/// with the defaults for those — which are exactly the older build's fixed behavior.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// UI volume, 0.0..=1.0 (the perceptual curve is applied by the audio engine).
    pub volume: f32,
    /// Shuffle toggle.
    pub shuffle: bool,
    /// Repeat mode.
    pub repeat: Repeat,
    /// Opaque route identifier owned by the app.
    pub last_view: String,
    /// Last window size in logical points.
    pub window: Option<(f32, f32)>,
    /// Library root chosen in Settings, as typed (a leading `~` is allowed and is expanded
    /// by [`paths::resolve_library_root`]). `None` means the default `~/.phoebus`.
    ///
    /// Kept as written rather than as a resolved `PathBuf` so the Settings input can show
    /// the user their own spelling, and so a root on a volume that is not mounted right now
    /// is not silently forgotten.
    pub library_root: Option<String>,
    /// Dark or light palette.
    pub theme_mode: ThemeMode,
    /// Accent color as `#RRGGBB`. Normalized to uppercase on load; an unparseable value
    /// falls back to [`DEFAULT_ACCENT`].
    pub accent: String,
    /// Follow a theme file the desktop offers (the Omarchy bridge), when there is one.
    ///
    /// `true` — the default — means a desktop that renders a `phoebus.toml` drives the
    /// palette; `false` means the user explicitly asked for Phoebus's own theme (the
    /// `theme_mode` + `accent` above) even while such a file exists. Meaningless, and
    /// ignored, where no theme file is ever found.
    pub follow_desktop_theme: bool,
    /// Sidebar width in logical points, as the user last dragged it ([`SIDEBAR_WIDTH`]).
    pub sidebar_w: f32,
    /// Up Next drawer width in logical points ([`QUEUE_WIDTH`]).
    pub queue_w: f32,
    /// Artists view list width in logical points ([`ARTIST_LIST_WIDTH`]).
    pub artist_list_w: f32,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            volume: DEFAULT_VOLUME,
            shuffle: false,
            repeat: Repeat::Off,
            last_view: DEFAULT_VIEW.to_string(),
            window: None,
            library_root: None,
            theme_mode: ThemeMode::default(),
            accent: DEFAULT_ACCENT.to_string(),
            follow_desktop_theme: true,
            sidebar_w: SIDEBAR_WIDTH.default,
            queue_w: QUEUE_WIDTH.default,
            artist_list_w: ARTIST_LIST_WIDTH.default,
        }
    }
}

impl AppState {
    /// Load `state.json` from an exact path. A missing or corrupt file yields
    /// [`AppState::default`] — never an error.
    ///
    /// A *missing* file is the normal fresh-install case and is silent. A file that exists
    /// but cannot be read (non-UTF-8 bytes, bad permissions, I/O error) is a real problem
    /// and is reported with a `log::warn!` instead of silently resetting the settings.
    /// Unlike playlists, this blob is regenerated rather than protected: it only holds
    /// volume, shuffle/repeat, the last view, the window size and the Settings choices.
    pub fn load_from(path: &Path) -> AppState {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AppState::default(),
            Err(e) => {
                log::warn!(
                    "state: {} cannot be read ({e}); using defaults",
                    path.display()
                );
                return AppState::default();
            }
        };
        match serde_json::from_str::<AppState>(&text) {
            Ok(mut state) => {
                state.sanitize();
                state
            }
            Err(e) => {
                log::warn!("state: {} is corrupt, using defaults: {e}", path.display());
                AppState::default()
            }
        }
    }

    /// Write `state.json` to an exact path, atomically (tmp file + rename).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        paths::write_atomic(path, &json)
    }

    /// The configured library root, trimmed, or `None` when it is unset or blank.
    pub fn configured_library_root(&self) -> Option<&str> {
        self.library_root
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// The accent as `[r, g, b]`. Always `Some` after [`AppState::load_from`] (which
    /// sanitizes it); `None` only if the field was overwritten with nonsense in memory.
    pub fn accent_rgb(&self) -> Option<[u8; 3]> {
        color::parse_hex_color(&self.accent)
    }

    /// Clamp anything a hand-edited file could have broken.
    fn sanitize(&mut self) {
        if !self.volume.is_finite() {
            self.volume = DEFAULT_VOLUME;
        }
        self.volume = self.volume.clamp(0.0, 1.0);
        if self.last_view.trim().is_empty() {
            self.last_view = DEFAULT_VIEW.to_string();
        }
        if let Some((w, h)) = self.window
            && (!w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0)
        {
            self.window = None;
        }
        // A blank library_root is "not configured", not "a root called nothing".
        self.library_root = self
            .library_root
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // A hand-edited width must never be able to hide a panel or swallow the window.
        self.sidebar_w = SIDEBAR_WIDTH.clamp(self.sidebar_w);
        self.queue_w = QUEUE_WIDTH.clamp(self.queue_w);
        self.artist_list_w = ARTIST_LIST_WIDTH.clamp(self.artist_list_w);
        // The accent has to be paintable: normalize it to `#RRGGBB`, or say why it went.
        match color::parse_hex_color(&self.accent) {
            Some(rgb) => self.accent = color::format_hex_color(rgb),
            None => {
                log::warn!(
                    "state: accent {:?} is not #RRGGBB; using {DEFAULT_ACCENT}",
                    self.accent
                );
                self.accent = DEFAULT_ACCENT.to_string();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, Once};

    static LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            LOGS.lock()
                .expect("log buffer")
                .push(record.args().to_string());
        }
        fn flush(&self) {}
    }

    /// `state.json` inside a throwaway data directory — the shape the app uses, with the
    /// data dir handed in explicitly rather than resolved from the environment.
    fn state_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        paths::Dirs::at(dir.path()).state_path()
    }

    /// Run `f` and return everything it logged. Other tests may log concurrently, so only
    /// ever assert that a line *is* present.
    fn logged(f: impl FnOnce()) -> Vec<String> {
        static CAPTURE: Capture = Capture;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = log::set_logger(&CAPTURE);
            log::set_max_level(log::LevelFilter::Warn);
        });
        f();
        LOGS.lock().expect("log buffer").clone()
    }

    #[test]
    fn defaults_when_the_file_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = AppState::load_from(&state_path(&dir));
        assert_eq!(s, AppState::default());
        assert_eq!(s.volume, DEFAULT_VOLUME);
        assert_eq!(s.last_view, DEFAULT_VIEW);
        assert_eq!(s.repeat, Repeat::Off);
        assert!(s.window.is_none());
        assert_eq!(s.library_root, None, "default root = ~/.phoebus");
        assert_eq!(s.theme_mode, ThemeMode::Dark);
        assert_eq!(s.accent, DEFAULT_ACCENT);
        assert!(
            s.follow_desktop_theme,
            "a desktop theme is followed until the user says otherwise"
        );
        assert_eq!(s.sidebar_w, SIDEBAR_WIDTH.default);
        assert_eq!(s.queue_w, QUEUE_WIDTH.default);
        assert_eq!(s.artist_list_w, ARTIST_LIST_WIDTH.default);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = AppState {
            volume: 0.35,
            shuffle: true,
            repeat: Repeat::One,
            last_view: "albums".to_string(),
            window: Some((1440.0, 900.0)),
            library_root: Some("~/Music/Media.localized/Music".to_string()),
            theme_mode: ThemeMode::Light,
            accent: "#2EF0FF".to_string(),
            follow_desktop_theme: false,
            sidebar_w: 265.0,
            queue_w: 244.0,
            artist_list_w: 310.0,
        };
        s.save_to(&state_path(&dir)).expect("save");
        assert!(state_path(&dir).exists());
        assert_eq!(AppState::load_from(&state_path(&dir)), s);
    }

    /// The data-dir API writes an exact path, with no library root anywhere in sight.
    #[test]
    fn round_trips_through_an_explicit_data_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(tmp.path().join("data"));
        let s = AppState {
            volume: 0.5,
            library_root: Some("/music".to_string()),
            theme_mode: ThemeMode::Light,
            ..AppState::default()
        };
        s.save_to(&dirs.state_path()).expect("save");
        assert_eq!(
            dirs.state_path(),
            tmp.path().join("data").join("state.json"),
            "no .phoebus subdirectory and no library root involved"
        );
        assert_eq!(AppState::load_from(&dirs.state_path()), s);
        assert_eq!(
            AppState::load_from(&tmp.path().join("nope.json")),
            AppState::default()
        );
    }

    /// `theme_mode` is snake_case on disk so the file stays hand-editable.
    #[test]
    fn theme_mode_serializes_as_snake_case() {
        let json = serde_json::to_string(&AppState {
            theme_mode: ThemeMode::Light,
            ..AppState::default()
        })
        .expect("serialize");
        assert!(json.contains(r#""theme_mode":"light""#), "{json}");
        assert_eq!(
            serde_json::from_str::<ThemeMode>(r#""dark""#).expect("parse"),
            ThemeMode::Dark
        );
        assert_eq!(ThemeMode::Light.as_str(), "light");
        assert_eq!(ThemeMode::parse("  DARK "), Some(ThemeMode::Dark));
        assert_eq!(ThemeMode::parse("Light"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("sepia"), None);
        assert!(ThemeMode::Dark.is_dark());
        assert_eq!(ThemeMode::Dark.toggled(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.toggled(), ThemeMode::Dark);
    }

    /// A `state.json` written by a pre-v1.1 build has none of the three new fields; it must
    /// load with exactly the old behavior rather than being treated as corrupt.
    #[test]
    fn an_old_format_file_loads_with_the_new_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            br#"{
              "volume": 0.4,
              "shuffle": true,
              "repeat": "All",
              "last_view": "songs",
              "window": [1280.0, 820.0]
            }"#,
        )
        .expect("write");
        let s = AppState::load_from(&state_path(&dir));
        assert_eq!(s.volume, 0.4);
        assert!(s.shuffle);
        assert_eq!(s.repeat, Repeat::All);
        assert_eq!(s.last_view, "songs");
        assert_eq!(s.window, Some((1280.0, 820.0)));
        assert_eq!(s.library_root, None);
        assert_eq!(s.theme_mode, ThemeMode::Dark);
        assert_eq!(s.accent, DEFAULT_ACCENT);
        assert!(
            s.follow_desktop_theme,
            "a file from before the toggle keeps following the desktop"
        );
        assert_eq!(s.configured_library_root(), None);
        // …and the v1.4 panel widths come back as the fixed sizes that build had.
        assert_eq!(s.sidebar_w, SIDEBAR_WIDTH.default);
        assert_eq!(s.queue_w, QUEUE_WIDTH.default);
        assert_eq!(s.artist_list_w, ARTIST_LIST_WIDTH.default);
    }

    /// A dragged panel width survives the round trip; a hand-edited one that would hide a
    /// panel or swallow the window is pulled back to the nearest usable size, and a width
    /// that is not a number at all falls back to the default rather than to a bound.
    #[test]
    fn panel_widths_round_trip_and_insane_ones_are_clamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        let dragged = AppState {
            sidebar_w: 271.0,
            queue_w: 355.0,
            artist_list_w: 199.0,
            ..AppState::default()
        };
        dragged.save_to(&path).expect("save");
        assert_eq!(AppState::load_from(&path), dragged);

        std::fs::write(
            &path,
            br#"{ "sidebar_w": 4000.0, "queue_w": 1.0, "artist_list_w": -20.0 }"#,
        )
        .expect("write");
        let s = AppState::load_from(&path);
        assert_eq!(s.sidebar_w, SIDEBAR_WIDTH.max);
        assert_eq!(s.queue_w, QUEUE_WIDTH.min);
        assert_eq!(s.artist_list_w, ARTIST_LIST_WIDTH.min);

        // JSON has no NaN literal, so that one is only reachable in memory.
        let mut broken = AppState {
            sidebar_w: f32::NAN,
            queue_w: f32::INFINITY,
            ..AppState::default()
        };
        broken.sanitize();
        assert_eq!(broken.sidebar_w, SIDEBAR_WIDTH.default);
        assert_eq!(broken.queue_w, QUEUE_WIDTH.default);
    }

    /// Every range has to be a range, with the default inside it — a typo that inverted
    /// `min` and `max` would make `f32::clamp` panic at load.
    #[test]
    fn every_panel_width_range_contains_its_default() {
        for (name, w) in [
            ("sidebar", SIDEBAR_WIDTH),
            ("queue", QUEUE_WIDTH),
            ("artist list", ARTIST_LIST_WIDTH),
        ] {
            assert!(w.min < w.max, "{name}: {:?}", w);
            assert!(w.min <= w.default && w.default <= w.max, "{name}: {:?}", w);
            assert_eq!(w.clamp(w.min - 1.0), w.min, "{name}");
            assert_eq!(w.clamp(w.max + 1.0), w.max, "{name}");
            assert_eq!(w.clamp(w.default), w.default, "{name}");
        }
    }

    #[test]
    fn a_broken_accent_falls_back_and_a_good_one_is_normalized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        std::fs::write(&path, br#"{ "accent": "  #2ef0ff " }"#).expect("write");
        let s = AppState::load_from(&state_path(&dir));
        assert_eq!(s.accent, "#2EF0FF", "canonical uppercase, trimmed");
        assert_eq!(s.accent_rgb(), Some([0x2E, 0xF0, 0xFF]));

        std::fs::write(&path, br#"{ "accent": "neon yellow please" }"#).expect("write");
        let lines = logged(|| {
            assert_eq!(
                AppState::load_from(&state_path(&dir)).accent,
                DEFAULT_ACCENT
            );
        });
        assert!(
            lines.iter().any(|l| l.contains("not #RRGGBB")),
            "an unpaintable accent must be reported: {lines:?}"
        );

        // A wrong *type* is still just a corrupt file: defaults, not a panic.
        std::fs::write(&path, br#"{ "accent": 42 }"#).expect("write");
        assert_eq!(AppState::load_from(&state_path(&dir)), AppState::default());
    }

    #[test]
    fn a_blank_library_root_means_not_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        std::fs::write(&path, br#"{ "library_root": "   " }"#).expect("write");
        assert_eq!(AppState::load_from(&state_path(&dir)).library_root, None);

        std::fs::write(&path, br#"{ "library_root": "  ~/Music  " }"#).expect("write");
        let s = AppState::load_from(&state_path(&dir));
        assert_eq!(
            s.library_root.as_deref(),
            Some("~/Music"),
            "trimmed, as typed"
        );
        assert_eq!(s.configured_library_root(), Some("~/Music"));

        // …and it survives the round trip that Settings performs.
        s.save_to(&state_path(&dir)).expect("save");
        assert_eq!(AppState::load_from(&state_path(&dir)), s);
    }

    /// The whole point of `library_root` living in `state.json`: resolution reads it back.
    #[test]
    fn a_configured_root_drives_resolution() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let music = home.join("Music");
        std::fs::create_dir_all(&music).expect("mkdir");
        let s = AppState {
            library_root: Some("~/Music".to_string()),
            ..AppState::default()
        };
        assert_eq!(
            paths::resolve_library_root(None, s.configured_library_root(), home),
            music
        );
        assert_eq!(
            paths::resolve_library_root(None, AppState::default().configured_library_root(), home),
            home.join(paths::APP_DIR_NAME)
        );
    }

    #[test]
    fn a_corrupt_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"<<<not json>>>").expect("write");
        assert_eq!(AppState::load_from(&state_path(&dir)), AppState::default());
        // …and saving over it repairs the file.
        AppState::default()
            .save_to(&state_path(&dir))
            .expect("save");
        assert_eq!(AppState::load_from(&state_path(&dir)), AppState::default());
    }

    #[test]
    fn partial_json_keeps_defaults_for_the_missing_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, br#"{ "shuffle": true, "repeat": "All" }"#).expect("write");
        let s = AppState::load_from(&state_path(&dir));
        assert!(s.shuffle);
        assert_eq!(s.repeat, Repeat::All);
        assert_eq!(s.volume, DEFAULT_VOLUME);
        assert_eq!(s.last_view, DEFAULT_VIEW);
    }

    /// A missing file is a fresh install and stays silent; a file that exists but cannot be
    /// read is a real problem and must say so instead of quietly resetting the settings.
    #[test]
    fn an_unreadable_file_warns_and_a_missing_one_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, [0x7b, 0xff, 0x7d]).expect("write"); // `{`, invalid byte, `}`

        let lines =
            logged(|| assert_eq!(AppState::load_from(&state_path(&dir)), AppState::default()));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("state.json") && l.contains("cannot be read")),
            "an unreadable state file must be reported: {lines:?}"
        );

        // …and the file is left alone for the user to inspect.
        assert_eq!(std::fs::read(&path).expect("read"), [0x7b, 0xff, 0x7d]);
    }

    #[test]
    fn insane_values_are_sanitized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = state_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            br#"{ "volume": 9.5, "last_view": "  ", "window": [0.0, 900.0] }"#,
        )
        .expect("write");
        let s = AppState::load_from(&state_path(&dir));
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.last_view, DEFAULT_VIEW);
        assert!(s.window.is_none());
    }
}
