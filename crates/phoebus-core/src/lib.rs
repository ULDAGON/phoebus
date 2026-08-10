//! `phoebus-core` — the library model, scanner, playlists, favorites, play queue, search and
//! persisted state for Phoebus. No UI and no audio dependencies: everything here is plain
//! data and pure logic, so it can be unit-tested and driven from any front-end.
//!
//! ```no_run
//! use phoebus_core::{AdvanceReason, Library, PlayQueue, PlaylistStore, AppState, Dirs, scanner};
//!
//! let dirs = Dirs::resolve();                       // $PHOEBUS_DATA or ~/.phoebus/.phoebus
//! dirs.ensure_dirs().ok();
//! let state = AppState::load_from(&dirs.state_path());
//! let playlists = PlaylistStore::load_from(&dirs.playlists_path());
//!
//! // $PHOEBUS_LIBRARY > state.json's library_root > ~/.phoebus
//! let root = phoebus_core::library_root_for(state.configured_library_root());
//! let library: Library = scanner::scan_with_covers(&root, &dirs.covers_dir());
//!
//! let mut queue = PlayQueue::new();
//! queue.set_shuffle(state.shuffle);
//! queue.set_repeat(state.repeat);
//! queue.set_context(library.tracks_sorted().to_vec(), 0);
//! let next = queue.advance(AdvanceReason::UserNext);
//! ```
//!
//! Layout on disk (v1.1): the library root holds `Artist/Album/track` files and is only ever
//! read; everything Phoebus writes lives in the app-data directory ([`Dirs`], `$PHOEBUS_DATA`
//! or `~/.phoebus/.phoebus`). The default library root is `~/.phoebus`, whose `.phoebus`
//! subdirectory *is* the default data dir, so the pre-v1.1 layout is unchanged for anyone who
//! never configures a root — existing `state.json`, `playlists.json` and `cache/covers/` keep
//! working with no migration step at all.
//!
//! The old root-relative path API is gone. `Dirs` is the only thing that knows the layout;
//! [`Dirs::inside`](paths::Dirs::inside) is the single remaining way to say "the data dir that
//! belongs to this root", and it is what the self-contained [`scan`] / [`Library::build`] pair
//! uses for tests and probes.

pub mod color;
pub mod favorites;
pub mod model;
pub mod paths;
pub mod playlists;
pub mod queue;
pub mod scanner;
pub mod search;
pub mod state;

pub use color::{format_hex_color, parse_hex_color};
pub use favorites::{Favorites, pinned_albums};
pub use model::{
    Album, AlbumKey, Artist, Library, Track, TrackId, UNKNOWN_ALBUM, UNKNOWN_ARTIST, UNKNOWN_TITLE,
};
pub use paths::{
    APP_DIR_NAME, DATA_ENV, Dirs, LIBRARY_ENV, data_dir, default_library_root, expand_tilde,
    fnv1a_64, home_dir, library_root_for, resolve_library_root,
};
pub use playlists::{Playlist, PlaylistStore};
pub use queue::{AdvanceReason, PlayQueue, Repeat, UpNext};
pub use scanner::{
    AUDIO_EXTENSIONS, COVER_MAX_EDGE, ScanPhase, ScanProgress, scan, scan_with_covers,
    scan_with_covers_progress, scan_with_progress,
};
pub use search::{SECTION_CAP, SearchResults, filter_tracks, search, search_capped};
pub use state::{
    ARTIST_LIST_WIDTH, AppState, DEFAULT_ACCENT, PanelWidth, QUEUE_WIDTH, SIDEBAR_WIDTH, ThemeMode,
};
