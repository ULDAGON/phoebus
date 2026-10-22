//! The library snapshot: the tracks of the last finished scan, kept as `library.json` in the
//! app-data dir ([`Dirs::library_path`](crate::paths::Dirs::library_path)) so the next
//! launch can show the library before it has opened a single audio file.
//!
//! Why it exists: a scan opens every file to read its tags. That is I/O, not CPU, and on a
//! few thousand tracks it is seconds — seconds the app used to spend behind a scanning
//! screen on every launch. With a snapshot, launch is one JSON parse; the scan still runs,
//! in the background, and hands the snapshot's tracks to
//! [`rescan_with_covers_progress`](crate::scanner::rescan_with_covers_progress), which only
//! opens the files whose size or mtime moved.
//!
//! Rules:
//!
//! * **A cache, never a source of truth.** A snapshot that is missing, unreadable, corrupt,
//!   from another [`VERSION`] or of another library root yields `None`, and the caller does
//!   the ordinary full scan. It is never an error and never repaired in place: the next
//!   finished scan simply overwrites it.
//! * **[`VERSION`] is the single gate.** Any change to what a stored track means — a field
//!   added, removed or reinterpreted — bumps it, and a file with any other version is
//!   ignored whole. No migrations, no half-read files; the header is checked before the
//!   tracks are even parsed, so an incompatible file is reported as such and not as corrupt.
//! * **The root is part of the key.** A snapshot of another root is ignored, so switching
//!   the root in Settings can never flash the old root's music under the new one's name.
//! * **Written atomically** ([`paths::write_atomic`]), like every other file Phoebus owns: a
//!   crash mid-save leaves the previous snapshot, never half of a new one.
//! * **A snapshot of nothing is nothing.** An empty library is never written and an empty
//!   file is never loaded: an unmounted music volume scans as zero tracks, and that must
//!   neither destroy the good snapshot nor make the next launch believe it has a library
//!   to show.
//! * **Saves are serialised and re-checked** ([`save_scan`]): a scan the app has given up on
//!   cannot publish its snapshot after the scan that replaced it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Library, Track};
use crate::paths;

/// Format version of `library.json`. Bump it whenever the stored form of a [`Track`]
/// changes; every older (and newer) snapshot is then ignored and rebuilt by one full scan.
///
/// 2: a track stores its own cover ([`Track::cover`], a content hash) instead of a
/// `has_artwork` flag. A version-1 file cannot say which picture a track carries, and
/// reusing its tracks would leave every song without a cover until its file changed.
pub const VERSION: u32 = 2;

/// The longest track a snapshot may hold: a year. No audio file is longer; a snapshot that
/// claims one is damaged or hostile, and is passed over like any other corrupt file (a
/// rescan reads the real value) instead of feeding absurd sums to every view.
const MAX_TRACK_DURATION: Duration = Duration::from_secs(366 * 24 * 60 * 60);

/// Held from the last "is this save still wanted?" check to the rename that publishes it.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// The file as written. Borrowed, so saving a library clones nothing.
#[derive(Serialize)]
struct Out<'a> {
    version: u32,
    root: &'a Path,
    tracks: Vec<&'a Track>,
}

/// The two fields that decide whether the rest is worth parsing. serde skips the track
/// array while reading this, without building a single track.
#[derive(Deserialize)]
struct Header {
    version: u32,
    root: PathBuf,
}

/// The payload, parsed only once the header has been accepted.
#[derive(Deserialize)]
struct Body {
    tracks: Vec<Track>,
}

/// Write `library` to `path` (normally `<data>/library.json`), atomically.
///
/// Compact JSON rather than the pretty form `state.json` uses: this file is thousands of
/// tracks long, nobody edits it by hand, and its size is parse time at every launch.
///
/// Fails only on I/O, or when the root is not valid UTF-8 (JSON cannot hold it); the app
/// logs that and carries on without a snapshot.
pub fn save(path: &Path, library: &Library) -> Result<()> {
    let json = encode(library)?;
    let _turn = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    paths::write_atomic(path, &json)
}

fn encode(library: &Library) -> Result<Vec<u8>> {
    let out = Out {
        version: VERSION,
        root: library.root(),
        tracks: library.tracks().collect(),
    };
    serde_json::to_vec(&out).context("encoding the library snapshot")
}

/// What [`save_scan`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    /// The snapshot at the path is now this library.
    Written,
    /// The scan found exactly what it started from, and the file is there: nothing to do.
    Unchanged,
    /// The library is empty. Not written, and the existing file is left alone — see the
    /// module docs.
    Empty,
    /// The caller no longer wants this save (its root changed under the scan).
    Superseded,
}

/// Publish the result of a finished scan as the snapshot, unless there is a reason not to.
///
/// `previous` is what the scan started from — the tracks of the snapshot it was seeded
/// with, or of the library on screen. `superseded` says whether the result is still
/// wanted. It is asked once more right before the file is renamed into place, while holding
/// the lock every save takes, because serialising and syncing are slow enough for the
/// answer to change: without that, the save of a scan of the old root could land after the
/// new root's and put the wrong library under the right name — harmless to the app (the
/// root is part of the key) but a full scan at the next launch.
pub fn save_scan(
    path: &Path,
    previous: &[Track],
    library: &Library,
    superseded: impl Fn() -> bool,
) -> Result<Saved> {
    if library.is_empty() {
        return Ok(Saved::Empty);
    }
    if library.holds_exactly(previous) && path.exists() {
        return Ok(Saved::Unchanged);
    }
    let json = encode(library)?;
    let _turn = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if superseded() {
        return Ok(Saved::Superseded);
    }
    paths::write_atomic(path, &json)?;
    Ok(Saved::Written)
}

/// Delete the temp files that crashed saves left in `dir`: `library.json.<pid>.<n>.tmp`
/// ([`paths::write_atomic`] names them so), and nothing else. Only those not touched for
/// `older_than`, so another running Phoebus's save in flight is safe. Returns how many went.
pub fn remove_stale_temp_files(dir: &Path, older_than: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("library.json.") && name.ends_with(".tmp")) {
            continue;
        }
        let idle = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok());
        if !idle.is_some_and(|idle| idle >= older_than) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => log::warn!("snapshot: stale {name} stayed behind: {e}"),
        }
    }
    removed
}

/// The library stored at `path`, if it is a current snapshot of `root`.
///
/// `covers_dir` is where the rebuilt library looks for its cover cache, exactly as for a
/// scan ([`Library::build_with_covers`]). Every way this can fail is `None` — see the module
/// docs. A missing file is the normal first-launch case and is silent; everything else says
/// why it was passed over.
pub fn load(path: &Path, root: &Path, covers_dir: &Path) -> Option<Library> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!(
                "snapshot: {} cannot be read ({e}); scanning",
                path.display()
            );
            return None;
        }
    };
    let header: Header = match serde_json::from_slice(&bytes) {
        Ok(header) => header,
        Err(e) => {
            log::warn!("snapshot: {} is corrupt ({e}); scanning", path.display());
            return None;
        }
    };
    if header.version != VERSION {
        log::info!(
            "snapshot: {} is version {}, this build reads {VERSION}; scanning",
            path.display(),
            header.version
        );
        return None;
    }
    if header.root != root {
        log::info!(
            "snapshot: {} is of {}, not {}; scanning",
            path.display(),
            header.root.display(),
            root.display()
        );
        return None;
    }
    let body: Body = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(e) => {
            log::warn!("snapshot: {} is corrupt ({e}); scanning", path.display());
            return None;
        }
    };
    if body.tracks.is_empty() {
        log::info!("snapshot: {} holds no tracks; scanning", path.display());
        return None;
    }
    if body.tracks.iter().any(|t| t.duration > MAX_TRACK_DURATION) {
        log::warn!(
            "snapshot: {} has a track longer than a year; scanning",
            path.display()
        );
        return None;
    }
    let tracks = body
        .tracks
        .into_iter()
        .map(|mut t| {
            t.refresh_key();
            t
        })
        .collect();
    Some(Library::build_with_covers(
        root.to_path_buf(),
        covers_dir.to_path_buf(),
        tracks,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CoverId, TrackId};
    use std::time::{Duration, SystemTime};

    fn track(rel: &str, secs: u64, size: u64) -> Track {
        let mut t = Track::new(rel);
        t.duration = Duration::from_millis(secs * 1000 + 321);
        t.added_at = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000 + secs, 123_456_789);
        t.size = size;
        t.year = Some(2014);
        // Every track its own picture, as on a compilation; the top bit set, so the id
        // only round-trips if it is stored as the full u64 and not as a float.
        t.cover = Some(CoverId(0x8000_0000_0000_0000 | size));
        t
    }

    fn library(root: &Path, covers: &Path) -> Library {
        let mut tagged = track("HOME/Odyssey/02 Resonance.m4a", 212, 7_000_000);
        tagged.album_artist = "HOME".to_string();
        tagged.artist = "HOME feat. Nobody".to_string();
        tagged.genre = Some("Synthwave".to_string());
        tagged.refresh_key();
        Library::build_with_covers(
            root,
            covers,
            vec![
                track("HOME/Odyssey/01 Intro.m4a", 61, 1_000_000),
                tagged,
                track("Woodkid/S16/01 Goliath.m4a", 250, 9_000_000),
            ],
        )
    }

    /// Every track of a loaded snapshot equals the one that was saved — including the
    /// derived id and album key, which are not stored but recomputed — and the library
    /// built from them is indexed exactly like the original.
    #[test]
    fn a_snapshot_round_trips_the_library() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = Path::new("/music");
        let saved = library(root, &dirs.covers_dir());
        save(&dirs.library_path(), &saved).expect("save");

        let loaded = load(&dirs.library_path(), root, &dirs.covers_dir()).expect("load");
        assert!(saved.changed_albums(&loaded).is_empty(), "same tracks");
        assert_eq!(loaded.track_count(), 3);
        assert_eq!(loaded.albums(), saved.albums());
        assert_eq!(loaded.tracks_sorted(), saved.tracks_sorted());
        assert_eq!(loaded.root(), root);
        assert_eq!(loaded.covers_dir(), dirs.covers_dir());
        let id = TrackId::for_rel_path("HOME/Odyssey/02 Resonance.m4a");
        assert_eq!(loaded.track(id), saved.track(id));
        for track in saved.tracks() {
            assert_eq!(
                loaded.track(track.id).and_then(|t| t.cover),
                track.cover,
                "{} keeps its own cover",
                track.rel_path
            );
        }
        assert_eq!(loaded.cover_ids(), saved.cover_ids());
    }

    /// A snapshot written before tracks knew their covers is ignored whole, so the first
    /// launch after the upgrade reads every file once and finds every song's picture.
    #[test]
    fn a_snapshot_from_before_per_song_covers_is_ignored() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let v1 = r#"{"version":1,"root":"/music","tracks":[{"rel_path":"HOME/Odyssey/01 Intro.m4a","source_path":null,"title":"Intro","artist":"HOME","album_artist":"HOME","album":"Odyssey","track_no":1,"disc_no":null,"year":null,"genre":null,"duration":{"secs":61,"nanos":0},"has_artwork":true,"added_at":[1700000000,0],"size":1000}]}"#;
        std::fs::write(dirs.library_path(), v1).expect("write");
        const { assert!(VERSION > 1) };
        assert!(
            load(
                &dirs.library_path(),
                Path::new("/music"),
                &dirs.covers_dir()
            )
            .is_none()
        );
    }

    #[test]
    fn a_missing_snapshot_is_none() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        assert!(
            load(
                &dirs.library_path(),
                Path::new("/music"),
                &dirs.covers_dir()
            )
            .is_none()
        );
    }

    /// Switching the root must never show the old root's music.
    #[test]
    fn a_snapshot_of_another_root_is_ignored() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let lib = library(Path::new("/music"), &dirs.covers_dir());
        save(&dirs.library_path(), &lib).expect("save");
        assert!(
            load(
                &dirs.library_path(),
                Path::new("/other/music"),
                &dirs.covers_dir()
            )
            .is_none()
        );
    }

    /// The version is the single gate: a file with any other version is ignored, even
    /// when its tracks would parse — that is what lets a later format change simply bump
    /// [`VERSION`].
    #[test]
    fn a_snapshot_of_another_version_is_ignored() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = Path::new("/music");
        save(&dirs.library_path(), &library(root, &dirs.covers_dir())).expect("save");

        let text = std::fs::read_to_string(dirs.library_path()).expect("read");
        let marker = format!("\"version\":{VERSION}");
        assert!(text.contains(&marker), "{text}");
        for other in [VERSION + 1, VERSION.wrapping_sub(1)] {
            let bumped = text.replacen(&marker, &format!("\"version\":{other}"), 1);
            std::fs::write(dirs.library_path(), bumped).expect("write");
            assert!(
                load(&dirs.library_path(), root, &dirs.covers_dir()).is_none(),
                "version {other} must not load"
            );
        }
    }

    /// A damaged file is passed over, never an error and never a panic: the caller falls
    /// back to a full scan, whose snapshot then replaces it.
    #[test]
    fn a_corrupt_snapshot_is_ignored() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = Path::new("/music");
        save(&dirs.library_path(), &library(root, &dirs.covers_dir())).expect("save");
        let text = std::fs::read_to_string(dirs.library_path()).expect("read");

        let truncated = &text[..text.len() / 2];
        let wrong_shape = format!("{{\"version\":{VERSION},\"root\":\"/music\",\"tracks\":[1,2]}}");
        for bad in [
            truncated,
            "",
            "null",
            "not json at all",
            wrong_shape.as_str(),
        ] {
            std::fs::write(dirs.library_path(), bad).expect("write");
            assert!(
                load(&dirs.library_path(), root, &dirs.covers_dir()).is_none(),
                "{bad:?} must not load"
            );
        }
    }

    /// A valid-JSON snapshot with absurd durations used to overflow the album sums and
    /// panic inside `Phoebus::new`, so that every launch crashed.
    #[test]
    fn a_snapshot_with_absurd_durations_is_ignored_not_a_panic() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = Path::new("/music");
        save(&dirs.library_path(), &library(root, &dirs.covers_dir())).expect("save");
        let text = std::fs::read_to_string(dirs.library_path()).expect("read");

        let mut json: serde_json::Value = serde_json::from_str(&text).expect("json");
        for track in json["tracks"].as_array_mut().expect("tracks") {
            track["duration"] = serde_json::json!({ "secs": 9223372036854775808u64, "nanos": 0 });
        }
        std::fs::write(dirs.library_path(), json.to_string()).expect("write");
        assert!(load(&dirs.library_path(), root, &dirs.covers_dir()).is_none());
    }

    /// An unmounted volume scans as nothing; a snapshot of nothing must not pass for a
    /// library at the next launch.
    #[test]
    fn a_snapshot_without_tracks_is_ignored() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = Path::new("/music");
        let empty = Library::empty_with_covers(root, dirs.covers_dir());
        save(&dirs.library_path(), &empty).expect("save");
        assert!(load(&dirs.library_path(), root, &dirs.covers_dir()).is_none());
    }

    fn tracks_of(lib: &Library) -> Vec<Track> {
        lib.tracks().cloned().collect()
    }

    #[test]
    fn a_scan_that_found_nothing_new_does_not_rewrite_the_snapshot() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let lib = library(Path::new("/music"), &dirs.covers_dir());
        let path = dirs.library_path();

        assert_eq!(
            save_scan(&path, &[], &lib, || false).expect("save"),
            Saved::Written
        );
        let first = std::fs::read(&path).expect("read");
        std::fs::write(&path, b"marker").expect("write");
        assert_eq!(
            save_scan(&path, &tracks_of(&lib), &lib, || false).expect("save"),
            Saved::Unchanged
        );
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"marker",
            "not rewritten"
        );

        // A missing file is rewritten even when nothing changed, and a change is saved.
        std::fs::remove_file(&path).expect("rm");
        let same = save_scan(&path, &tracks_of(&lib), &lib, || false).expect("save");
        assert_eq!(same, Saved::Written);
        assert_eq!(std::fs::read(&path).expect("read"), first);
        let mut changed = tracks_of(&lib);
        changed.pop();
        let again = save_scan(&path, &changed, &lib, || false).expect("save");
        assert_eq!(again, Saved::Written);
    }

    /// The unmounted-volume scenario: an empty scan result leaves the good snapshot alone.
    #[test]
    fn an_empty_scan_never_replaces_a_snapshot() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let root = Path::new("/music");
        let good = library(root, &dirs.covers_dir());
        let path = dirs.library_path();
        save(&path, &good).expect("save");
        let before = std::fs::read(&path).expect("read");

        let empty = Library::empty_with_covers(root, dirs.covers_dir());
        let saved = save_scan(&path, &tracks_of(&good), &empty, || false).expect("save");
        assert_eq!(saved, Saved::Empty);
        assert_eq!(std::fs::read(&path).expect("read"), before);
        assert!(load(&path, root, &dirs.covers_dir()).is_some());
    }

    /// The race: the old scan passes its first check, then waits behind the new scan's
    /// save. The answer must be taken again once its turn comes.
    #[test]
    fn a_save_is_rechecked_when_its_turn_comes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = paths::Dirs::at(data.path());
        let lib = library(Path::new("/music"), &dirs.covers_dir());
        let path = dirs.library_path();
        let wanted_no_more = AtomicBool::new(false);

        let turn = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = std::thread::scope(|s| {
            let old_scan = s.spawn(|| {
                save_scan(&path, &[], &lib, || wanted_no_more.load(Ordering::Acquire))
                    .expect("save")
            });
            std::thread::sleep(Duration::from_millis(100)); // let it reach the lock
            wanted_no_more.store(true, Ordering::Release);
            drop(turn);
            old_scan.join().expect("thread")
        });
        assert_eq!(outcome, Saved::Superseded);
        assert!(!path.exists(), "the superseded save wrote nothing");
    }

    #[test]
    fn only_old_library_temp_files_are_removed() {
        let data = tempfile::tempdir().expect("tempdir");
        let dir = data.path();
        let stale = ["library.json.123.0.tmp", "library.json.9.41.tmp"];
        let keep = [
            "library.json",
            "state.json.123.0.tmp",
            "library.json.tmp.bak",
            "notes.tmp",
        ];
        for name in stale.iter().chain(&keep) {
            std::fs::write(dir.join(name), b"x").expect("write");
        }
        assert_eq!(
            remove_stale_temp_files(dir, Duration::from_secs(3600)),
            0,
            "too fresh"
        );
        assert_eq!(remove_stale_temp_files(dir, Duration::ZERO), 2);
        for name in stale {
            assert!(!dir.join(name).exists(), "{name}");
        }
        for name in keep {
            assert!(dir.join(name).exists(), "{name}");
        }
    }
}
