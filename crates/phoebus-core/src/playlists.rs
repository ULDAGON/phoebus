//! Playlists: a [`Playlist`] model and a [`PlaylistStore`] that keeps every playlist in one
//! `playlists.json` inside the app-data directory ([`crate::paths::Dirs`]).
//!
//! Entries are library-relative paths, not ids, so the file survives the library moving and
//! a track that is temporarily missing stays in the playlist (it is only skipped when the
//! playlist is resolved against a [`Library`]).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::model::{Library, TrackId};
use crate::paths;

/// Prefix used for auto-generated playlist names: `Playlist`, `Playlist 2`, …
const DEFAULT_NAME: &str = "Playlist";

/// A user playlist. `entries` are library-relative, `/`-separated paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    /// Stable id, unique within the store.
    pub id: u64,
    /// Display name.
    pub name: String,
    /// Library-relative paths, in playback order.
    #[serde(default)]
    pub entries: Vec<String>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: u64,
    /// Unix seconds.
    #[serde(default)]
    pub modified_at: u64,
}

impl Playlist {
    /// Number of entries, including ones whose file is currently missing.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the playlist has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Track ids for the entries that currently exist in `library`, in order.
    pub fn resolve(&self, library: &Library) -> Vec<TrackId> {
        self.entries
            .iter()
            .map(|e| TrackId::for_rel_path(e))
            .filter(|id| library.track(*id).is_some())
            .collect()
    }

    /// The [`TrackId`] of every entry, in order — [`Playlist::resolve`] without a library.
    ///
    /// Entries whose file is currently missing are **kept**, because this answers "is this
    /// song already on the list?", which is a question about the playlist and not about
    /// what the last scan happened to find. Collect it into a set once when asking about
    /// many tracks at a time (the add-songs picker does exactly that).
    pub fn entry_ids(&self) -> impl Iterator<Item = TrackId> + '_ {
        self.entries.iter().map(|e| TrackId::for_rel_path(e))
    }

    /// True when `track` is already an entry. O(entries) — see [`Playlist::entry_ids`] for
    /// the many-tracks case.
    pub fn contains(&self, track: TrackId) -> bool {
        self.entry_ids().any(|id| id == track)
    }
}

#[derive(Serialize, Deserialize)]
struct PlaylistsFile {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    playlists: Vec<Playlist>,
}

fn one() -> u32 {
    1
}

/// All playlists, backed by `playlists.json` in the app-data directory
/// ([`Dirs::playlists_path`](paths::Dirs::playlists_path)).
///
/// Every mutation writes the whole file atomically (tmp + rename) and returns the I/O
/// result; the in-memory state is updated regardless, so a read-only disk degrades to
/// "changes are not persisted" rather than to a broken UI.
#[derive(Clone, Debug)]
pub struct PlaylistStore {
    path: PathBuf,
    playlists: Vec<Playlist>,
    next_id: u64,
    read_only: bool,
}

impl PlaylistStore {
    /// Load `playlists.json` from an exact path — never an error.
    ///
    /// Three outcomes, and the difference matters:
    /// * the file does not exist → a fresh, writable, empty store (a new install);
    /// * the file parsed → the playlists it held;
    /// * the file exists but could **not** be read (non-UTF-8 bytes, bad permissions, I/O
    ///   error) → an empty store that is [read-only](PlaylistStore::is_read_only), with a
    ///   `log::warn!`. The bytes on disk are left exactly as they are, and every later
    ///   mutation refuses to save rather than replacing playlists it could not read.
    ///
    /// A file that *is* readable but is not valid JSON is a separate, milder case: it is
    /// renamed to `playlists.json.corrupt` (with a warning) and the store stays writable.
    pub fn load_from(path: &Path) -> PlaylistStore {
        let path = path.to_path_buf();
        let mut store = PlaylistStore {
            path,
            playlists: Vec::new(),
            next_id: 1,
            read_only: false,
        };
        let text = match std::fs::read_to_string(&store.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return store,
            Err(e) => {
                log::warn!(
                    "playlists: {} exists but cannot be read ({e}); \
                     keeping it untouched and refusing to save over it",
                    store.path.display()
                );
                store.read_only = true;
                return store;
            }
        };
        match serde_json::from_str::<PlaylistsFile>(&text) {
            Ok(file) => {
                store.playlists = file.playlists;
                store.next_id = file
                    .next_id
                    .max(store.playlists.iter().map(|p| p.id + 1).max().unwrap_or(1));
            }
            Err(e) => {
                // Keep the damaged file: the next mutation rewrites playlists.json, and
                // silently destroying someone's playlists is not an acceptable failure.
                let backup = store.path.with_extension("json.corrupt");
                let saved = std::fs::rename(&store.path, &backup).is_ok();
                log::warn!(
                    "playlists: {} is corrupt, starting empty: {e}{}",
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
    /// avoid overwriting playlists that were never loaded. Mutations still apply in memory.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// All playlists, in creation order (the sidebar order).
    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    /// Look up a playlist.
    pub fn get(&self, id: u64) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    /// Number of playlists.
    pub fn len(&self) -> usize {
        self.playlists.len()
    }

    /// True when there are no playlists.
    pub fn is_empty(&self) -> bool {
        self.playlists.is_empty()
    }

    /// Write the file atomically. Called by every mutation; also safe to call directly.
    ///
    /// Refused with an error while the store [is read-only](PlaylistStore::is_read_only).
    pub fn save(&self) -> Result<()> {
        if self.read_only {
            return Err(anyhow!(
                "refusing to overwrite {}: it could not be read, so its playlists are unknown",
                self.path.display()
            ));
        }
        let file = PlaylistsFile {
            version: 1,
            next_id: self.next_id,
            playlists: self.playlists.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)?;
        paths::write_atomic(&self.path, &json)
    }

    /// Create a playlist. `name` of `None` auto-names it `Playlist`, `Playlist 2`, …
    /// Returns the new id.
    pub fn create(&mut self, name: Option<&str>) -> Result<u64> {
        let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => self.auto_name(),
        };
        let now = now_secs();
        let id = self.next_id;
        self.next_id += 1;
        self.playlists.push(Playlist {
            id,
            name,
            entries: Vec::new(),
            created_at: now,
            modified_at: now,
        });
        self.save()?;
        Ok(id)
    }

    /// Rename a playlist. Empty names are ignored.
    pub fn rename(&mut self, id: u64, name: &str) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Ok(());
        }
        let p = self.get_mut(id)?;
        p.name = name;
        p.modified_at = now_secs();
        self.save()
    }

    /// Delete a playlist.
    pub fn delete(&mut self, id: u64) -> Result<()> {
        let before = self.playlists.len();
        self.playlists.retain(|p| p.id != id);
        if self.playlists.len() == before {
            return Err(anyhow!("no playlist with id {id}"));
        }
        self.save()
    }

    /// Append tracks (resolved to their library-relative paths) to a playlist.
    pub fn append_tracks(&mut self, id: u64, library: &Library, ids: &[TrackId]) -> Result<()> {
        let paths: Vec<String> = ids
            .iter()
            .filter_map(|t| library.track(*t))
            .map(|t| t.rel_path.clone())
            .collect();
        self.append_paths(id, paths)
    }

    /// Append raw library-relative paths to a playlist.
    pub fn append_paths(
        &mut self,
        id: u64,
        rel_paths: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        let p = self.get_mut(id)?;
        for rel in rel_paths {
            p.entries.push(paths::normalize_rel(&rel));
        }
        p.modified_at = now_secs();
        self.save()
    }

    /// Remove the entry at `index`. Out-of-range indices are an error.
    pub fn remove_at(&mut self, id: u64, index: usize) -> Result<()> {
        let p = self.get_mut(id)?;
        if index >= p.entries.len() {
            return Err(anyhow!("entry {index} out of range"));
        }
        p.entries.remove(index);
        p.modified_at = now_secs();
        self.save()
    }

    /// Move the entry at `from` to `to` (reorder). `to` is clamped to the last position.
    pub fn move_entry(&mut self, id: u64, from: usize, to: usize) -> Result<()> {
        let p = self.get_mut(id)?;
        if from >= p.entries.len() {
            return Err(anyhow!("entry {from} out of range"));
        }
        let to = to.min(p.entries.len() - 1);
        if from == to {
            return Ok(());
        }
        let entry = p.entries.remove(from);
        p.entries.insert(to, entry);
        p.modified_at = now_secs();
        self.save()
    }

    /// Track ids of a playlist that currently exist in `library` (missing files are
    /// skipped here but kept in the JSON, so they return if the file comes back).
    pub fn resolve(&self, id: u64, library: &Library) -> Vec<TrackId> {
        self.get(id).map(|p| p.resolve(library)).unwrap_or_default()
    }

    /// The name a new auto-named playlist would get.
    pub fn auto_name(&self) -> String {
        for n in 1..=u32::MAX {
            let candidate = if n == 1 {
                DEFAULT_NAME.to_string()
            } else {
                format!("{DEFAULT_NAME} {n}")
            };
            if !self.playlists.iter().any(|p| p.name == candidate) {
                return candidate;
            }
        }
        DEFAULT_NAME.to_string()
    }

    fn get_mut(&mut self, id: u64) -> Result<&mut Playlist> {
        self.playlists
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow!("no playlist with id {id}"))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    /// `playlists.json` inside a throwaway data directory.
    fn store_path(dir: &Path) -> PathBuf {
        paths::Dirs::at(dir).playlists_path()
    }

    fn library(root: &Path) -> Library {
        Library::build(
            root.to_path_buf(),
            vec![
                Track::new("HOME/Odyssey/01 Intro.m4a"),
                Track::new("HOME/Odyssey/02 Resonance.m4a"),
                Track::new("Woodkid/S16/01 Goliath.m4a"),
            ],
        )
    }

    fn ids(rels: &[&str]) -> Vec<TrackId> {
        rels.iter().map(|r| TrackId::for_rel_path(r)).collect()
    }

    #[test]
    fn auto_names_count_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        assert!(store.is_empty());
        let a = store.create(None).expect("create");
        let b = store.create(None).expect("create");
        let c = store.create(Some("  Chill  ")).expect("create");
        assert_eq!(store.get(a).expect("a").name, "Playlist");
        assert_eq!(store.get(b).expect("b").name, "Playlist 2");
        assert_eq!(store.get(c).expect("c").name, "Chill");
        assert_eq!(store.auto_name(), "Playlist 3");
        assert_eq!(store.len(), 3);
        assert_ne!(a, b);
    }

    #[test]
    fn create_rename_delete_round_trip_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        let id = store.create(None).expect("create");
        store.rename(id, "Night Drive").expect("rename");
        assert!(store.path().exists());

        let mut reloaded = PlaylistStore::load_from(&store_path(dir.path()));
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.get(id).expect("playlist").name, "Night Drive");

        // Ids keep counting up after a reload.
        let second = reloaded.create(None).expect("create");
        assert!(second > id);
        reloaded.delete(id).expect("delete");
        assert!(reloaded.get(id).is_none());
        assert!(reloaded.delete(id).is_err());
        assert_eq!(PlaylistStore::load_from(&store_path(dir.path())).len(), 1);
    }

    #[test]
    fn entries_append_remove_and_reorder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        let id = store.create(Some("Mix")).expect("create");

        let all = ids(&[
            "HOME/Odyssey/01 Intro.m4a",
            "HOME/Odyssey/02 Resonance.m4a",
            "Woodkid/S16/01 Goliath.m4a",
        ]);
        store.append_tracks(id, &lib, &all).expect("append");
        assert_eq!(store.get(id).expect("pl").len(), 3);

        store.move_entry(id, 2, 0).expect("move");
        assert_eq!(
            store.get(id).expect("pl").entries[0],
            "Woodkid/S16/01 Goliath.m4a"
        );
        store.move_entry(id, 0, 99).expect("move clamped");
        assert_eq!(
            store.get(id).expect("pl").entries[2],
            "Woodkid/S16/01 Goliath.m4a"
        );
        assert!(store.move_entry(id, 99, 0).is_err());

        store.remove_at(id, 0).expect("remove");
        assert_eq!(store.get(id).expect("pl").len(), 2);
        assert!(store.remove_at(id, 9).is_err());
        assert!(store.append_paths(999, ["x".to_string()]).is_err());

        // The file on disk matches memory.
        let reloaded = PlaylistStore::load_from(&store_path(dir.path()));
        assert_eq!(
            reloaded.get(id).expect("pl").entries,
            store.get(id).expect("pl").entries
        );
    }

    /// The add-songs picker's whole contract at this level (UI-SPEC v1.4 §Add songs): the
    /// popup asks `contains` per row to decide `+` or `✓`, and answers a `+` with the same
    /// `append_tracks` every other "add to playlist" path uses.
    ///
    /// The two halves that matter are that membership flips the moment the append lands —
    /// so the row can go quiet in the same frame — and that it survives the reload, which
    /// is the only reason the popup can be reopened and still be right.
    #[test]
    fn membership_answers_the_add_songs_picker_and_survives_a_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        let id = store.create(Some("Mix")).expect("create");

        let intro = TrackId::for_rel_path("HOME/Odyssey/01 Intro.m4a");
        let goliath = TrackId::for_rel_path("Woodkid/S16/01 Goliath.m4a");
        assert!(
            !store.get(id).expect("pl").contains(intro),
            "a fresh playlist contains nothing, so every row offers `+`"
        );

        store.append_tracks(id, &lib, &[intro]).expect("append");
        let playlist = store.get(id).expect("pl");
        assert!(playlist.contains(intro), "the added row must flip to `✓`");
        assert!(
            !playlist.contains(goliath),
            "…and only that row: the rest keep offering `+`"
        );
        assert_eq!(
            playlist.entry_ids().collect::<Vec<_>>(),
            vec![intro],
            "the popup's membership set is the entry list, in order"
        );

        // Reopening the popup later must show the checkmark from the start.
        let reloaded = PlaylistStore::load_from(&store_path(dir.path()));
        assert!(reloaded.get(id).expect("pl").contains(intro));

        // An entry whose file has left the library is still membership: the popup must not
        // offer to add a song the playlist already lists (it would simply be added twice).
        let mut store = reloaded;
        store
            .append_paths(id, ["Ghost/Gone/99 Missing.m4a".to_string()])
            .expect("append");
        let playlist = store.get(id).expect("pl");
        let ghost = TrackId::for_rel_path("Ghost/Gone/99 Missing.m4a");
        assert!(playlist.contains(ghost));
        assert_eq!(
            playlist.resolve(&lib).len(),
            1,
            "…even though it resolves to nothing"
        );
    }

    #[test]
    fn resolution_skips_missing_files_but_keeps_them_in_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        let id = store.create(Some("Mix")).expect("create");
        store
            .append_paths(
                id,
                [
                    "HOME/Odyssey/01 Intro.m4a".to_string(),
                    "Ghost/Gone/99 Missing.m4a".to_string(),
                    "Woodkid/S16/01 Goliath.m4a".to_string(),
                ],
            )
            .expect("append");

        let resolved = store.resolve(id, &lib);
        assert_eq!(
            resolved,
            vec![
                TrackId::for_rel_path("HOME/Odyssey/01 Intro.m4a"),
                TrackId::for_rel_path("Woodkid/S16/01 Goliath.m4a"),
            ]
        );

        let reloaded = PlaylistStore::load_from(&store_path(dir.path()));
        assert_eq!(
            reloaded.get(id).expect("pl").entries.len(),
            3,
            "the missing entry must survive on disk"
        );
        assert_eq!(reloaded.resolve(id, &lib).len(), 2);
        assert!(reloaded.resolve(4242, &lib).is_empty());
    }

    #[test]
    fn a_corrupt_file_degrades_to_an_empty_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = store_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{ not json at all").expect("write");

        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        assert!(store.is_empty());
        assert!(
            path.with_extension("json.corrupt").exists(),
            "the damaged file must be kept, not overwritten"
        );
        let id = store.create(None).expect("create over corrupt file");
        assert!(
            PlaylistStore::load_from(&store_path(dir.path()))
                .get(id)
                .is_some()
        );
    }

    #[test]
    fn an_unreadable_file_is_preserved_and_saves_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = store_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        // Perfectly good playlists, damaged by one non-UTF-8 byte: `read_to_string` fails
        // with InvalidData, so the serde arm below it never gets a chance to run.
        let mut bytes =
            br#"{"version":1,"next_id":2,"playlists":[{"id":1,"name":"Night Drive"}]}"#.to_vec();
        bytes.push(0xff);
        std::fs::write(&path, &bytes).expect("write");

        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        assert!(store.is_empty(), "nothing could be read");
        assert!(
            store.is_read_only(),
            "a store that could not be read must not write over the file"
        );
        assert!(
            store.create(Some("Fresh")).is_err(),
            "the mutation must be reported as not persisted"
        );
        assert_eq!(
            std::fs::read(&path).expect("read"),
            bytes,
            "the user's playlists must still be on disk, byte for byte"
        );
        assert!(
            !path.with_extension("json.corrupt").exists(),
            "nothing to rescue: the original file was never moved"
        );
        // The app stays usable — the change just is not persisted.
        assert_eq!(store.len(), 1);
        assert!(store.save().is_err(), "an explicit save is refused too");

        // Once the file is readable again a fresh load is a normal, writable store.
        std::fs::write(&path, b"{\"version\":1,\"next_id\":1,\"playlists\":[]}").expect("write");
        let mut ok = PlaylistStore::load_from(&store_path(dir.path()));
        assert!(!ok.is_read_only());
        assert!(ok.create(None).is_ok());
    }

    #[test]
    fn append_tracks_ignores_ids_that_are_not_in_the_library() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = library(dir.path());
        let mut store = PlaylistStore::load_from(&store_path(dir.path()));
        let id = store.create(None).expect("create");
        store
            .append_tracks(
                id,
                &lib,
                &[
                    TrackId(1),
                    TrackId::for_rel_path("Woodkid/S16/01 Goliath.m4a"),
                ],
            )
            .expect("append");
        assert_eq!(store.get(id).expect("pl").entries.len(), 1);
        assert!(!store.get(id).expect("pl").is_empty());
    }
}
