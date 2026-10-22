//! The library data model: [`TrackId`], [`Track`], [`AlbumKey`], [`Album`], [`Artist`],
//! [`Library`].
//!
//! Everything the UI needs per frame is pre-computed at build time: sorted `Vec`s of keys,
//! `HashMap`/`BTreeMap` lookups, and durations. Nothing here allocates when read, so an
//! egui app that redraws every frame can call these getters freely.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::paths::{self, fnv1a_64};

/// Placeholder used when a track has no artist tag and no `Artist/Album/…` directory.
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";
/// Placeholder used when a track has no album tag and no parent directory.
pub const UNKNOWN_ALBUM: &str = "Unknown Album";
/// Placeholder used when a track has no title tag and an empty file stem.
pub const UNKNOWN_TITLE: &str = "Untitled";

/// Stable identifier for a track: FNV-1a-64 of its library-relative, `/`-separated path.
///
/// Stable across runs and machines, so playlists (which store paths) and any UI state that
/// remembers a track survive a rescan.
#[derive(
    Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize,
)]
pub struct TrackId(pub u64);

impl TrackId {
    /// Hash a library-relative path string (normalized first: no leading `./` or `/`).
    pub fn for_rel_path(rel: &str) -> Self {
        TrackId(fnv1a_64(paths::normalize_rel(rel).as_bytes()))
    }

    /// The raw hash.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Identity of an album: lowercased album-artist + lowercased album title.
///
/// Lowercasing is what makes `HOME` and `Home` the same album; `Ord` therefore sorts
/// case-insensitively by artist, then album — which is exactly the Albums view order.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AlbumKey {
    /// Lowercased album artist.
    pub artist: String,
    /// Lowercased album title.
    pub album: String,
}

impl AlbumKey {
    /// Build a key from display strings (trimmed and lowercased).
    pub fn new(album_artist: &str, album: &str) -> Self {
        AlbumKey {
            artist: album_artist.trim().to_lowercase(),
            album: album.trim().to_lowercase(),
        }
    }

    /// FNV-1a-64 of the key — the name of this album's cached cover PNG.
    pub fn hash64(&self) -> u64 {
        let mut buf = String::with_capacity(self.artist.len() + self.album.len() + 1);
        buf.push_str(&self.artist);
        buf.push('\u{1f}');
        buf.push_str(&self.album);
        fnv1a_64(buf.as_bytes())
    }

    /// File name of this album's cached cover, e.g. `176f932674e9821f.png`.
    pub fn cover_file_name(&self) -> String {
        format!("{:016x}.png", self.hash64())
    }
}

impl std::fmt::Display for AlbumKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.artist, self.album)
    }
}

/// One audio file plus its (tag- or path-derived) metadata.
///
/// Serializable because the library snapshot ([`crate::snapshot`]) stores tracks as they
/// are. The two derived fields, [`Track::id`] and [`Track::album_key`], are left out of
/// that form and recomputed on load with [`Track::refresh_key`]: they are pure functions
/// of other fields, and storing them would only add bytes to parse and a way for the file
/// to disagree with itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    /// FNV-1a-64 of [`Track::rel_path`].
    #[serde(skip)]
    pub id: TrackId,
    /// Library-relative, `/`-separated path (`HOME/Odyssey/01 Intro.m4a`).
    pub rel_path: String,
    /// The real on-disk path, set only when [`Track::rel_path`] is a *lossy* rendering of
    /// it — a file name that is not valid UTF-8, which Linux file systems allow. `None` in
    /// the normal case, where `root` + `rel_path` is exactly the file.
    ///
    /// Stored as raw bytes, because such a path is by definition not a string.
    #[serde(with = "path_bytes")]
    pub source_path: Option<PathBuf>,
    /// Display title.
    pub title: String,
    /// Track artist (may differ from the album artist on compilations).
    pub artist: String,
    /// Album artist — falls back to `artist`, which falls back to the grandparent dir.
    pub album_artist: String,
    /// Display album title.
    pub album: String,
    /// Precomputed `AlbumKey` so the UI can look up the album without allocating.
    #[serde(skip)]
    pub album_key: AlbumKey,
    /// Track number within its disc.
    pub track_no: Option<u32>,
    /// Disc number.
    pub disc_no: Option<u32>,
    /// Release year.
    pub year: Option<u32>,
    /// Genre tag.
    pub genre: Option<String>,
    /// Decoded duration (from the container properties, not an estimate).
    pub duration: Duration,
    /// Whether the file carries at least one embedded picture.
    pub has_artwork: bool,
    /// File mtime — drives the Recently Added view, and is half of what tells an
    /// incremental rescan that the file has not changed since it was last read.
    #[serde(with = "mtime")]
    pub added_at: SystemTime,
    /// File size in bytes — the other half of that check. A tag edit that keeps the mtime
    /// (some taggers can) usually still moves the size; one that keeps both is only picked
    /// up by an explicit `RESCAN`, which reads every file.
    pub size: u64,
}

impl Track {
    /// Build a track from its library-relative path alone, applying every path fallback:
    /// title ← file stem (minus a leading `NN `/`NN-`/`NN.` track prefix), album ← parent
    /// directory, artist and album artist ← grandparent directory, `track_no` ← the `NN`
    /// prefix. The scanner starts here and overwrites whatever the tags actually provide.
    pub fn new(rel_path: &str) -> Track {
        let rel = paths::normalize_rel(rel_path);
        let (dirs, file) = split_rel(&rel);
        let stem = file_stem(file);
        let (track_no, title) = split_track_prefix(stem);
        let title = if title.is_empty() {
            UNKNOWN_TITLE.to_string()
        } else {
            title.to_string()
        };
        let album = dirs
            .last()
            .map_or_else(|| UNKNOWN_ALBUM.to_string(), |d| (*d).to_string());
        let artist = if dirs.len() >= 2 {
            dirs[dirs.len() - 2].to_string()
        } else {
            UNKNOWN_ARTIST.to_string()
        };
        let mut t = Track {
            id: TrackId::for_rel_path(&rel),
            rel_path: rel,
            source_path: None,
            title,
            artist: artist.clone(),
            album_artist: artist,
            album,
            album_key: AlbumKey::default(),
            track_no,
            disc_no: None,
            year: None,
            genre: None,
            duration: Duration::ZERO,
            has_artwork: false,
            added_at: SystemTime::UNIX_EPOCH,
            size: 0,
        };
        t.refresh_key();
        t
    }

    /// Recompute [`Track::album_key`] and [`Track::id`] after changing the path, album or
    /// album artist.
    pub fn refresh_key(&mut self) {
        self.album_key = AlbumKey::new(&self.album_artist, &self.album);
        self.id = TrackId::for_rel_path(&self.rel_path);
    }

    /// Absolute path of the file, for the audio engine.
    ///
    /// This is the *real* path even when the name could not be rendered as UTF-8, so a
    /// track whose displayed name is approximate still plays.
    pub fn abs_path(&self, root: &Path) -> PathBuf {
        match &self.source_path {
            Some(path) => path.clone(),
            None => root.join(&self.rel_path),
        }
    }
}

/// An album: a group of tracks sharing an [`AlbumKey`].
#[derive(Clone, Debug)]
pub struct Album {
    /// Identity (lowercased artist/title pair).
    pub key: AlbumKey,
    /// Display album title.
    pub title: String,
    /// Display album artist.
    pub artist: String,
    /// Release year, taken from the first track that has one.
    pub year: Option<u32>,
    /// Track ids sorted by disc, then track number, then title.
    pub track_ids: Vec<TrackId>,
    /// Newest mtime among the album's tracks — drives Recently Added.
    pub added_at: SystemTime,
    /// Sum of the tracks' durations (for `YEAR · N SONGS · MM MIN`).
    pub duration: Duration,
    /// True if any track carries embedded artwork (i.e. a cover PNG should exist).
    pub has_artwork: bool,
}

impl Album {
    /// Path of this album's cached cover PNG inside `covers_dir`
    /// (`<library>/.phoebus/cache/covers`). The file may not exist — check before use.
    pub fn cover_path(&self, covers_dir: &Path) -> PathBuf {
        covers_dir.join(self.key.cover_file_name())
    }

    /// Number of tracks on the album.
    pub fn track_count(&self) -> usize {
        self.track_ids.len()
    }
}

/// An artist: everything grouped under one album-artist name.
#[derive(Clone, Debug)]
pub struct Artist {
    /// Display name (first spelling encountered).
    pub name: String,
    /// Lowercased name — the lookup key.
    pub sort_key: String,
    /// This artist's albums, sorted by album title.
    pub album_keys: Vec<AlbumKey>,
    /// Total number of tracks across those albums.
    pub track_count: usize,
}

/// The whole library, pre-indexed and pre-sorted for a redraw-every-frame UI.
///
/// A library knows two directories and they are independent: `root`, the read-only music
/// tree, and `covers`, where the cover cache lives (v1.1: the app-data directory, see
/// [`crate::paths::Dirs`]). The root-relative constructors keep the pre-v1.1 layout
/// (`<root>/.phoebus/cache/covers`).
#[derive(Clone, Debug)]
pub struct Library {
    root: PathBuf,
    covers: PathBuf,
    tracks: HashMap<TrackId, Track>,
    albums: BTreeMap<AlbumKey, Album>,
    album_order: Vec<AlbumKey>,
    artists: Vec<Artist>,
    artist_index: HashMap<String, usize>,
    recently_added: Vec<AlbumKey>,
    tracks_sorted: Vec<TrackId>,
    total_duration: Duration,
}

impl Library {
    /// An empty library rooted at `root` — what the app shows before the first scan ends.
    ///
    /// Covers resolve to `<root>/.phoebus/cache/covers`; see
    /// [`Library::empty_with_covers`] for an explicit cache directory.
    pub fn empty(root: impl Into<PathBuf>) -> Library {
        Library::build(root, Vec::new())
    }

    /// An empty library with both directories given explicitly.
    pub fn empty_with_covers(root: impl Into<PathBuf>, covers_dir: impl Into<PathBuf>) -> Library {
        Library::build_with_covers(root, covers_dir, Vec::new())
    }

    /// Index and sort `tracks` into a library, with covers at `<root>/.phoebus/cache/covers`.
    ///
    /// Used by the scanner and by tests. See [`Library::build_with_covers`] to point the
    /// cover cache somewhere outside the library root.
    pub fn build(root: impl Into<PathBuf>, tracks: Vec<Track>) -> Library {
        let root = root.into();
        let covers = paths::Dirs::inside(&root).covers_dir();
        Library::build_with_covers(root, covers, tracks)
    }

    /// Index and sort `tracks` into a library whose cover cache lives at `covers_dir`.
    ///
    /// `covers_dir` is normally [`Dirs::covers_dir`](crate::paths::Dirs::covers_dir), which
    /// is outside the library root — Phoebus never writes inside the music tree.
    pub fn build_with_covers(
        root: impl Into<PathBuf>,
        covers_dir: impl Into<PathBuf>,
        tracks: Vec<Track>,
    ) -> Library {
        let root = root.into();
        let covers = covers_dir.into();

        // Group track indices by album.
        let mut groups: HashMap<AlbumKey, Vec<usize>> = HashMap::new();
        for (i, t) in tracks.iter().enumerate() {
            groups.entry(t.album_key.clone()).or_default().push(i);
        }

        let mut albums: BTreeMap<AlbumKey, Album> = BTreeMap::new();
        for (key, mut members) in groups {
            members.sort_by(|&a, &b| track_order(&tracks[a], &tracks[b]));
            let first = &tracks[members[0]];
            let mut album = Album {
                key: key.clone(),
                title: first.album.clone(),
                artist: first.album_artist.clone(),
                year: None,
                track_ids: Vec::with_capacity(members.len()),
                added_at: SystemTime::UNIX_EPOCH,
                duration: Duration::ZERO,
                has_artwork: false,
            };
            for &i in &members {
                let t = &tracks[i];
                album.track_ids.push(t.id);
                album.duration = album.duration.saturating_add(t.duration);
                album.has_artwork |= t.has_artwork;
                if album.year.is_none() {
                    album.year = t.year;
                }
                if t.added_at > album.added_at {
                    album.added_at = t.added_at;
                }
            }
            albums.insert(key, album);
        }

        // BTreeMap iteration order == (artist, album) ascending, case-insensitive.
        let album_order: Vec<AlbumKey> = albums.keys().cloned().collect();

        // Artists, grouped by lowercased album artist.
        let mut artists: Vec<Artist> = Vec::new();
        let mut artist_index: HashMap<String, usize> = HashMap::new();
        for key in &album_order {
            let album = &albums[key];
            let sort_key = key.artist.clone();
            let idx = *artist_index.entry(sort_key.clone()).or_insert_with(|| {
                artists.push(Artist {
                    name: album.artist.clone(),
                    sort_key,
                    album_keys: Vec::new(),
                    track_count: 0,
                });
                artists.len() - 1
            });
            artists[idx].album_keys.push(key.clone());
            artists[idx].track_count += album.track_ids.len();
        }
        artists.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
        artist_index.clear();
        for (i, a) in artists.iter().enumerate() {
            artist_index.insert(a.sort_key.clone(), i);
        }

        // Recently added: albums by mtime, newest first.
        let mut recently_added = album_order.clone();
        recently_added.sort_by(|a, b| {
            albums[b]
                .added_at
                .cmp(&albums[a].added_at)
                .then_with(|| a.cmp(b))
        });

        // Songs view default order: album artist → album → disc → track → title.
        // The key is the whole `AlbumKey` — the same grouping the Albums and Artists views
        // use — so a guest credit on one track cannot detach it from its album block, and
        // two albums that happen to share a title stay apart.
        let mut order: Vec<usize> = (0..tracks.len()).collect();
        order.sort_by(|&a, &b| {
            tracks[a]
                .album_key
                .cmp(&tracks[b].album_key)
                .then_with(|| track_order(&tracks[a], &tracks[b]))
        });
        let tracks_sorted: Vec<TrackId> = order.iter().map(|&i| tracks[i].id).collect();

        let total_duration = sum_durations(tracks.iter().map(|t| t.duration));
        let mut map: HashMap<TrackId, Track> = HashMap::with_capacity(tracks.len());
        for t in tracks {
            if let Some(previous) = map.insert(t.id, t) {
                // Two different files hashing to one id — only reachable when a name is not
                // valid UTF-8 and two lossy renderings collide. Losing a track silently is
                // exactly what the scan is not allowed to do.
                let kept = &map[&previous.id];
                if kept.rel_path != previous.rel_path {
                    log::warn!(
                        "library: {} and {} share a track id; only the second is listed",
                        previous.rel_path,
                        kept.rel_path
                    );
                }
            }
        }

        Library {
            root,
            covers,
            tracks: map,
            albums,
            album_order,
            artists,
            artist_index,
            recently_added,
            tracks_sorted,
            total_duration,
        }
    }

    /// The library root this library was scanned from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory this library's cached cover PNGs live in.
    ///
    /// `<root>/.phoebus/cache/covers` for a library built with [`Library::build`], or
    /// whatever was passed to [`Library::build_with_covers`].
    pub fn covers_dir(&self) -> PathBuf {
        self.covers.clone()
    }

    /// Cached cover PNG path for an album key (the file may not exist).
    pub fn cover_path(&self, key: &AlbumKey) -> PathBuf {
        self.covers.join(key.cover_file_name())
    }

    /// Look up a track. O(1).
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.get(&id)
    }

    /// Absolute path of a track, for the audio engine.
    pub fn track_path(&self, id: TrackId) -> Option<PathBuf> {
        self.tracks.get(&id).map(|t| t.abs_path(&self.root))
    }

    /// Look up an album. O(log n) and allocation-free — pass a key borrowed from a track.
    pub fn album(&self, key: &AlbumKey) -> Option<&Album> {
        self.albums.get(key)
    }

    /// The album of a track, if any.
    pub fn album_of(&self, id: TrackId) -> Option<&Album> {
        self.tracks
            .get(&id)
            .and_then(|t| self.albums.get(&t.album_key))
    }

    /// All album keys, sorted by artist then title. Index-addressable for grid layout.
    pub fn albums(&self) -> &[AlbumKey] {
        &self.album_order
    }

    /// Album keys ordered by `added_at`, newest first.
    pub fn recently_added(&self) -> &[AlbumKey] {
        &self.recently_added
    }

    /// Track ids of an album, already sorted by disc/track/title.
    pub fn album_tracks(&self, key: &AlbumKey) -> &[TrackId] {
        self.albums.get(key).map_or(&[], |a| &a.track_ids)
    }

    /// All artists, sorted case-insensitively by name.
    pub fn artists(&self) -> &[Artist] {
        &self.artists
    }

    /// Look up an artist by display name (case-insensitive).
    pub fn artist(&self, name: &str) -> Option<&Artist> {
        let key = name.trim().to_lowercase();
        self.artist_index.get(&key).map(|&i| &self.artists[i])
    }

    /// Album keys of an artist (case-insensitive name), sorted by album title.
    pub fn albums_of_artist(&self, name: &str) -> &[AlbumKey] {
        self.artist(name).map_or(&[], |a| &a.album_keys)
    }

    /// Every track id in the Songs-view default order: album artist → album → disc → track
    /// → title. Grouping by *album* artist is what keeps an album's rows contiguous when a
    /// track carries a guest credit.
    pub fn tracks_sorted(&self) -> &[TrackId] {
        &self.tracks_sorted
    }

    /// Number of tracks.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Number of albums.
    pub fn album_count(&self) -> usize {
        self.albums.len()
    }

    /// Number of artists.
    pub fn artist_count(&self) -> usize {
        self.artists.len()
    }

    /// Total playing time of the whole library.
    pub fn total_duration(&self) -> Duration {
        self.total_duration
    }

    /// True when the scan found nothing.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Sum of the durations of a set of tracks (for playlist headers).
    pub fn duration_of(&self, ids: &[TrackId]) -> Duration {
        sum_durations(
            ids.iter()
                .filter_map(|id| self.tracks.get(id))
                .map(|t| t.duration),
        )
    }

    /// Every track, in no particular order — what the snapshot stores, and what a rescan
    /// hands back to the scanner as already known.
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.values()
    }

    /// True when `tracks` — which must not repeat a track — are exactly the tracks of this
    /// library, field for field. How a finished scan tells that it found nothing new
    /// compared with what it started from, without building a second library.
    pub fn holds_exactly(&self, tracks: &[Track]) -> bool {
        self.tracks.len() == tracks.len()
            && tracks.iter().all(|t| self.tracks.get(&t.id) == Some(t))
    }

    /// The albums whose tracks differ between `self` and `newer`.
    ///
    /// A track that was added, removed or changed in any field puts its album in the set —
    /// both albums, when a re-tag moved it from one to another. The set is empty exactly
    /// when the two libraries hold the same tracks, which is how the app tells a background
    /// rescan that found nothing from one it has to show, and which covers it has to reload
    /// when it does. One hash lookup and one comparison per track: far cheaper than the
    /// stat per file the rescan itself costs.
    pub fn changed_albums(&self, newer: &Library) -> BTreeSet<AlbumKey> {
        let mut changed = BTreeSet::new();
        for (id, track) in &newer.tracks {
            match self.tracks.get(id) {
                Some(old) if old == track => {}
                Some(old) => {
                    changed.insert(old.album_key.clone());
                    changed.insert(track.album_key.clone());
                }
                None => {
                    changed.insert(track.album_key.clone());
                }
            }
        }
        for (id, track) in &self.tracks {
            if !newer.tracks.contains_key(id) {
                changed.insert(track.album_key.clone());
            }
        }
        changed
    }
}

/// Sum of `durations`, clamped at [`Duration::MAX`] instead of overflowing.
///
/// A duration is data read from a file or from a snapshot, so no sum of them may panic
/// (`Duration`'s own `+` and `sum` do, past `u64::MAX` seconds): a damaged `library.json`
/// must cost a rescan, never every launch.
pub fn sum_durations(durations: impl IntoIterator<Item = Duration>) -> Duration {
    durations
        .into_iter()
        .fold(Duration::ZERO, Duration::saturating_add)
}

/// Disc → track → title → path ordering, used inside albums and as a tiebreak elsewhere.
fn track_order(a: &Track, b: &Track) -> std::cmp::Ordering {
    a.disc_no
        .unwrap_or(1)
        .cmp(&b.disc_no.unwrap_or(1))
        .then_with(|| {
            a.track_no
                .unwrap_or(u32::MAX)
                .cmp(&b.track_no.unwrap_or(u32::MAX))
        })
        .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        .then_with(|| a.rel_path.cmp(&b.rel_path))
}

/// Split a normalized relative path into (directory components, file name).
fn split_rel(rel: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let file = parts.pop().unwrap_or("");
    (parts, file)
}

/// File name without its extension.
fn file_stem(file: &str) -> &str {
    match file.rfind('.') {
        Some(i) if i > 0 => &file[..i],
        _ => file,
    }
}

/// Split a leading `NN`/`NN-`/`NN.`/`NN_` track-number prefix off a file stem.
///
/// Requires 1–3 digits followed by a separator, so `2001 A Space Odyssey` keeps its name.
pub fn split_track_prefix(stem: &str) -> (Option<u32>, &str) {
    let digits = stem.len() - stem.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > 3 {
        return (None, stem);
    }
    let rest = &stem[digits..];
    let trimmed = rest.trim_start_matches([' ', '-', '_', '.']);
    if trimmed.len() == rest.len() || trimmed.is_empty() {
        return (None, stem);
    }
    (stem[..digits].parse::<u32>().ok(), trimmed.trim_end())
}

/// `Option<PathBuf>` as raw bytes, for [`Track::source_path`].
///
/// That field is only ever set for a path that is *not* valid UTF-8, which serde's own
/// `PathBuf` impl refuses to write — one such file would sink the whole snapshot. On Unix a
/// path is bytes, so bytes are what round-trip it exactly.
mod path_bytes {
    use std::path::PathBuf;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(path: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
        path.as_deref().map(to_bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
        Ok(Option::<Vec<u8>>::deserialize(d)?.map(from_bytes))
    }

    #[cfg(unix)]
    fn to_bytes(path: &std::path::Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(unix)]
    fn from_bytes(bytes: Vec<u8>) -> PathBuf {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }

    // Phoebus ships on macOS and Linux only; elsewhere the lossy form at least compiles.
    #[cfg(not(unix))]
    fn to_bytes(path: &std::path::Path) -> Vec<u8> {
        path.to_string_lossy().into_owned().into_bytes()
    }

    #[cfg(not(unix))]
    fn from_bytes(bytes: Vec<u8>) -> PathBuf {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// `SystemTime` as `[seconds, nanoseconds]` since the Unix epoch, for [`Track::added_at`].
///
/// Exact to the nanosecond, because an incremental rescan compares it for equality with
/// the file's mtime. The seconds are signed: serde's own impl refuses a time before 1970,
/// and one file restored with a zeroed or negative mtime would otherwise sink the whole
/// snapshot.
mod mtime {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const NANOS: u32 = 1_000_000_000;

    pub fn serialize<S: Serializer>(at: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let (secs, nanos) = match at.duration_since(UNIX_EPOCH) {
            Ok(after) => (
                i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
                after.subsec_nanos(),
            ),
            Err(e) => {
                // `before` ahead of the epoch, written as a negative second plus a forward
                // fraction, so `nanos` is always in 0..1e9 whichever side it is on.
                let before = e.duration();
                let secs = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
                match before.subsec_nanos() {
                    0 => (-secs, 0),
                    n => (-secs - 1, NANOS - n),
                }
            }
        };
        (secs, nanos).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let (secs, nanos) = <(i64, u32)>::deserialize(d)?;
        if nanos >= NANOS {
            return Err(D::Error::custom("mtime nanoseconds out of range"));
        }
        let whole = Duration::from_secs(secs.unsigned_abs());
        let base = if secs >= 0 {
            UNIX_EPOCH.checked_add(whole)
        } else {
            UNIX_EPOCH.checked_sub(whole)
        };
        base.and_then(|t| t.checked_add(Duration::from_nanos(u64::from(nanos))))
            .ok_or_else(|| D::Error::custom("mtime out of range"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(rel: &str, secs: u64, added: u64) -> Track {
        let mut tr = Track::new(rel);
        tr.duration = Duration::from_secs(secs);
        tr.added_at = SystemTime::UNIX_EPOCH + Duration::from_secs(added);
        tr
    }

    #[test]
    fn track_id_is_stable_and_path_normalized() {
        assert_eq!(
            TrackId::for_rel_path("HOME/Odyssey/01 Intro.m4a"),
            TrackId(0x9835_19c8_dd02_5be3)
        );
        assert_eq!(
            TrackId::for_rel_path("./HOME/Odyssey/01 Intro.m4a"),
            TrackId::for_rel_path("HOME/Odyssey/01 Intro.m4a")
        );
        assert_eq!(format!("{}", TrackId(0xab)), "00000000000000ab");
    }

    #[test]
    fn album_key_lowercases_and_names_its_cover() {
        let k = AlbumKey::new(" HOME ", "Odyssey");
        assert_eq!(k.artist, "home");
        assert_eq!(k.album, "odyssey");
        assert_eq!(k, AlbumKey::new("home", "ODYSSEY"));
        assert_eq!(k.hash64(), 0x176f_9326_74e9_821f);
        assert_eq!(k.cover_file_name(), "176f932674e9821f.png");
        let album = Album {
            key: k.clone(),
            title: "Odyssey".into(),
            artist: "HOME".into(),
            year: None,
            track_ids: vec![],
            added_at: SystemTime::UNIX_EPOCH,
            duration: Duration::ZERO,
            has_artwork: true,
        };
        assert_eq!(
            album.cover_path(Path::new("/c")),
            PathBuf::from("/c/176f932674e9821f.png")
        );
    }

    #[test]
    fn track_prefix_splitting() {
        assert_eq!(split_track_prefix("01 Intro"), (Some(1), "Intro"));
        assert_eq!(split_track_prefix("12-Intro"), (Some(12), "Intro"));
        assert_eq!(split_track_prefix("3. Intro"), (Some(3), "Intro"));
        assert_eq!(split_track_prefix("07 - Intro"), (Some(7), "Intro"));
        assert_eq!(split_track_prefix("Intro"), (None, "Intro"));
        assert_eq!(
            split_track_prefix("2001 A Space Odyssey"),
            (None, "2001 A Space Odyssey")
        );
        assert_eq!(split_track_prefix("01Intro"), (None, "01Intro"));
    }

    #[test]
    fn path_fallbacks_fill_every_field() {
        let tr = Track::new("HOME/Odyssey/01 Intro.m4a");
        assert_eq!(tr.title, "Intro");
        assert_eq!(tr.artist, "HOME");
        assert_eq!(tr.album_artist, "HOME");
        assert_eq!(tr.album, "Odyssey");
        assert_eq!(tr.track_no, Some(1));
        assert_eq!(tr.album_key, AlbumKey::new("HOME", "Odyssey"));

        let loose = Track::new("stray.mp3");
        assert_eq!(loose.title, "stray");
        assert_eq!(loose.artist, UNKNOWN_ARTIST);
        assert_eq!(loose.album, UNKNOWN_ALBUM);
    }

    #[test]
    fn library_indexes_albums_artists_and_sorts() {
        let lib = Library::build(
            "/lib",
            vec![
                t("Zed/Beta/02 Two.mp3", 10, 500),
                t("Zed/Beta/01 One.mp3", 20, 400),
                t("Ann/Alpha/01 Solo.mp3", 30, 900),
            ],
        );
        assert_eq!(lib.track_count(), 3);
        assert_eq!(lib.album_count(), 2);
        assert_eq!(lib.artist_count(), 2);
        assert_eq!(lib.total_duration(), Duration::from_secs(60));

        // Albums sorted by (artist, title), case-insensitively.
        assert_eq!(lib.albums()[0], AlbumKey::new("Ann", "Alpha"));
        assert_eq!(lib.albums()[1], AlbumKey::new("Zed", "Beta"));

        // Album tracks sorted by track number, added_at is the max, duration the sum.
        let beta = lib.album(&AlbumKey::new("Zed", "Beta")).expect("album");
        assert_eq!(beta.title, "Beta");
        assert_eq!(beta.artist, "Zed");
        assert_eq!(beta.duration, Duration::from_secs(30));
        assert_eq!(
            beta.added_at,
            SystemTime::UNIX_EPOCH + Duration::from_secs(500)
        );
        assert_eq!(
            beta.track_ids,
            vec![
                TrackId::for_rel_path("Zed/Beta/01 One.mp3"),
                TrackId::for_rel_path("Zed/Beta/02 Two.mp3")
            ]
        );

        // Artists sorted case-insensitively, with their albums.
        assert_eq!(lib.artists()[0].name, "Ann");
        assert_eq!(lib.artists()[1].name, "Zed");
        assert_eq!(lib.artists()[1].track_count, 2);
        assert_eq!(lib.albums_of_artist("zed"), &[AlbumKey::new("Zed", "Beta")]);
        assert!(lib.albums_of_artist("nobody").is_empty());

        // Recently added is newest first.
        assert_eq!(lib.recently_added()[0], AlbumKey::new("Ann", "Alpha"));

        // Songs order: artist → album → disc → track.
        let names: Vec<&str> = lib
            .tracks_sorted()
            .iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(names, vec!["Solo", "One", "Two"]);
    }

    /// The Songs list groups by *album artist*, like every other index in the model. A
    /// guest credit on one track changes `artist`, not the album the track belongs to.
    #[test]
    fn songs_order_keeps_an_album_together_when_a_track_has_a_guest_artist() {
        let mut guest = t("HOME/Odyssey/02 Resonance (HAZH Remix).mp3", 10, 100);
        guest.artist = "HAZH".to_string(); // aART is still HOME
        guest.refresh_key();

        let lib = Library::build(
            "/lib",
            vec![
                t("HOME/Odyssey/01 Intro.mp3", 10, 100),
                guest,
                t("HOME/Odyssey/03 Outro.mp3", 10, 100),
                t("Ian/Solo/01 Alone.mp3", 10, 100),
            ],
        );
        let titles: Vec<&str> = lib
            .tracks_sorted()
            .iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec!["Intro", "Resonance (HAZH Remix)", "Outro", "Alone"],
            "the remix must stay inside its album block"
        );
    }

    /// The whole album key is the sort key, so two compilations that share a title (and,
    /// on a compilation, a track-artist string) do not interleave.
    #[test]
    fn songs_order_separates_albums_that_share_a_title() {
        let various = |rel: &str| {
            let mut track = t(rel, 10, 100);
            track.artist = "Various".to_string();
            track
        };
        let lib = Library::build(
            "/lib",
            vec![
                various("Comp A/Greatest Hits/01 One.mp3"),
                various("Comp B/Greatest Hits/01 Two.mp3"),
                various("Comp A/Greatest Hits/02 Three.mp3"),
                various("Comp B/Greatest Hits/02 Four.mp3"),
            ],
        );
        let titles: Vec<&str> = lib
            .tracks_sorted()
            .iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(titles, vec!["One", "Three", "Two", "Four"]);
    }

    /// A name that is not valid UTF-8 survives in `rel_path` only as a lossy rendering, so
    /// the engine must be handed the remembered real path instead of `root + rel_path`.
    #[test]
    fn abs_path_uses_the_real_path_when_the_name_is_lossy() {
        let mut tr = Track::new("Bj\u{fffd}rk/Post/01 Army.wav");
        assert_eq!(
            tr.abs_path(Path::new("/lib")),
            PathBuf::from("/lib/Bj\u{fffd}rk/Post/01 Army.wav"),
            "the normal case is unchanged"
        );
        tr.source_path = Some(PathBuf::from("/lib/real name/01 Army.wav"));
        assert_eq!(
            tr.abs_path(Path::new("/lib")),
            PathBuf::from("/lib/real name/01 Army.wav")
        );
    }

    #[test]
    fn library_lookup_helpers() {
        let lib = Library::build("/lib", vec![t("A/B/01 C.mp3", 5, 1)]);
        let id = TrackId::for_rel_path("A/B/01 C.mp3");
        assert_eq!(lib.track(id).expect("track").title, "C");
        assert_eq!(lib.track_path(id), Some(PathBuf::from("/lib/A/B/01 C.mp3")));
        assert_eq!(lib.album_of(id).expect("album").title, "B");
        assert_eq!(lib.duration_of(&[id, TrackId(9)]), Duration::from_secs(5));
        assert!(lib.track(TrackId(9)).is_none());
        assert!(Library::empty("/lib").is_empty());
        assert_eq!(
            lib.cover_path(&AlbumKey::new("A", "B")),
            PathBuf::from("/lib/.phoebus/cache/covers")
                .join(AlbumKey::new("A", "B").cover_file_name())
        );
    }

    /// The cover cache is a property of the *library*, not of its root: v1.1 keeps it in the
    /// app-data dir so nothing is ever written inside the music tree.
    #[test]
    fn a_library_carries_its_covers_dir() {
        let key = AlbumKey::new("A", "B");
        let lib = Library::build_with_covers(
            "/music",
            "/data/cache/covers",
            vec![t("A/B/01 C.mp3", 5, 1)],
        );
        assert_eq!(lib.root(), Path::new("/music"));
        assert_eq!(lib.covers_dir(), PathBuf::from("/data/cache/covers"));
        assert_eq!(
            lib.cover_path(&key),
            PathBuf::from("/data/cache/covers").join(key.cover_file_name()),
            "cover_path follows the explicit dir, not the root"
        );
        assert_eq!(
            lib.track_path(TrackId::for_rel_path("A/B/01 C.mp3")),
            Some(PathBuf::from("/music/A/B/01 C.mp3")),
            "track paths still come from the root"
        );

        let empty = Library::empty_with_covers("/music", "/data/cache/covers");
        assert!(empty.is_empty());
        assert_eq!(empty.covers_dir(), PathBuf::from("/data/cache/covers"));

        // The root-relative constructors keep the pre-v1.1 layout.
        assert_eq!(
            Library::empty("/music").covers_dir(),
            PathBuf::from("/music/.phoebus/cache/covers")
        );
    }

    /// The snapshot form of a track: every stored field survives exactly, the derived ones
    /// are recomputed, and the two fields serde cannot write on its own — a pre-1970 mtime
    /// and a path that is not a string — still round-trip.
    #[test]
    fn a_track_round_trips_through_json() {
        let mut tr = t("HOME/Odyssey/01 Intro.m4a", 241, 0);
        tr.duration = Duration::new(241, 123_456_789);
        tr.added_at = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 987_654_321);
        tr.size = 8_123_456;
        tr.genre = Some("Synthwave".to_string());
        tr.has_artwork = true;
        tr.source_path = Some(PathBuf::from("/lib/real name/01 Intro.m4a"));

        let json = serde_json::to_string(&tr).expect("serialize");
        assert!(
            !json.contains("album_key"),
            "derived fields stay out: {json}"
        );
        let mut back: Track = serde_json::from_str(&json).expect("deserialize");
        back.refresh_key();
        assert_eq!(back, tr);

        for before in [Duration::new(5, 0), Duration::new(5, 250_000_000)] {
            tr.added_at = SystemTime::UNIX_EPOCH - before;
            let json = serde_json::to_string(&tr).expect("pre-epoch serializes");
            let mut back: Track = serde_json::from_str(&json).expect("deserialize");
            back.refresh_key();
            assert_eq!(back.added_at, tr.added_at, "{before:?} before the epoch");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_lossy_source_path_round_trips_as_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let mut tr = Track::new("Bj\u{fffd}rk/Post/01 Army.wav");
        let real = PathBuf::from(std::ffi::OsStr::from_bytes(
            b"/lib/Bj\xf6rk/Post/01 Army.wav",
        ));
        tr.source_path = Some(real.clone());
        let json = serde_json::to_string(&tr).expect("a non-UTF-8 path must not fail the save");
        let back: Track = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.source_path, Some(real));
    }

    /// Durations come from files and snapshots, so summing them must clamp, not panic.
    #[test]
    fn huge_durations_saturate_instead_of_overflowing() {
        let mut a = t("A/One/01 x.mp3", 0, 1);
        a.duration = Duration::MAX;
        let mut b = t("A/One/02 y.mp3", 0, 1);
        b.duration = Duration::from_secs(60);
        let ids = [a.id, b.id];
        let lib = Library::build("/lib", vec![a, b]);
        assert_eq!(lib.total_duration(), Duration::MAX);
        assert_eq!(lib.duration_of(&ids), Duration::MAX);
        let album = lib.album(&AlbumKey::new("A", "One")).expect("album");
        assert_eq!(album.duration, Duration::MAX);
    }

    #[test]
    fn changed_albums_names_exactly_the_albums_that_moved() {
        let old = Library::build(
            "/lib",
            vec![
                t("A/One/01 x.mp3", 10, 1),
                t("A/One/02 y.mp3", 10, 1),
                t("B/Two/01 z.mp3", 10, 1),
                t("C/Gone/01 w.mp3", 10, 1),
            ],
        );
        assert!(
            old.changed_albums(&old.clone()).is_empty(),
            "same tracks, nothing changed"
        );

        let mut retagged = t("A/One/02 y.mp3", 10, 2); // newer mtime
        retagged.title = "Why".to_string();
        let newer = Library::build(
            "/lib",
            vec![
                t("A/One/01 x.mp3", 10, 1),
                retagged,
                t("B/Two/01 z.mp3", 10, 1),
                t("D/New/01 v.mp3", 10, 1),
            ],
        );
        let changed: Vec<AlbumKey> = old.changed_albums(&newer).into_iter().collect();
        assert_eq!(
            changed,
            vec![
                AlbumKey::new("A", "One"),
                AlbumKey::new("C", "Gone"),
                AlbumKey::new("D", "New"),
            ],
            "a changed, a removed and an added album; the untouched one is not named"
        );
    }
}
