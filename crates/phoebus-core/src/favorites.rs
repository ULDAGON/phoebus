//! Favorites: the hearted albums and tracks, kept in one `favorites.json` inside the
//! app-data directory ([`crate::paths::Dirs`]).
//!
//! Same discipline as [`crate::playlists::PlaylistStore`], for the same reason: what the
//! user hearted is theirs, and a file Phoebus could not read is never overwritten.
//!
//! Two kinds of entry, stored the way each survives a library that moves or a rescan:
//!
//! * albums as their [`AlbumKey`] pair (lowercased artist + album), which is already the
//!   library's identity for an album;
//! * tracks as library-relative paths, exactly like playlist entries — a favourite whose
//!   file is temporarily gone stays in the JSON and comes back with the file.
//!
//! Membership is what the UI asks per row and per cover, every frame, so both
//! [`Favorites::is_album`] and [`Favorites::is_track`] are O(1) hash lookups.
//! `is_track` answers about a [`TrackId`], which is a hash of the path — see
//! [`Favorites::resolve`] for how the id set is kept in step with the live library.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::model::{AlbumKey, Library, TrackId};
use crate::paths;

/// On-disk shape of `favorites.json`. Both lists are written sorted, so an unchanged set of
/// favorites always produces byte-identical output.
#[derive(Serialize, Deserialize)]
struct FavoritesFile {
    #[serde(default = "one")]
    version: u32,
    /// Hearted albums as `{"artist": …, "album": …}` pairs.
    #[serde(default)]
    albums: Vec<AlbumKey>,
    /// Hearted tracks as library-relative, `/`-separated paths.
    #[serde(default)]
    tracks: Vec<String>,
}

fn one() -> u32 {
    1
}

/// Every hearted album and track, backed by `favorites.json` in the app-data directory
/// ([`Dirs::favorites_path`](paths::Dirs::favorites_path)).
///
/// Every successful toggle writes the whole file atomically (tmp + rename). A write that
/// fails is logged and the in-memory state still changes, so a read-only disk degrades to
/// "this heart is not remembered" rather than to a UI that will not toggle.
#[derive(Clone, Debug)]
pub struct Favorites {
    path: PathBuf,
    /// Hearted album keys — O(1) membership for the album grids.
    albums: HashSet<AlbumKey>,
    /// Hearted tracks as library-relative paths: the durable form, including the ones that
    /// no live library can resolve.
    tracks: HashSet<String>,
    /// `tracks` hashed into ids — the per-row lookup. Rebuilt by [`Favorites::resolve`].
    ids: HashSet<TrackId>,
    read_only: bool,
    ephemeral: bool,
}

impl Favorites {
    /// Load `favorites.json` from an exact path — never an error.
    ///
    /// Three outcomes, and the difference matters:
    /// * the file does not exist → a fresh, writable, empty store (a new install);
    /// * the file parsed → the favorites it held;
    /// * the file exists but could **not** be read (non-UTF-8 bytes, bad permissions, I/O
    ///   error) → an empty store that is [read-only](Favorites::is_read_only), with a
    ///   `log::warn!`. The bytes on disk are left exactly as they are, and every later
    ///   toggle refuses to save rather than replacing favorites it could not read.
    ///
    /// A file that *is* readable but is not valid JSON is a separate, milder case: it is
    /// renamed to `favorites.json.corrupt` (with a warning) and the store stays writable.
    pub fn load_from(path: &Path) -> Favorites {
        let mut store = Favorites {
            path: path.to_path_buf(),
            albums: HashSet::new(),
            tracks: HashSet::new(),
            ids: HashSet::new(),
            read_only: false,
            ephemeral: false,
        };
        let text = match std::fs::read_to_string(&store.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return store,
            Err(e) => {
                log::warn!(
                    "favorites: {} exists but cannot be read ({e}); \
                     keeping it untouched and refusing to save over it",
                    store.path.display()
                );
                store.read_only = true;
                return store;
            }
        };
        match serde_json::from_str::<FavoritesFile>(&text) {
            Ok(file) => {
                store.albums = file.albums.into_iter().collect();
                store.tracks = file
                    .tracks
                    .iter()
                    .map(|rel| paths::normalize_rel(rel))
                    .collect();
                // Usable before the first `resolve`: a TrackId *is* the hash of the path, so
                // the stored paths alone already answer correctly for every track a view can
                // show. `resolve` then narrows the set to what the live library holds.
                store.ids = store
                    .tracks
                    .iter()
                    .map(|r| TrackId::for_rel_path(r))
                    .collect();
            }
            Err(e) => {
                // Keep the damaged file: the next toggle rewrites favorites.json, and
                // silently destroying someone's favorites is not an acceptable failure.
                let backup = store.path.with_extension("json.corrupt");
                let saved = std::fs::rename(&store.path, &backup).is_ok();
                log::warn!(
                    "favorites: {} is corrupt, starting empty: {e}{}",
                    store.path.display(),
                    if saved {
                        format!(" (kept a copy at {})", backup.display())
                    } else {
                        String::new()
                    }
                );
            }
        }
        store
    }

    /// Path of the backing JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True when the backing file exists but could not be read, so saving is refused to
    /// avoid overwriting favorites that were never loaded. Toggles still apply in memory.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Detach the store from disk: toggles keep working, [`Favorites::save`] becomes a
    /// successful no-op, and nothing is ever written.
    ///
    /// This is what the screenshot tour uses to seed demo favorites for its `favorites.png`
    /// step without touching the user's file.
    pub fn set_ephemeral(&mut self, ephemeral: bool) {
        self.ephemeral = ephemeral;
    }

    /// True while the store is [detached from disk](Favorites::set_ephemeral).
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    /// Is this album hearted? O(1) — safe to call once per cover, per frame.
    pub fn is_album(&self, key: &AlbumKey) -> bool {
        self.albums.contains(key)
    }

    /// Is this track hearted? O(1) — safe to call once per row, per frame.
    ///
    /// Answers from the id set described in [`Favorites::resolve`].
    pub fn is_track(&self, id: TrackId) -> bool {
        self.ids.contains(&id)
    }

    /// Number of hearted albums.
    pub fn album_count(&self) -> usize {
        self.albums.len()
    }

    /// Number of hearted tracks **as stored**, including ones no live library resolves.
    ///
    /// The count a view shows is `track_ids(library).len()` — that one hides the entries
    /// whose file is currently missing.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// True when nothing at all is hearted.
    pub fn is_empty(&self) -> bool {
        self.albums.is_empty() && self.tracks.is_empty()
    }

    /// Rebuild the track-id set from the stored paths, keeping only tracks that exist in
    /// `library`. Call it after every scan and rescan.
    ///
    /// Mirrors [`Playlist::resolve`](crate::playlists::Playlist::resolve): the paths in the
    /// JSON are the durable truth and are never touched here, so a favourite whose file is
    /// missing today is hearted again the moment the file comes back.
    pub fn resolve(&mut self, library: &Library) {
        self.ids = self
            .tracks
            .iter()
            .map(|rel| TrackId::for_rel_path(rel))
            .filter(|id| library.track(*id).is_some())
            .collect();
    }

    /// The hearted tracks that `library` currently holds, in the Songs-view order
    /// ([`Library::tracks_sorted`]) — the Favorites view's rows, and the context its `PLAY`
    /// and `SHUFFLE` buttons hand to the queue.
    ///
    /// Independent of when [`Favorites::resolve`] last ran: entries the library does not
    /// have simply are not in `tracks_sorted`.
    pub fn track_ids(&self, library: &Library) -> Vec<TrackId> {
        library
            .tracks_sorted()
            .iter()
            .copied()
            .filter(|id| self.ids.contains(id))
            .collect()
    }

    /// Heart or unheart an album; returns the **new** state (`true` == hearted now).
    ///
    /// Saves immediately; a failed save is logged, not returned — the heart still flips.
    pub fn toggle_album(&mut self, key: &AlbumKey) -> bool {
        let now_hearted = if self.albums.remove(key) {
            false
        } else {
            self.albums.insert(key.clone());
            true
        };
        self.persist();
        now_hearted
    }

    /// Heart or unheart a track; returns the **new** state (`true` == hearted now).
    ///
    /// `library` is what turns the id back into the path that gets stored, so an id the
    /// library does not know is a no-op that reports the unchanged state.
    ///
    /// Saves immediately; a failed save is logged, not returned — the heart still flips.
    pub fn toggle_track(&mut self, library: &Library, id: TrackId) -> bool {
        let Some(track) = library.track(id) else {
            return self.is_track(id);
        };
        let rel = paths::normalize_rel(&track.rel_path);
        let now_hearted = if self.tracks.remove(&rel) {
            self.ids.remove(&id);
            false
        } else {
            self.tracks.insert(rel);
            self.ids.insert(id);
            true
        };
        self.persist();
        now_hearted
    }

    /// Write the file atomically. Called by every toggle; also safe to call directly.
    ///
    /// A no-op success while the store is [ephemeral](Favorites::set_ephemeral); refused
    /// with an error while it [is read-only](Favorites::is_read_only).
    pub fn save(&self) -> Result<()> {
        if self.ephemeral {
            return Ok(());
        }
        if self.read_only {
            return Err(anyhow!(
                "refusing to overwrite {}: it could not be read, so its favorites are unknown",
                self.path.display()
            ));
        }
        let mut albums: Vec<AlbumKey> = self.albums.iter().cloned().collect();
        albums.sort();
        let mut tracks: Vec<String> = self.tracks.iter().cloned().collect();
        tracks.sort();
        let json = serde_json::to_vec_pretty(&FavoritesFile {
            version: 1,
            albums,
            tracks,
        })?;
        paths::write_atomic(&self.path, &json)
    }

    fn persist(&self) {
        if let Err(e) = self.save() {
            log::warn!("favorites: change not saved: {e}");
        }
    }
}

// There is no `pinned_albums` here any more. The Albums view used to reorder the whole grid
// so hearted albums came first; it now leaves the grid in library order and puts a FAVORITES
// section above it, so the hearted album is in both places at once. Filtering `Library::albums`
// with [`Favorites::is_album`] is all that takes, and `views::albums::State` does it where it
// can cache the answer — a `Vec<AlbumKey>` rebuilt per frame was the reason this lived here.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    /// `favorites.json` inside a throwaway data directory.
    fn store_path(dir: &Path) -> PathBuf {
        paths::Dirs::at(dir).favorites_path()
    }

    const INTRO: &str = "HOME/Odyssey/01 Intro.m4a";
    const RESONANCE: &str = "HOME/Odyssey/02 Resonance.m4a";
    const GOLIATH: &str = "Woodkid/S16/01 Goliath.m4a";

    fn library(root: &Path) -> Library {
        Library::build(
            root.to_path_buf(),
            vec![
                Track::new(INTRO),
                Track::new(RESONANCE),
                Track::new(GOLIATH),
            ],
        )
    }

    /// Four albums, whose sorted order is aleph, beth, gimel, dalet's artist last.
    fn grid_library(root: &Path) -> Library {
        Library::build(
            root.to_path_buf(),
            vec![
                Track::new("Alpha/Aleph/01 a.m4a"),
                Track::new("Alpha/Beth/01 b.m4a"),
                Track::new("Beta/Gimel/01 c.m4a"),
                Track::new("Gamma/Dalet/01 d.m4a"),
            ],
        )
    }

    fn key(artist: &str, album: &str) -> AlbumKey {
        AlbumKey::new(artist, album)
    }

    fn id(rel: &str) -> TrackId {
        TrackId::for_rel_path(rel)
    }

    #[test]
    fn albums_and_tracks_round_trip_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut fav = Favorites::load_from(&store_path(dir.path()));
        assert!(fav.is_empty());
        assert!(!fav.is_read_only());

        assert!(fav.toggle_album(&key("HOME", "Odyssey")));
        assert!(fav.toggle_track(&lib, id(INTRO)));
        assert!(fav.toggle_track(&lib, id(GOLIATH)));
        assert!(fav.path().exists());
        assert_eq!((fav.album_count(), fav.track_count()), (1, 2));

        let mut reloaded = Favorites::load_from(&store_path(dir.path()));
        reloaded.resolve(&lib);
        assert!(reloaded.is_album(&key("home", "odyssey")));
        assert!(!reloaded.is_album(&key("Woodkid", "S16")));
        assert!(reloaded.is_track(id(INTRO)));
        assert!(reloaded.is_track(id(GOLIATH)));
        assert!(!reloaded.is_track(id(RESONANCE)));
        assert!(!reloaded.is_empty());

        // Ordered like tracks_sorted (album artist → album → disc → track), not like the
        // order they were hearted in.
        assert_eq!(reloaded.track_ids(&lib), vec![id(INTRO), id(GOLIATH)]);

        // The JSON keeps the album as its two lowercased fields.
        let text = std::fs::read_to_string(store_path(dir.path())).expect("read");
        assert!(
            text.contains("\"artist\": \"home\"") && text.contains("\"album\": \"odyssey\""),
            "album keys are (artist, album) pairs: {text}"
        );
        assert!(
            text.contains(INTRO),
            "tracks are library-relative paths: {text}"
        );
    }

    #[test]
    fn toggling_off_removes_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut fav = Favorites::load_from(&store_path(dir.path()));
        let odyssey = key("HOME", "Odyssey");

        assert!(fav.toggle_album(&odyssey), "first toggle hearts it");
        assert!(fav.is_album(&odyssey));
        assert!(!fav.toggle_album(&odyssey), "second toggle unhearts it");
        assert!(!fav.is_album(&odyssey));
        assert_eq!(fav.album_count(), 0);

        assert!(fav.toggle_track(&lib, id(RESONANCE)));
        assert!(fav.is_track(id(RESONANCE)));
        assert!(!fav.toggle_track(&lib, id(RESONANCE)));
        assert!(!fav.is_track(id(RESONANCE)));
        assert_eq!(fav.track_count(), 0);
        assert!(fav.is_empty());
        assert!(fav.track_ids(&lib).is_empty());

        // An id the library never had is a no-op, not a phantom entry.
        assert!(!fav.toggle_track(&lib, TrackId(1)));
        assert_eq!(fav.track_count(), 0);

        let reloaded = Favorites::load_from(&store_path(dir.path()));
        assert!(reloaded.is_empty(), "the empty state is on disk too");
    }

    #[test]
    fn unresolvable_tracks_stay_in_the_json_and_out_of_the_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut fav = Favorites::load_from(&store_path(dir.path()));
        fav.toggle_track(&lib, id(INTRO));
        fav.toggle_track(&lib, id(GOLIATH));

        // The file for one of them disappears from the library (rescan without it).
        let thinner = Library::build(
            dir.path().to_path_buf(),
            vec![Track::new(INTRO), Track::new(RESONANCE)],
        );
        fav.resolve(&thinner);
        assert!(fav.is_track(id(INTRO)));
        assert!(!fav.is_track(id(GOLIATH)), "hidden while the file is gone");
        assert_eq!(fav.track_ids(&thinner), vec![id(INTRO)]);
        assert_eq!(fav.track_count(), 2, "still remembered");

        // Toggling something else must not drop the entry that could not be resolved.
        assert!(fav.toggle_track(&thinner, id(RESONANCE)));
        assert_eq!(fav.track_ids(&thinner), vec![id(INTRO), id(RESONANCE)]);
        let text = std::fs::read_to_string(store_path(dir.path())).expect("read");
        assert!(
            text.contains(GOLIATH),
            "the missing favourite survives: {text}"
        );

        let mut reloaded = Favorites::load_from(&store_path(dir.path()));
        assert_eq!(reloaded.track_count(), 3);
        reloaded.resolve(&lib);
        assert!(
            reloaded.is_track(id(GOLIATH)),
            "it is hearted again the moment the file is back"
        );
    }

    #[test]
    fn resolution_follows_a_library_swap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = Library::build(dir.path().to_path_buf(), vec![Track::new(INTRO)]);
        let second = Library::build(dir.path().to_path_buf(), vec![Track::new(GOLIATH)]);

        let mut fav = Favorites::load_from(&store_path(dir.path()));
        fav.toggle_track(&first, id(INTRO));
        // Not in `first`, so it has to be hearted through a library that has it.
        fav.toggle_track(&second, id(GOLIATH));
        assert_eq!(fav.track_count(), 2);

        fav.resolve(&first);
        assert!(fav.is_track(id(INTRO)));
        assert!(!fav.is_track(id(GOLIATH)));
        assert_eq!(fav.track_ids(&first), vec![id(INTRO)]);

        fav.resolve(&second);
        assert!(!fav.is_track(id(INTRO)));
        assert!(fav.is_track(id(GOLIATH)));
        assert_eq!(fav.track_ids(&second), vec![id(GOLIATH)]);

        // Album favorites are library-independent — the key is the identity.
        assert!(fav.toggle_album(&key("HOME", "Odyssey")));
        fav.resolve(&second);
        assert!(fav.is_album(&key("HOME", "Odyssey")));
    }

    /// What the Albums view's `FAVORITES` section is made of, at the source: hearting picks
    /// albums *out of* `Library::albums` and leaves that list exactly as it was, so the two
    /// can be drawn one above the other with the hearted album in both.
    ///
    /// The section's own business — its order on screen, its cache, and the fact that the
    /// grid below repeats it — belongs to `views::albums` and is tested there. This is only
    /// the predicate underneath it, and the guarantee that the predicate is non-destructive.
    #[test]
    fn hearting_selects_albums_without_disturbing_the_library_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = grid_library(dir.path());
        let mut fav = Favorites::load_from(&store_path(dir.path()));

        let sorted = vec![
            key("Alpha", "Aleph"),
            key("Alpha", "Beth"),
            key("Beta", "Gimel"),
            key("Gamma", "Dalet"),
        ];
        assert_eq!(lib.albums(), sorted.as_slice(), "the plain Albums order");

        let section = |fav: &Favorites| -> Vec<AlbumKey> {
            lib.albums()
                .iter()
                .filter(|k| fav.is_album(k))
                .cloned()
                .collect()
        };
        assert!(section(&fav).is_empty(), "no favorites, no section");

        // The last one hearted first, then a middle one: the section comes out in the
        // *library's* relative order, never in the order the hearts were clicked in.
        fav.toggle_album(&key("Gamma", "Dalet"));
        fav.toggle_album(&key("Alpha", "Beth"));
        assert_eq!(
            section(&fav),
            vec![key("Alpha", "Beth"), key("Gamma", "Dalet")]
        );
        assert_eq!(
            lib.albums(),
            sorted.as_slice(),
            "the grid the section was taken from must be untouched"
        );

        // A hearted album this library does not hold is still stored, and still has no card.
        fav.toggle_album(&key("Ghost", "Nowhere"));
        assert_eq!(fav.album_count(), 3);
        assert_eq!(section(&fav).len(), 2);

        fav.toggle_album(&key("Alpha", "Beth"));
        fav.toggle_album(&key("Gamma", "Dalet"));
        assert!(section(&fav).is_empty(), "unhearting empties the section");
    }

    #[test]
    fn a_corrupt_file_degrades_to_an_empty_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = store_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{ not json at all").expect("write");

        let mut fav = Favorites::load_from(&path);
        assert!(fav.is_empty());
        assert!(!fav.is_read_only());
        assert!(
            path.with_extension("json.corrupt").exists(),
            "the damaged file must be kept, not overwritten"
        );
        assert!(fav.toggle_album(&key("HOME", "Odyssey")));
        assert!(Favorites::load_from(&path).is_album(&key("HOME", "Odyssey")));
    }

    #[test]
    fn an_unreadable_file_is_preserved_and_saves_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let path = store_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        // Perfectly good favorites, damaged by one non-UTF-8 byte: `read_to_string` fails
        // with InvalidData, so the serde arm below it never gets a chance to run.
        let mut bytes =
            br#"{"version":1,"albums":[{"artist":"home","album":"odyssey"}],"tracks":[]}"#.to_vec();
        bytes.push(0xff);
        std::fs::write(&path, &bytes).expect("write");

        let mut fav = Favorites::load_from(&path);
        assert!(fav.is_empty(), "nothing could be read");
        assert!(
            fav.is_read_only(),
            "a store that could not be read must not write over the file"
        );
        assert!(fav.save().is_err(), "an explicit save is refused");

        // The app stays usable — the heart just is not remembered.
        assert!(fav.toggle_album(&key("Woodkid", "S16")));
        assert!(fav.is_album(&key("Woodkid", "S16")));
        assert!(fav.toggle_track(&lib, id(INTRO)));
        assert_eq!(
            std::fs::read(&path).expect("read"),
            bytes,
            "the user's favorites must still be on disk, byte for byte"
        );
        assert!(
            !path.with_extension("json.corrupt").exists(),
            "nothing to rescue: the original file was never moved"
        );

        // Once the file is readable again a fresh load is a normal, writable store.
        std::fs::write(&path, br#"{"version":1,"albums":[],"tracks":[]}"#).expect("write");
        let mut ok = Favorites::load_from(&path);
        assert!(!ok.is_read_only());
        assert!(ok.toggle_album(&key("HOME", "Odyssey")));
        assert!(Favorites::load_from(&path).is_album(&key("HOME", "Odyssey")));
    }

    #[test]
    fn an_ephemeral_store_never_touches_the_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let path = store_path(dir.path());
        let mut fav = Favorites::load_from(&path);
        fav.set_ephemeral(true);
        assert!(fav.is_ephemeral());

        assert!(fav.toggle_album(&key("HOME", "Odyssey")));
        assert!(fav.toggle_track(&lib, id(GOLIATH)));
        assert!(fav.is_album(&key("HOME", "Odyssey")));
        assert_eq!(fav.track_ids(&lib), vec![id(GOLIATH)]);
        assert!(fav.save().is_ok(), "saving is a successful no-op");
        assert!(!path.exists(), "the demo favorites must never reach disk");
    }
}
