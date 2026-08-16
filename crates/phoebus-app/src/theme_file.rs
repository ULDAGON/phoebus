//! Follow a theme file the desktop writes — the Omarchy bridge.
//!
//! Omarchy renders every template in `~/.config/omarchy/themed/` on each theme switch and
//! atomically swaps the results into `~/.local/state/omarchy/current/theme/`. With the
//! bridge template from `contrib/omarchy` installed, that directory gains a `phoebus.toml`
//! spelling the desktop's palette in Phoebus's own tokens, and this module notices — at
//! start-up and then by polling. Polling rather than a watcher on purpose: it is one
//! `stat` per second ([`theme::THEME_FILE_POLL_MS`]), it needs no new dependency, and it
//! shrugs at the directory swap that would make an inotify watch dance.
//!
//! Precedence, and what persists:
//!
//! * `PHOEBUS_THEME` (an explicit ask for one run) beats the file — the file is not even
//!   looked for.
//! * The file beats `state.json` — but never *writes* it. Settings picks made while the
//!   file is followed apply on top (and persist as usual); the file re-asserts itself on
//!   its next change, and when it disappears the palette falls back to `state.json`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::theme::{self, ExternalTheme};

/// Points at the theme file to follow instead of the Omarchy locations, on any platform.
/// Set but empty disables the integration for the run.
pub const ENV_THEME_FILE: &str = "PHOEBUS_THEME_FILE";

/// What the Omarchy template renders (`contrib/omarchy/phoebus.toml.tpl`).
const FILE_NAME: &str = "phoebus.toml";

/// Consecutive misses before the file counts as gone. `omarchy-theme-set` replaces the
/// whole `current/theme` directory with an `rm -rf` + `mv`, so a single poll can land in
/// the gap between the two; a real removal survives a second look, one poll later.
const MISSES_BEFORE_GONE: u8 = 2;

/// What one poll turned up.
#[derive(Clone, Debug, PartialEq)]
pub enum ThemeFileEvent {
    /// The file appeared or changed, and holds at least one usable value.
    Changed(ExternalTheme),
    /// The file the palette was following is gone: fall back to the persisted theme.
    Removed,
}

/// One file identity: which path answered, and the `(mtime, len)` pair that changes when
/// the content does. Omarchy swaps a fresh directory in every time, so the mtime is new
/// on every switch and its granularity is never what a change hides behind.
type Identity = (PathBuf, SystemTime, u64);

/// The follower. Owned by the app, polled from its frame loop.
pub struct ThemeFile {
    /// Paths tried in order, first hit wins. Empty when the integration is off — no
    /// Linux, `PHOEBUS_THEME` pinning the run, or `PHOEBUS_THEME_FILE=` saying don't.
    candidates: Vec<PathBuf>,
    /// The identity last examined (whether or not it parsed to a theme).
    seen: Option<Identity>,
    /// Whether the palette currently follows the file — what decides if a disappearance
    /// is worth a [`ThemeFileEvent::Removed`].
    applied: bool,
    misses: u8,
    last_poll: Option<Instant>,
}

impl ThemeFile {
    /// Work out which file to follow, from the environment.
    pub fn discover() -> ThemeFile {
        if theme::env_override().is_some() {
            // An explicit `PHOEBUS_THEME` pins the run; `theme::resolve` already said so.
            return ThemeFile::with_candidates(Vec::new());
        }
        let candidates = match std::env::var(ENV_THEME_FILE) {
            Ok(path) if path.trim().is_empty() => {
                log::info!("{ENV_THEME_FILE} is empty: not following any theme file");
                Vec::new()
            }
            Ok(path) => vec![PathBuf::from(path.trim())],
            // The default: Omarchy's current-theme directory, newest layout first (the
            // state dir arrived with Omarchy 3; older installs kept it under config).
            // Linux only — Omarchy is a Linux desktop, and where the file cannot exist
            // the app should not keep a 1 Hz poll (and its repaint) alive for it.
            Err(_) if cfg!(target_os = "linux") => {
                let home = phoebus_core::home_dir();
                let state = xdg_dir("XDG_STATE_HOME", home.join(".local/state"));
                let config = xdg_dir("XDG_CONFIG_HOME", home.join(".config"));
                vec![
                    state.join("omarchy/current/theme").join(FILE_NAME),
                    config.join("omarchy/current/theme").join(FILE_NAME),
                ]
            }
            Err(_) => Vec::new(),
        };
        ThemeFile::with_candidates(candidates)
    }

    /// Follow an explicit list of paths. [`ThemeFile::discover`] ends here, and so do the
    /// tests, which cannot reach into the environment.
    pub fn with_candidates(candidates: Vec<PathBuf>) -> ThemeFile {
        ThemeFile {
            candidates,
            seen: None,
            applied: false,
            misses: 0,
            last_poll: None,
        }
    }

    /// True when there is anything to poll — i.e. the app should keep waking up for it.
    pub fn active(&self) -> bool {
        !self.candidates.is_empty()
    }

    /// The file the palette follows right now.
    pub fn source(&self) -> Option<&Path> {
        if !self.applied {
            return None;
        }
        self.seen.as_ref().map(|(path, ..)| path.as_path())
    }

    /// Throttled [`ThemeFile::check`]: at most one stat pass per
    /// [`theme::THEME_FILE_POLL_MS`], however fast the frames come.
    pub fn poll(&mut self) -> Option<ThemeFileEvent> {
        if self.candidates.is_empty() {
            return None;
        }
        let now = Instant::now();
        let gap = Duration::from_millis(theme::THEME_FILE_POLL_MS);
        if self
            .last_poll
            .is_some_and(|at| now.duration_since(at) < gap)
        {
            return None;
        }
        self.last_poll = Some(now);
        self.check()
    }

    /// One unthrottled look at the candidates. Called directly at start-up, where the
    /// first frame must already paint the desktop's palette.
    pub fn check(&mut self) -> Option<ThemeFileEvent> {
        let found = self.candidates.iter().find_map(identity_of);
        match found {
            None if self.seen.is_none() => None,
            None => {
                // One miss can be `omarchy-theme-set` mid-swap; see MISSES_BEFORE_GONE.
                self.misses += 1;
                if self.misses < MISSES_BEFORE_GONE {
                    return None;
                }
                self.seen = None;
                self.misses = 0;
                if std::mem::take(&mut self.applied) {
                    log::info!("theme file: gone; back to the persisted theme");
                    Some(ThemeFileEvent::Removed)
                } else {
                    None
                }
            }
            Some(id) => {
                self.misses = 0;
                if self.seen.as_ref() == Some(&id) {
                    return None;
                }
                let event = self.read(&id.0);
                self.seen = Some(id);
                event
            }
        }
    }

    /// Read and parse one file that is new or changed. A file that cannot be read or
    /// holds nothing usable is a removal if a theme was being followed, and simply not a
    /// theme yet otherwise.
    fn read(&mut self, path: &Path) -> Option<ThemeFileEvent> {
        let parsed = match std::fs::read_to_string(path) {
            Ok(text) => theme::parse_external(&text),
            Err(e) => {
                log::warn!("theme file: {} cannot be read ({e})", path.display());
                ExternalTheme::default()
            }
        };
        if parsed.is_empty() {
            log::warn!("theme file: {} holds no usable theme", path.display());
            return std::mem::take(&mut self.applied).then_some(ThemeFileEvent::Removed);
        }
        log::info!("theme file: following {}", path.display());
        self.applied = true;
        Some(ThemeFileEvent::Changed(parsed))
    }
}

/// An XDG base directory: `$var` when it is set the way the spec means it (absolute,
/// non-empty), the conventional default otherwise.
fn xdg_dir(var: &str, default: PathBuf) -> PathBuf {
    match std::env::var(var) {
        Ok(dir) if dir.starts_with('/') => PathBuf::from(dir),
        _ => default,
    }
}

/// The identity of `path`, if it is a readable file right now.
fn identity_of(path: &PathBuf) -> Option<Identity> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    // A filesystem without mtimes still gets change detection through the length.
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Some((path.clone(), mtime, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use egui::Color32;

    /// A follower over `n` paths inside a fresh tempdir.
    fn follower(dir: &tempfile::TempDir, names: &[&str]) -> ThemeFile {
        ThemeFile::with_candidates(names.iter().map(|n| dir.path().join(n)).collect())
    }

    #[test]
    fn nothing_to_follow_is_quiet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut tf = follower(&dir, &["phoebus.toml"]);
        assert!(tf.active());
        assert_eq!(tf.check(), None);
        assert_eq!(tf.check(), None, "still nothing, still quiet");
        assert_eq!(tf.source(), None);

        let mut off = ThemeFile::with_candidates(Vec::new());
        assert!(!off.active());
        assert_eq!(off.poll(), None);
    }

    #[test]
    fn the_first_existing_candidate_wins_and_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut tf = follower(&dir, &["a.toml", "b.toml"]);
        std::fs::write(dir.path().join("b.toml"), "accent = \"#7AA2F7\"\n").expect("write");
        std::fs::write(
            dir.path().join("a.toml"),
            "mode = \"dark\"\naccent = \"#FF0000\"\n",
        )
        .expect("write");
        match tf.check() {
            Some(ThemeFileEvent::Changed(ext)) => {
                assert_eq!(ext.mode, Some(phoebus_core::ThemeMode::Dark));
                assert_eq!(ext.accent, Some(Color32::from_rgb(0xFF, 0x00, 0x00)));
            }
            other => panic!("expected a.toml's theme, got {other:?}"),
        }
        assert_eq!(tf.source(), Some(dir.path().join("a.toml").as_path()));
        assert_eq!(tf.check(), None, "unchanged file, no event");
    }

    #[test]
    fn a_content_change_is_noticed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phoebus.toml");
        let mut tf = follower(&dir, &["phoebus.toml"]);

        std::fs::write(&path, "accent = \"#7AA2F7\"\n").expect("write");
        assert!(matches!(tf.check(), Some(ThemeFileEvent::Changed(_))));

        // A different byte length changes the identity even where the filesystem's mtime
        // granularity would hide a same-second rewrite.
        std::fs::write(&path, "accent = \"#E0AF68\"\nbg0 = \"#1A1B26\"\n").expect("write");
        match tf.check() {
            Some(ThemeFileEvent::Changed(ext)) => {
                assert_eq!(ext.accent, Some(Color32::from_rgb(0xE0, 0xAF, 0x68)));
                assert_eq!(ext.ramp.bg0, Some(Color32::from_rgb(0x1A, 0x1B, 0x26)));
            }
            other => panic!("expected the rewritten theme, got {other:?}"),
        }
    }

    /// The `rm -rf` + `mv` swap in `omarchy-theme-set` can make the path vanish for one
    /// poll; only a repeated miss is a removal, and only if a theme was being followed.
    #[test]
    fn removal_takes_two_misses_and_a_followed_theme() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phoebus.toml");
        let mut tf = follower(&dir, &["phoebus.toml"]);

        std::fs::write(&path, "bg0 = \"#101010\"\n").expect("write");
        assert!(matches!(tf.check(), Some(ThemeFileEvent::Changed(_))));

        std::fs::remove_file(&path).expect("remove");
        assert_eq!(tf.check(), None, "first miss could be a mid-swap gap");

        // The file coming back between the two misses is a swap, not a removal.
        std::fs::write(&path, "bg0 = \"#202020\"\n").expect("write");
        assert!(matches!(tf.check(), Some(ThemeFileEvent::Changed(_))));

        std::fs::remove_file(&path).expect("remove");
        assert_eq!(tf.check(), None);
        assert_eq!(tf.check(), Some(ThemeFileEvent::Removed));
        assert_eq!(tf.source(), None);
        assert_eq!(tf.check(), None, "a removal is reported once");
    }

    #[test]
    fn junk_never_becomes_a_theme_and_unfollows_a_followed_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phoebus.toml");
        let mut tf = follower(&dir, &["phoebus.toml"]);

        std::fs::write(&path, "not a theme at all\n").expect("write");
        assert_eq!(
            tf.check(),
            None,
            "junk before any theme is just not a theme"
        );
        assert_eq!(tf.source(), None);

        std::fs::write(&path, "accent = \"#7AA2F7\"\n# now a theme\n").expect("write");
        assert!(matches!(tf.check(), Some(ThemeFileEvent::Changed(_))));

        std::fs::write(
            &path,
            "ceci n'est pas un theme -- and long enough to differ\n",
        )
        .expect("write");
        assert_eq!(
            tf.check(),
            Some(ThemeFileEvent::Removed),
            "a followed file degrading to junk must fall back"
        );
    }

    /// End to end into the palette math: the parsed file drives `palette_with`.
    #[test]
    fn a_parsed_file_lands_in_the_palette() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phoebus.toml");
        std::fs::write(
            &path,
            "mode = \"dark\"\naccent = \"#7AA2F7\"\nbg0 = \"#1A1B26\"\n",
        )
        .expect("write");
        let mut tf = follower(&dir, &["phoebus.toml"]);
        let Some(ThemeFileEvent::Changed(ext)) = tf.check() else {
            panic!("expected a theme");
        };
        let mode = ext.mode.expect("mode");
        let p = theme::palette_with(mode, ext.accent.expect("accent"), ext.ramp_for(mode));
        assert_eq!(p.bg0, Color32::from_rgb(0x1A, 0x1B, 0x26));
        assert_eq!(p.accent, Color32::from_rgb(0x7A, 0xA2, 0xF7));
        assert!(
            theme::contrast(p.accent_text, p.bg0) >= 4.5,
            "the contrast guarantee holds against the overridden bg0"
        );
    }
}
