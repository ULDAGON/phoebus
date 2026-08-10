//! Routing vocabulary shared by every view and widget: which screen is on show
//! ([`View`]), what the user just asked for ([`Action`]), and the read-only bundle a view
//! needs in order to draw itself ([`Ctx`]).
//!
//! Views never touch the controller directly. They push [`Action`]s into `Ctx::actions`
//! and the app applies them after the frame is laid out — which keeps every view a pure
//! function of the library plus a little UI state, and side-steps egui's borrow puzzles.

use std::collections::HashMap;
use std::time::Duration;

use phoebus_core::{AlbumKey, Favorites, Library, Playlist, ThemeMode, TrackId};

use crate::artwork::Artwork;

/// One screen of the content router.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum View {
    /// Albums by `added_at`, newest first.
    RecentlyAdded,
    /// Every hearted song, in `tracks_sorted` order (UI-SPEC v1.3 §Favorites).
    Favorites,
    /// Every album.
    Albums,
    /// One album's detail page.
    Album(AlbumKey),
    /// Artist list + selected artist (C2).
    Artists,
    /// The flat song table (C2).
    Songs,
    /// One playlist (C2).
    Playlist(u64),
    /// Live search results (C2).
    Search,
    /// Library root + theme settings.
    Settings,
}

impl View {
    /// Serialize into [`phoebus_core::AppState::last_view`].
    pub fn to_state(&self) -> String {
        match self {
            View::RecentlyAdded => "recently_added".to_string(),
            View::Favorites => "favorites".to_string(),
            View::Albums => "albums".to_string(),
            View::Album(key) => format!("album:{}\u{1f}{}", key.artist, key.album),
            View::Artists => "artists".to_string(),
            View::Songs => "songs".to_string(),
            View::Playlist(id) => format!("playlist:{id}"),
            View::Search => "search".to_string(),
            View::Settings => "settings".to_string(),
        }
    }

    /// Parse [`View::to_state`]. Unknown strings fall back to Recently Added, and the
    /// Search view is never restored (its query is gone).
    pub fn from_state(s: &str) -> View {
        if let Some(rest) = s.strip_prefix("album:") {
            let (artist, album) = rest.split_once('\u{1f}').unwrap_or((rest, ""));
            return View::Album(AlbumKey {
                artist: artist.to_string(),
                album: album.to_string(),
            });
        }
        if let Some(id) = s.strip_prefix("playlist:") {
            return match id.parse::<u64>() {
                Ok(id) => View::Playlist(id),
                Err(_) => View::RecentlyAdded,
            };
        }
        match s {
            "favorites" => View::Favorites,
            "albums" => View::Albums,
            "artists" => View::Artists,
            "songs" => View::Songs,
            "settings" => View::Settings,
            _ => View::RecentlyAdded,
        }
    }

    /// True if the view can still be shown against `library` (an album that vanished in a
    /// rescan must not strand the user on an empty page).
    pub fn is_valid(&self, library: &Library) -> bool {
        match self {
            View::Album(key) => library.album(key).is_some(),
            _ => true,
        }
    }
}

/// Something the user asked for, queued while a view draws and applied straight after.
#[derive(Clone, Debug)]
pub enum Action {
    /// Show another view (pushes the current one on the back stack).
    Go(View),
    /// Pop the back stack.
    Back,
    /// Select an artist by display name in the Artists view.
    ///
    /// The name must be one the Artists view can actually land on — build it with
    /// [`artist_target`] rather than from a track's `artist` tag, which is frequently not
    /// an artist page at all (features, remixes, compilations).
    GoArtist(String),
    /// Make `tracks` the queue context and start playing at the row the user pointed at.
    ///
    /// For "play this whole album/playlist", where no row was pointed at, raise
    /// [`Action::PlayCollection`] instead — with shuffle on the two differ (UI-SPEC v1.2
    /// §Shuffle correctness).
    Play {
        /// The context, in the order the view shows it.
        tracks: Vec<TrackId>,
        /// Index into `tracks` to start on. Honoured even with shuffle on: the user named
        /// this song.
        index: usize,
        /// The `SHUFFLE` button: turn shuffle on and draw a fresh uniformly random order,
        /// `index` included, re-rolled on every press.
        shuffle: bool,
    },
    /// Play a whole collection with no start row named — the `▶ PLAY` buttons, the album
    /// card's hover badge, the context menu's `Play`. Clears shuffle and plays linearly
    /// from the top; only a named row or the `SHUFFLE` button keeps/starts shuffling.
    PlayCollection(Vec<TrackId>),
    /// Put `tracks` at the front of the manual queue.
    PlayNext(Vec<TrackId>),
    /// Put `tracks` at the back of the manual queue.
    PlayLater(Vec<TrackId>),
    /// Append `tracks` to an existing playlist.
    AddToPlaylist(u64, Vec<TrackId>),
    /// Create an auto-named playlist and append `tracks` to it.
    NewPlaylistWith(Vec<TrackId>),
    /// Create an empty auto-named playlist (the sidebar's `+ NEW PLAYLIST`).
    NewPlaylist,
    /// Open a playlist and start its inline rename (the sidebar's `Rename`).
    StartRename(u64),
    /// `F2`: rename whatever the current view is renameable for (a playlist).
    RenameShortcut,
    /// Commit an inline rename.
    RenamePlaylist(u64, String),
    /// Start the sidebar's two-click delete confirmation for a playlist.
    AskDeletePlaylist(u64),
    /// Abandon the delete confirmation.
    CancelDelete,
    /// Delete a playlist for real.
    DeletePlaylist(u64),
    /// Drop the entry at `index` (an index into `Playlist::entries`, not a track id, so
    /// duplicates of the same song are removed one at a time).
    RemoveFromPlaylist(u64, usize),
    /// Move the entry at `from` to `to` (both are `Playlist::entries` indices).
    MovePlaylistEntry(u64, usize, usize),
    /// Play the `idx`-th row of the Up Next drawer, consuming everything above it.
    QueueJump(usize),
    /// Drop the `idx`-th row of the Up Next drawer.
    QueueRemove(usize),
    /// Empty the manual queue (the drawer's `CLEAR`).
    QueueClear,
    /// Play / pause.
    TogglePlay,
    /// Skip forward.
    Next,
    /// Skip back (restart first if more than three seconds have elapsed).
    Prev,
    /// Unload the current track and go idle. Only the OS media controls raise this
    /// (MPRIS has a `Stop` button; macOS and the keyboard do not).
    Stop,
    /// Seek the current track.
    Seek(Duration),
    /// Live seek while the knob is being dragged (throttled by the controller).
    SeekLive(Duration),
    /// Set the UI volume, 0.0..=1.0.
    Volume(f32),
    /// Nudge the volume by a delta.
    VolumeBy(f32),
    /// Heart / unheart one track, and persist it immediately (UI-SPEC v1.3 §Favorites).
    ToggleFavTrack(TrackId),
    /// Heart / unheart one album, and persist it immediately.
    ToggleFavAlbum(AlbumKey),
    /// Toggle shuffle.
    ToggleShuffle,
    /// Cycle repeat Off → All → One.
    CycleRepeat,
    /// Show / hide the Up Next drawer.
    ToggleQueue,
    /// Settings: re-run the library scan over the root already in force.
    Rescan,
    /// Settings: adopt a library root (as typed, `~` allowed) and rescan. `None` restores
    /// the default `~/.phoebus`. A path that is not a directory is refused, and the view
    /// shows `NOT A DIRECTORY` instead.
    SetLibraryRoot(Option<String>),
    /// Settings: switch the palette between dark and light, live.
    SetThemeMode(ThemeMode),
    /// Settings: repaint with a new accent, live.
    SetAccent([u8; 3]),
    /// The Artists view's split was dragged: persist the new list width.
    ///
    /// The view has already moved its own copy in `ViewState` — this frame is drawn at the
    /// new width, not the next one. The action exists so the *persisting* still goes
    /// through the app, which is the only thing allowed to talk to the controller.
    SetArtistListW(f32),
    /// Move keyboard focus into the sidebar's search field.
    FocusSearch,
    /// Escape: unwind one step, innermost first — the add-songs picker, then a rename,
    /// then a delete confirmation, then the drawer, then search. `Phoebus::escape` owns
    /// the order and documents why it is that one.
    Escape,
}

/// The artist page a track navigates to, or `None` if the library has none for it.
///
/// `Library::artists()` is grouped by **album artist**, so a track's own `artist` tag is
/// often not a page: `HAZH` on HOME's *Odyssey* has no entry of its own, and neither does
/// any featured guest or `Various Artists` filler. Resolution order:
///
/// 1. the album artist of the track's album — always a real page, by construction;
/// 2. a case-insensitive match of the track artist against the artist index, for a track
///    whose album somehow did not survive the scan;
/// 3. the raw tag, which the Artists view will fail to resolve and answer with *no*
///    selection rather than silently landing the user on artist `[0]`.
///
/// `None` only for a track that is not in the library, or one with no artist at all —
/// there is nothing to offer, so the caller must not navigate.
pub fn artist_target(library: &Library, id: TrackId) -> Option<String> {
    let track = library.track(id)?;
    if let Some(album) = library.album(&track.album_key) {
        return Some(album.artist.clone());
    }
    if let Some(artist) = library.artist(&track.artist) {
        return Some(artist.name.clone());
    }
    let raw = track.artist.trim();
    (!raw.is_empty()).then(|| raw.to_string())
}

/// An empty, disk-detached [`Favorites`] for the crate's headless render tests.
///
/// Ephemeral, so that a test which does toggle something can never touch a file: the path
/// is fictional, and `set_ephemeral` makes every save a successful no-op regardless.
#[cfg(test)]
pub fn test_favorites() -> Favorites {
    let mut favs =
        Favorites::load_from(std::path::Path::new("/nonexistent/phoebus/favorites.json"));
    favs.set_ephemeral(true);
    favs
}

/// Now-playing summary handed to views so they can mark the playing row.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Now {
    /// The loaded track, playing or paused.
    pub track: Option<TrackId>,
    /// True while audio is actually running.
    pub playing: bool,
}

impl Now {
    /// True if `id` is the loaded track.
    pub fn is_current(&self, id: TrackId) -> bool {
        self.track == Some(id)
    }
}

/// Pre-rendered `M:SS` / `H:MM:SS` strings, built once per scan so no view formats on the
/// frame path.
#[derive(Default)]
pub struct Fmt {
    durations: HashMap<TrackId, String>,
}

impl Fmt {
    /// Pre-format every track's duration.
    pub fn build(library: &Library) -> Fmt {
        let mut durations = HashMap::with_capacity(library.track_count());
        for id in library.tracks_sorted() {
            if let Some(t) = library.track(*id) {
                durations.insert(*id, mmss(t.duration));
            }
        }
        Fmt { durations }
    }

    /// The formatted duration of a track, or `-:--` if it is not in the library.
    pub fn dur(&self, id: TrackId) -> &str {
        self.durations.get(&id).map_or("-:--", String::as_str)
    }
}

/// `M:SS`, or `H:MM:SS` past an hour (UI-SPEC v1.2 §Durations).
///
/// The minutes field is *within the hour*, which is the whole trap here: a 1 h 1 min 5 s
/// track is `1:01:05`, never `61:05`. Nothing may cap or wrap at 59:59 — DJ sets, live
/// albums ripped as one file and audiobook chapters all land past the hour.
pub fn mmss(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// An album/playlist total: `M MIN`, or `N HR M MIN` past an hour (UI-SPEC v1.2
/// §Durations). Both fields are rounded down, and the minutes are within the hour — a
/// 90-minute album reads `1 HR 30 MIN`, not `90 MIN` and certainly not `1 HR 90 MIN`.
pub fn minutes(d: Duration) -> String {
    let total_min = d.as_secs() / 60;
    let (h, m) = (total_min / 60, total_min % 60);
    if h > 0 {
        format!("{h} HR {m} MIN")
    } else {
        format!("{m} MIN")
    }
}

/// Everything a view may read while drawing, plus the outbox it writes into.
pub struct Ctx<'a> {
    /// The scanned library.
    pub lib: &'a Library,
    /// Cover cache; ask it for textures, never decode on the frame path.
    pub art: &'a mut Artwork,
    /// Playlists, for the `Add to Playlist ▸` submenus.
    pub playlists: &'a [Playlist],
    /// What is hearted. Read once per row and once per cover, every frame — both lookups
    /// are O(1), which is the reason the store keeps a `TrackId` set at all.
    pub favs: &'a Favorites,
    /// What is loaded right now.
    pub now: Now,
    /// Pre-formatted durations.
    pub fmt: &'a Fmt,
    /// Actions raised by this frame.
    pub actions: &'a mut Vec<Action>,
}

impl Ctx<'_> {
    /// Queue an action for the app to apply after the frame.
    pub fn act(&mut self, action: Action) {
        self.actions.push(action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_state_round_trips() {
        for v in [
            View::RecentlyAdded,
            View::Favorites,
            View::Albums,
            View::Artists,
            View::Songs,
            View::Playlist(7),
            View::Settings,
            View::Album(AlbumKey::new("HOME", "Odyssey")),
        ] {
            assert_eq!(View::from_state(&v.to_state()), v);
        }
        assert_eq!(View::from_state("nonsense"), View::RecentlyAdded);
        assert_eq!(View::from_state("playlist:xx"), View::RecentlyAdded);
    }

    /// UI-SPEC v1.2 §Durations. The named bug: a 1 h 1 min 5 s track showing `61:05`,
    /// which is what any `total/60` minutes field produces once the hour is split off.
    #[test]
    fn durations_format() {
        assert_eq!(mmss(Duration::from_secs(0)), "0:00");
        assert_eq!(mmss(Duration::from_secs(65)), "1:05");
        assert_eq!(mmss(Duration::from_secs(3599)), "59:59");
        assert_eq!(mmss(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(mmss(Duration::from_secs(3665)), "1:01:05", "not 61:05");
        assert_eq!(mmss(Duration::from_secs(3725)), "1:02:05");
        assert_eq!(mmss(Duration::from_secs(7 * 3600 + 4)), "7:00:04");
    }

    /// Album and playlist totals: `N HR M MIN` past the hour, and the minutes stay inside
    /// the hour (a 90-minute album is `1 HR 30 MIN`, not `1 HR 90 MIN`).
    #[test]
    fn totals_format() {
        assert_eq!(minutes(Duration::from_secs(0)), "0 MIN");
        assert_eq!(minutes(Duration::from_secs(119)), "1 MIN");
        assert_eq!(minutes(Duration::from_secs(59 * 60)), "59 MIN");
        assert_eq!(minutes(Duration::from_secs(60 * 60)), "1 HR 0 MIN");
        assert_eq!(minutes(Duration::from_secs(90 * 60)), "1 HR 30 MIN");
        assert_eq!(minutes(Duration::from_secs(90 * 60 + 59)), "1 HR 30 MIN");
        assert_eq!(minutes(Duration::from_secs(25 * 3600 + 61)), "25 HR 1 MIN");
    }
}
