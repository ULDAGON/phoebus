//! Filesystem layout: library root resolution, app-data paths, atomic writes, hashing.
//!
//! Two directories, deliberately independent:
//!
//! * the **library root** — a directory of `Artist/Album/track` files that Phoebus only ever
//!   reads. It is configurable (`$PHOEBUS_LIBRARY` > `library_root` in `state.json` >
//!   `~/.phoebus`) and may be someone's Apple Music folder;
//! * the **data dir** ([`Dirs`]) — the fixed `~/.phoebus/.phoebus/`, overridable with
//!   `$PHOEBUS_DATA`, holding `state.json`, `playlists.json`, `favorites.json` and
//!   `cache/covers/`. Phoebus never writes inside a configured library root.
//!
//! The default library root is `~/.phoebus` and its `.phoebus` subdirectory is exactly the
//! default data dir, so an install that never configures a root keeps the old layout
//! byte-for-byte — no migration code, and no user data left behind.
//!
//! There is no root-relative path API any more: [`Dirs`] is the only thing that knows the
//! layout, and [`Dirs::inside`] is the one place that still says "the data dir that belongs
//! to this root" (which is what a self-contained scan of a throwaway directory wants).

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// Name of the app-data directory, a dot-directory *inside* the library root.
pub const APP_DIR_NAME: &str = ".phoebus";
/// Environment variable that overrides the library root (used by tests and power users).
pub const LIBRARY_ENV: &str = "PHOEBUS_LIBRARY";
/// Environment variable that overrides the app-data directory.
///
/// Tests, the selftest and the screenshot tour MUST set it, so they can never touch the
/// real `~/.phoebus/.phoebus`.
pub const DATA_ENV: &str = "PHOEBUS_DATA";

/// The app-data directory: `$PHOEBUS_DATA` if set and non-empty, else `~/.phoebus/.phoebus`.
///
/// Independent of the library root. The environment value is used verbatim (no `~`
/// expansion — a shell has already done that); if the home directory cannot be determined
/// it falls back to a relative `.phoebus/.phoebus`.
pub fn data_dir() -> PathBuf {
    resolve_data_dir(std::env::var_os(DATA_ENV), dirs::home_dir())
}

fn resolve_data_dir(env: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    if let Some(v) = env.filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let base = match home {
        Some(h) => h.join(APP_DIR_NAME),
        None => PathBuf::from(APP_DIR_NAME),
    };
    base.join(APP_DIR_NAME)
}

/// The app-data directory and everything inside it.
///
/// Build it once at startup — `Dirs::resolve()` in the app, `Dirs::at(tmp)` in tests — and
/// pass it down; nothing below this type reads the environment again, so a test can point
/// the whole app at a temp directory without touching globals.
///
/// ```
/// # use phoebus_core::paths::Dirs;
/// let dirs = Dirs::at("/tmp/phoebus-data");
/// assert_eq!(dirs.state_path(), std::path::Path::new("/tmp/phoebus-data/state.json"));
/// assert_eq!(
///     dirs.covers_dir(),
///     std::path::Path::new("/tmp/phoebus-data/cache/covers")
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    data: PathBuf,
}

impl Dirs {
    /// The real app-data directory: `$PHOEBUS_DATA`, else `~/.phoebus/.phoebus` (see
    /// [`data_dir`]).
    pub fn resolve() -> Dirs {
        Dirs::at(data_dir())
    }

    /// An explicit data directory — how tests stay out of the user's home.
    pub fn at(data_dir: impl Into<PathBuf>) -> Dirs {
        Dirs {
            data: data_dir.into(),
        }
    }

    /// The data directory that *belongs to* `root`: `<root>/.phoebus`.
    ///
    /// This is the pre-v1.1 layout, where app data lived inside the library root. It is
    /// still exactly right for the default root — `~/.phoebus/.phoebus` **is** the default
    /// data dir — and it is what a self-contained scan of a throwaway directory wants
    /// ([`crate::scanner::scan`], [`crate::Library::build`]).
    ///
    /// The app never calls it: it resolves the data dir independently with [`Dirs::resolve`],
    /// so a configured library root is only ever read from.
    pub fn inside(root: &Path) -> Dirs {
        Dirs::at(root.join(APP_DIR_NAME))
    }

    /// The data directory itself.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// `<data>/state.json`
    pub fn state_path(&self) -> PathBuf {
        self.data.join("state.json")
    }

    /// `<data>/playlists.json`
    pub fn playlists_path(&self) -> PathBuf {
        self.data.join("playlists.json")
    }

    /// `<data>/favorites.json`
    pub fn favorites_path(&self) -> PathBuf {
        self.data.join("favorites.json")
    }

    /// `<data>/cache/covers` — one PNG per album, named by [`fnv1a_64`] of the album key.
    pub fn covers_dir(&self) -> PathBuf {
        self.data.join("cache").join("covers")
    }

    /// Create the data directory and its cover cache (and their parents) if missing.
    pub fn ensure_dirs(&self) -> Result<()> {
        let covers = self.covers_dir();
        fs::create_dir_all(&covers).with_context(|| format!("creating {}", covers.display()))
    }
}

/// The home directory, as every path decision in Phoebus sees it.
///
/// Empty when the platform will not say — exactly the fall-back [`library_root_for`] uses, so
/// the UI and the resolver can never disagree about what `~` means. It is also the only way
/// the app crate gets at the home directory: `dirs` is a core dependency, not a UI one.
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// Expand a leading `~` (alone, or followed by `/`) against `home`; everything else is
/// taken verbatim. `~user` is **not** expanded — it is a literal directory name here.
pub fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    let p = path.trim();
    if p == "~" {
        return home.to_path_buf();
    }
    match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(p),
    }
}

/// `~/.phoebus` — the library root used when nothing is configured.
pub fn default_library_root(home: &Path) -> PathBuf {
    home.join(APP_DIR_NAME)
}

/// Resolve the library root from the two things that can set it, in priority order:
///
/// 1. `env_override` (`$PHOEBUS_LIBRARY`) — wins unconditionally and is never validated, so
///    a deliberate override still works before the directory exists;
/// 2. `configured` (`library_root` from `state.json`, set in Settings) — `~` is expanded,
///    then it must be absolute *and* an existing directory. Anything else is a settings
///    file that no longer matches the disk, so it falls back to the default with a
///    `log::warn!` instead of leaving the user with an empty library and no explanation;
/// 3. otherwise [`default_library_root`] (`~/.phoebus`).
///
/// Pure with respect to the environment: every input is a parameter, which is what makes it
/// unit-testable. It does stat `configured` — that check is the whole point.
pub fn resolve_library_root(
    env_override: Option<&str>,
    configured: Option<&str>,
    home: &Path,
) -> PathBuf {
    if let Some(env) = env_override.map(str::trim).filter(|v| !v.is_empty()) {
        return expand_tilde(env, home);
    }
    let default = default_library_root(home);
    let Some(configured) = configured.map(str::trim).filter(|v| !v.is_empty()) else {
        return default;
    };
    let path = expand_tilde(configured, home);
    if !path.is_absolute() {
        log::warn!(
            "library: configured root {configured} is not an absolute path; using {}",
            default.display()
        );
        return default;
    }
    if !path.is_dir() {
        log::warn!(
            "library: configured root {} is not a directory; using {}",
            path.display(),
            default.display()
        );
        return default;
    }
    path
}

/// [`resolve_library_root`] wired to the real environment and home directory.
///
/// `configured` is [`crate::AppState::library_root`]. A `$PHOEBUS_LIBRARY` that is not valid
/// UTF-8 is used exactly as the OS gave it (no `~` expansion is possible then).
pub fn library_root_for(configured: Option<&str>) -> PathBuf {
    let home = home_dir();
    match std::env::var_os(LIBRARY_ENV).filter(|v| !v.is_empty()) {
        Some(v) => match v.to_str() {
            Some(s) => expand_tilde(s, &home),
            None => PathBuf::from(v),
        },
        None => resolve_library_root(None, configured, &home),
    }
}

/// Write `bytes` to `path` atomically: write a sibling `*.tmp` file, fsync, then rename.
///
/// Creates the parent directory if needed. A crash mid-write can never leave a
/// half-written `playlists.json` / `state.json` behind.
///
/// The temp file's name carries this process's id and a per-call counter, so two Phoebus
/// processes sharing a library root (a `--shot` run next to an open window, two windows)
/// cannot write into each other's temp file and publish a splice of both payloads. It still
/// lives in the destination's directory, which is what keeps the rename atomic.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = unique_tmp_path(path);
    let result = write_then_rename(&tmp, path, bytes);
    if result.is_err() {
        // Unique names never get reused, so a failed write must clean up after itself.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `<path>.<pid>.<counter>.tmp`, next to `path`.
fn unique_tmp_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

fn write_then_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::File::create(tmp).with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.sync_all()
        .with_context(|| format!("syncing {}", tmp.display()))?;
    drop(f);

    fs::rename(tmp, path).with_context(|| format!("renaming into {}", path.display()))
}

/// FNV-1a, 64-bit. Stable across processes and runs (unlike `DefaultHasher`, which is
/// randomly seeded), which is what makes [`crate::TrackId`]s and cover file names durable.
pub const fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(PRIME);
        i += 1;
    }
    hash
}

/// Normalize a library-relative path into the canonical string form used for hashing and
/// for playlist entries: `/`-separated, no leading `./` or `/`.
///
/// `\` is **not** a separator: Phoebus ships on macOS and Linux only, where a backslash is
/// an ordinary character in a file name (`AC\DC`, `Bad\Ass.mp3`). Rewriting it would invent
/// directories that do not exist and make the file unplayable.
pub fn normalize_rel(rel: &str) -> String {
    let mut s: &str = rel;
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    s.trim_start_matches('/').to_string()
}

/// Library-relative, `/`-separated string form of `path` (which must be under `root`).
///
/// Returns `None` if `path` is not inside `root`.
pub fn rel_path_string(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for comp in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_known_vectors() {
        // Canonical FNV-1a 64 test vectors — if these ever change, every TrackId and every
        // cached cover file name in every user's library breaks.
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(
            fnv1a_64(b"HOME/Odyssey/01 Intro.m4a"),
            0x9835_19c8_dd02_5be3
        );
    }

    #[test]
    fn data_dir_prefers_the_env_and_defaults_next_to_the_default_root() {
        assert_eq!(
            resolve_data_dir(
                Some(OsString::from("/tmp/phoebus-data")),
                Some(PathBuf::from("/home/nobody"))
            ),
            PathBuf::from("/tmp/phoebus-data")
        );
        // The default data dir is the `.phoebus` *inside* the default library root, so an
        // install that never configures a root keeps the pre-v1.1 layout.
        let home = PathBuf::from("/home/nobody");
        assert_eq!(
            resolve_data_dir(None, Some(home.clone())),
            PathBuf::from("/home/nobody/.phoebus/.phoebus")
        );
        assert_eq!(
            Dirs::at(resolve_data_dir(None, Some(home.clone()))),
            Dirs::inside(&default_library_root(&home)),
            "default data dir == <default root>/.phoebus — no migration, ever"
        );
        assert_eq!(
            resolve_data_dir(Some(OsString::new()), Some(home)),
            PathBuf::from("/home/nobody/.phoebus/.phoebus")
        );
        assert_eq!(
            resolve_data_dir(None, None),
            PathBuf::from(".phoebus/.phoebus")
        );
    }

    #[test]
    fn dirs_lays_out_the_data_directory() {
        let d = Dirs::at("/data");
        assert_eq!(d.data_dir(), Path::new("/data"));
        assert_eq!(d.state_path(), PathBuf::from("/data/state.json"));
        assert_eq!(d.playlists_path(), PathBuf::from("/data/playlists.json"));
        assert_eq!(d.favorites_path(), PathBuf::from("/data/favorites.json"));
        assert_eq!(d.covers_dir(), PathBuf::from("/data/cache/covers"));
        assert_eq!(d, Dirs::at(PathBuf::from("/data")));

        // The one remaining root-relative shape: the pre-v1.1 in-root layout.
        let inside = Dirs::inside(Path::new("/lib"));
        assert_eq!(inside.data_dir(), Path::new("/lib/.phoebus"));
        assert_eq!(
            inside.covers_dir(),
            PathBuf::from("/lib/.phoebus/cache/covers")
        );
        assert_eq!(
            inside.playlists_path(),
            PathBuf::from("/lib/.phoebus/playlists.json")
        );
        assert_eq!(
            inside.favorites_path(),
            PathBuf::from("/lib/.phoebus/favorites.json")
        );
        assert_eq!(
            inside.state_path(),
            PathBuf::from("/lib/.phoebus/state.json")
        );
    }

    #[test]
    fn dirs_ensure_creates_the_cover_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let d = Dirs::at(tmp.path().join("nested").join("data"));
        assert!(!d.covers_dir().exists());
        d.ensure_dirs().expect("ensure");
        assert!(d.covers_dir().is_dir());
        d.ensure_dirs().expect("ensure is idempotent");
    }

    #[test]
    fn tilde_expands_only_at_the_front() {
        let home = Path::new("/home/nobody");
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/nobody"));
        assert_eq!(
            expand_tilde("~/Music/Media", home),
            PathBuf::from("/home/nobody/Music/Media")
        );
        assert_eq!(
            expand_tilde("  ~/Music  ", home),
            PathBuf::from("/home/nobody/Music")
        );
        assert_eq!(expand_tilde("/abs/path", home), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("rel/path", home), PathBuf::from("rel/path"));
        assert_eq!(
            expand_tilde("~other/Music", home),
            PathBuf::from("~other/Music"),
            "~user is a literal directory name, not a home lookup"
        );
        assert_eq!(
            expand_tilde("/a/~/b", home),
            PathBuf::from("/a/~/b"),
            "only a leading tilde expands"
        );
    }

    #[test]
    fn library_root_priority_is_env_then_configured_then_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let music = home.join("Music").join("Media");
        fs::create_dir_all(&music).expect("mkdir");

        // 1. env wins over a perfectly good configured path, and is not validated.
        assert_eq!(
            resolve_library_root(
                Some("/nowhere/at/all"),
                Some(music.to_str().expect("utf8")),
                home
            ),
            PathBuf::from("/nowhere/at/all")
        );
        // …and an empty env var is "not set".
        assert_eq!(
            resolve_library_root(Some("  "), Some(music.to_str().expect("utf8")), home),
            music
        );
        // 2. a configured directory that exists is used, with `~` expanded against home.
        assert_eq!(
            resolve_library_root(None, Some("~/Music/Media"), home),
            music
        );
        // 3. nothing configured -> the default.
        assert_eq!(
            resolve_library_root(None, None, home),
            home.join(APP_DIR_NAME)
        );
        assert_eq!(
            resolve_library_root(None, Some(""), home),
            home.join(APP_DIR_NAME)
        );
    }

    #[test]
    fn a_configured_root_that_cannot_be_used_falls_back_and_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let default = home.join(APP_DIR_NAME);
        let file = home.join("not-a-dir.txt");
        fs::write(&file, b"x").expect("write");

        for configured in [
            "relative/path",                          // not absolute after expansion
            "/definitely/does/not/exist/phoebus-lib", // gone
            file.to_str().expect("utf8"),             // a file, not a directory
        ] {
            assert_eq!(
                resolve_library_root(None, Some(configured), home),
                default,
                "{configured} must fall back to the default root"
            );
        }
    }

    #[test]
    fn normalize_and_relativize() {
        assert_eq!(normalize_rel("./a/b.mp3"), "a/b.mp3");
        assert_eq!(normalize_rel("/a/b.mp3"), "a/b.mp3");
        assert_eq!(normalize_rel("//a/b.mp3"), "a/b.mp3");
        assert_eq!(normalize_rel("././a/b.mp3"), "a/b.mp3");
        let root = Path::new("/lib");
        assert_eq!(
            rel_path_string(root, Path::new("/lib/A/B/c.mp3")).as_deref(),
            Some("A/B/c.mp3")
        );
        assert!(rel_path_string(root, Path::new("/other/c.mp3")).is_none());
    }

    /// A backslash is an ordinary character in a POSIX file name (`AC\DC`, `Bad\Ass.mp3`),
    /// and Phoebus only ships on macOS and Linux — rewriting it to `/` invents directories
    /// that do not exist and makes the file unplayable.
    #[test]
    fn a_backslash_is_a_file_name_character_not_a_separator() {
        assert_eq!(normalize_rel("a\\b.mp3"), "a\\b.mp3");
        assert_eq!(
            normalize_rel("./AC\\DC/Back in Black/01 Hells Bells.mp3"),
            "AC\\DC/Back in Black/01 Hells Bells.mp3"
        );
        let root = Path::new("/lib");
        assert_eq!(
            rel_path_string(
                root,
                Path::new("/lib/AC\\DC/Back in Black/01 Hells Bells.mp3")
            )
            .as_deref(),
            Some("AC\\DC/Back in Black/01 Hells Bells.mp3"),
            "components are joined with '/', so the backslash stays inside one component"
        );
    }

    /// The temp file must belong to *this* writer. A second Phoebus process (the `--shot`
    /// tour next to an open window) writes the same destinations at the same time.
    #[test]
    fn atomic_write_does_not_touch_another_writers_tmp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("playlists.json");
        // Exactly the name the old fixed scheme would have picked.
        let other = p.with_extension("json.tmp");
        fs::write(&other, b"another process is mid-write here").expect("write");

        write_atomic(&p, b"mine").expect("write");
        assert_eq!(fs::read_to_string(&p).expect("read"), "mine");
        assert_eq!(
            fs::read_to_string(&other).expect("read"),
            "another process is mid-write here",
            "the other writer's temp file must be untouched"
        );
    }

    /// Concurrent writers may only ever publish one writer's bytes, never a splice of two.
    #[test]
    fn concurrent_writes_publish_one_whole_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("state.json");
        let payloads: Vec<Vec<u8>> = (1u8..=6)
            .map(|n| vec![b'a' + n; usize::from(n) * 40_000])
            .collect();

        for _round in 0..8 {
            std::thread::scope(|s| {
                for payload in &payloads {
                    s.spawn(|| write_atomic(&p, payload).expect("write"));
                }
            });
            let got = fs::read(&p).expect("read");
            assert!(
                payloads.contains(&got),
                "published a mixture: {} bytes, starts with {:?}, ends with {:?}",
                got.len(),
                got.first(),
                got.last()
            );
        }

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("sub").join("f.json");
        write_atomic(&p, b"one").expect("write");
        assert_eq!(fs::read_to_string(&p).expect("read"), "one");
        write_atomic(&p, b"two").expect("write");
        assert_eq!(fs::read_to_string(&p).expect("read"), "two");
        let leftovers: Vec<_> = fs::read_dir(p.parent().expect("parent"))
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
    }
}
