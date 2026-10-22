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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Library, Track};
use crate::paths;

/// Format version of `library.json`. Bump it whenever the stored form of a [`Track`]
/// changes; every older (and newer) snapshot is then ignored and rebuilt by one full scan.
pub const VERSION: u32 = 1;

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
    let out = Out {
        version: VERSION,
        root: library.root(),
        tracks: library.tracks().collect(),
    };
    let json = serde_json::to_vec(&out).context("encoding the library snapshot")?;
    paths::write_atomic(path, &json)
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
    use crate::model::TrackId;
    use std::time::{Duration, SystemTime};

    fn track(rel: &str, secs: u64, size: u64) -> Track {
        let mut t = Track::new(rel);
        t.duration = Duration::from_millis(secs * 1000 + 321);
        t.added_at = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000 + secs, 123_456_789);
        t.size = size;
        t.year = Some(2014);
        t.has_artwork = true;
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
}
