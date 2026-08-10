//! The root app: panels, routing, the scan thread, and the one place where an [`Action`]
//! turns into a state change.
//!
//! eframe 0.36 has no `App::update`: channel polling and screenshot handling live in
//! [`Phoebus::logic`], all painting in [`Phoebus::ui`] (API-FACTS §3.1).

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::Receiver;
use egui::{Align2, Color32, Rect, Sense, Ui, Vec2};
use phoebus_core::{
    AppState, Dirs, Library, Playlist, PlaylistStore, ScanPhase, ScanProgress, TrackId, scanner,
};

use crate::artwork::Artwork;
use crate::controller::Controller;
use crate::media_keys::MediaKeys;
use crate::nav::{Action, Ctx, Fmt, Now, View};
use crate::shots::{self, Capture, Shot, Step, Tour};
use crate::theme;
use crate::views::{self, ViewState};
use crate::widgets::{self, menus, player_bar};

/// How many views back the `←` row can walk.
const BACK_STACK_CAP: usize = 32;
/// How many times per frame the action buffer is drained before we call it a cycle.
const ACTION_ROUNDS: usize = 8;

/// What the scan thread sends home.
enum ScanMsg {
    /// A progress tick.
    Progress(ScanProgress),
    /// The finished library (boxed: it is far larger than a tick).
    Done(Box<Library>),
}

/// A scan in flight.
struct Scan {
    rx: Receiver<ScanMsg>,
    progress: ScanProgress,
}

/// One step of history. The query travels with the view because a `View::Search` restored
/// after the sidebar field was cleared would otherwise land the user on `SEARCH ""` with
/// `NO RESULTS` and no way back except the sidebar.
struct Back {
    view: View,
    /// The search text at the moment this view was left.
    query: String,
}

/// The whole application.
pub struct Phoebus {
    /// Where Phoebus writes: `state.json`, `playlists.json`, `cache/covers/`. Fixed for the
    /// run — changing the library root in Settings does not move it.
    dirs: Dirs,
    /// The music being scanned. Read-only, and swappable from the Settings view.
    root: PathBuf,
    /// `$PHOEBUS_LIBRARY`, when it is set: it outranks the Settings root.
    library_env: Option<String>,
    /// `~/.phoebus` — what `RESET TO DEFAULT` restores.
    default_root: PathBuf,
    library: Library,
    fmt: Fmt,
    scan: Option<Scan>,
    artwork: Artwork,
    controller: Controller,
    /// OS media keys and the Now Playing card. Disabled, never absent, if the platform
    /// would not hand them over.
    media_keys: MediaKeys,

    view: View,
    back_stack: Vec<Back>,
    search: String,
    search_return: Option<View>,
    focus_search: bool,
    /// Up Next drawer visibility.
    queue_open: bool,
    /// Per-view UI state: artist selection, song sort, inline rename, search cache.
    vstate: ViewState,

    /// The `--shot` tour's in-memory demo playlist. Never written to disk.
    demo: Option<Playlist>,
    /// Store playlists plus [`Phoebus::demo`]; only used while a demo exists.
    playlists_all: Vec<Playlist>,

    actions: Vec<Action>,
    shot: Option<Shot>,
    tour: Option<Tour>,
}

impl Phoebus {
    /// Build the app: install the theme, wire the controller, start the first scan.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        dirs: Dirs,
        root: PathBuf,
        state: AppState,
        capture: Option<Capture>,
    ) -> Phoebus {
        // `PHOEBUS_THEME` if it is set, else what `state.json` remembers. Read before the
        // state is handed to the controller, and applied below through the same setter the
        // Settings view uses, so there is only ever one way to change the palette.
        let (mode, accent) = theme::resolve(&state);
        let playlists = PlaylistStore::load_from(&dirs.playlists_path());
        let view = View::from_state(&state.last_view);
        // A tour must never be audible, whatever the environment says.
        let touring = matches!(capture, Some(Capture::Tour(_)));
        let controller = Controller::new(dirs.clone(), state, playlists, touring);
        let (shot, tour) = match capture {
            Some(Capture::Once(path)) => (Some(Shot::new(path)), None),
            Some(Capture::Tour(dir)) => (None, Some(Tour::new(dir))),
            None => (None, None),
        };
        let mut app = Phoebus {
            library: Library::empty_with_covers(&root, dirs.covers_dir()),
            library_env: std::env::var(phoebus_core::LIBRARY_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty()),
            default_root: phoebus_core::default_library_root(&phoebus_core::home_dir()),
            dirs,
            fmt: Fmt::default(),
            scan: None,
            artwork: Artwork::new(),
            controller,
            media_keys: MediaKeys::new(&cc.egui_ctx),
            view,
            back_stack: Vec::new(),
            search: String::new(),
            search_return: None,
            focus_search: false,
            queue_open: false,
            vstate: ViewState::default(),
            demo: None,
            playlists_all: Vec::new(),
            actions: Vec::new(),
            shot,
            tour,
            root,
        };
        // The two top-level panels read their persisted width off the controller every
        // frame; the Artists split is drawn by a view, which never sees the controller, so
        // its width is seeded into `ViewState` here instead and travels back out as an
        // `Action::SetArtistListW`.
        app.vstate.artists.list_w = app.controller.artist_list_w();
        // Before the first frame: nothing above this line paints.
        app.set_theme(&cc.egui_ctx, mode, accent);
        app.start_scan();
        app
    }

    /// Repaint the whole app in `mode` + `accent`, and remember the choice.
    ///
    /// The one way the palette changes: it publishes the [`theme::Palette`] the hand-painted
    /// widgets read, rebuilds the egui `Style` the built-in ones read, and hands the choice
    /// to the controller, which persists it through the ordinary `state.json` debounce
    /// (unless `PHOEBUS_THEME` is holding the theme for this run).
    fn set_theme(&mut self, ctx: &egui::Context, mode: phoebus_core::ThemeMode, accent: Color32) {
        theme::apply(ctx, mode, accent);
        self.controller.set_theme(mode, theme::rgb(accent));
        ctx.request_repaint();
    }

    // ---- library scan ----------------------------------------------------------------

    fn start_scan(&mut self) {
        if self.scan.is_some() {
            return;
        }
        let root = self.root.clone();
        // Covers land in the app-data dir, never inside the library root — that is what
        // makes pointing Phoebus at someone's Apple Music folder a read-only operation.
        let covers = self.dirs.covers_dir();
        let (tx, rx) = crossbeam_channel::unbounded::<ScanMsg>();
        let progress_tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("phoebus-scan".to_string())
            .spawn(move || {
                let library = scanner::scan_with_covers_progress(&root, &covers, |p| {
                    // Drop `current` here rather than shipping a heap-allocated path per
                    // audio file down an unbounded channel: the scanning screen shows the
                    // phase and the counts, and nothing in the crate reads the name.
                    let _ =
                        progress_tx.send(ScanMsg::Progress(ScanProgress { current: None, ..p }));
                });
                let _ = tx.send(ScanMsg::Done(Box::new(library)));
            });
        match spawned {
            Ok(_) => {
                self.scan = Some(Scan {
                    rx,
                    progress: ScanProgress {
                        phase: ScanPhase::Discovering,
                        done: 0,
                        total: None,
                        tracks: 0,
                        current: None,
                    },
                });
            }
            Err(e) => log::error!("scan: could not spawn the scanner thread: {e}"),
        }
    }

    fn poll_scan(&mut self) {
        let mut finished = None;
        if let Some(scan) = &mut self.scan {
            for msg in scan.rx.try_iter() {
                match msg {
                    ScanMsg::Progress(p) => scan.progress = p,
                    ScanMsg::Done(library) => finished = Some(*library),
                }
            }
        }
        if let Some(library) = finished {
            log::info!(
                "scan: {} tracks, {} albums, {} artists",
                library.track_count(),
                library.album_count(),
                library.artist_count()
            );
            self.artwork.reset(library.covers_dir());
            self.fmt = Fmt::build(&library);
            if !self.view.is_valid(&library) {
                self.view = View::RecentlyAdded;
            }
            self.library = library;
            self.scan = None;
            // The hearted paths are the durable truth; their ids have to be re-derived
            // against the library that just arrived, exactly as playlist entries are.
            self.controller.favorites.resolve(&self.library);
            // Sort orders, search hits, artist indices and both favourites caches all
            // referred to the old library.
            self.vstate.library_changed();
        }
    }

    fn library_ready(&self) -> bool {
        self.scan.is_none()
    }

    // ---- actions ---------------------------------------------------------------------

    /// Drain the frame's actions. Applying one may raise another (a new playlist navigates
    /// to itself), so this drains until the buffer is empty — with a cap, because a cycle
    /// must slow the app down rather than hang it.
    fn apply_actions(&mut self, ctx: &egui::Context) {
        for _ in 0..ACTION_ROUNDS {
            if self.actions.is_empty() {
                return;
            }
            for action in std::mem::take(&mut self.actions) {
                self.apply(ctx, action);
            }
        }
        if !self.actions.is_empty() {
            log::warn!("actions: giving up after {ACTION_ROUNDS} rounds");
            self.actions.clear();
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Go(view) => self.navigate(view),
            Action::Back => self.back(),
            Action::GoArtist(name) => self.vstate.pending_artist = Some(name),
            Action::Play {
                tracks,
                index,
                shuffle,
            } => {
                if shuffle {
                    self.controller.shuffle_context(&self.library, tracks);
                } else {
                    self.controller.play_context(&self.library, tracks, index);
                }
            }
            Action::PlayCollection(tracks) => {
                self.controller.play_collection(&self.library, tracks);
            }
            Action::PlayNext(tracks) => self.controller.queue.play_next(tracks),
            Action::PlayLater(tracks) => self.controller.queue.play_later(tracks),
            Action::AddToPlaylist(id, tracks) => self.add_to_playlist(id, &tracks),
            Action::NewPlaylistWith(tracks) => {
                if let Some(id) = self.create_playlist() {
                    self.add_to_playlist(id, &tracks);
                }
            }
            Action::NewPlaylist => {
                if let Some(id) = self.create_playlist() {
                    self.start_rename(id);
                }
            }
            Action::StartRename(id) => self.start_rename(id),
            Action::RenameShortcut => {
                if let View::Playlist(id) = self.view {
                    self.start_rename(id);
                }
            }
            Action::RenamePlaylist(id, name) => self.rename_playlist(id, &name),
            Action::AskDeletePlaylist(id) => self.vstate.confirm_delete = Some(id),
            Action::CancelDelete => self.vstate.confirm_delete = None,
            Action::DeletePlaylist(id) => self.delete_playlist(id),
            Action::RemoveFromPlaylist(id, index) => {
                if !self.is_demo(id) {
                    if let Err(e) = self.controller.playlists.remove_at(id, index) {
                        log::warn!("playlists: could not remove entry {index}: {e:#}");
                    }
                    self.playlists_changed();
                }
            }
            Action::MovePlaylistEntry(id, from, to) => {
                if !self.is_demo(id) {
                    if let Err(e) = self.controller.playlists.move_entry(id, from, to) {
                        log::warn!("playlists: could not move entry {from} to {to}: {e:#}");
                    }
                    self.playlists_changed();
                }
            }
            Action::QueueJump(index) => self.controller.queue_jump(&self.library, index),
            Action::QueueRemove(index) => {
                self.controller.queue.remove_upcoming(index);
            }
            Action::QueueClear => self.controller.queue.clear_manual(),
            Action::TogglePlay => self.controller.toggle_play(),
            Action::Next => self.controller.next(&self.library),
            Action::Prev => self.controller.prev(&self.library),
            Action::Stop => self.controller.stop(),
            Action::Seek(pos) => self.controller.seek(pos),
            Action::SeekLive(pos) => self.controller.seek_live(pos),
            Action::Volume(v) => self.controller.set_volume(v),
            Action::VolumeBy(delta) => self.controller.volume_by(delta),
            Action::ToggleFavTrack(id) => self.toggle_fav_track(id),
            Action::ToggleFavAlbum(key) => self.toggle_fav_album(&key),
            Action::ToggleShuffle => self.controller.toggle_shuffle(),
            Action::CycleRepeat => self.controller.cycle_repeat(),
            Action::ToggleQueue => self.queue_open = !self.queue_open,
            Action::Rescan => self.start_scan(),
            Action::SetLibraryRoot(typed) => self.set_library_root(typed),
            Action::SetThemeMode(mode) => {
                let accent = theme::p().accent;
                self.set_theme(ctx, mode, accent);
            }
            Action::SetAccent(rgb) => {
                let mode = theme::p().mode;
                self.set_theme(ctx, mode, theme::color(rgb));
            }
            Action::SetArtistListW(w) => self.controller.set_artist_list_w(w),
            Action::FocusSearch => self.focus_search = true,
            Action::Escape => self.escape(),
        }
    }

    /// Settings' `APPLY & RESCAN` / `RESET TO DEFAULT`.
    ///
    /// `typed` is what the user wrote (`None` = the default root). A path that is not a
    /// directory changes nothing and leaves `NOT A DIRECTORY` under the input — the setting
    /// is only written once it is known to be usable. Otherwise: persist, stop playback (the
    /// loaded track is about to leave the library), forget every cover texture, and rescan.
    /// The data directory does not move, so `playlists.json` and the cover cache stay put.
    fn set_library_root(&mut self, typed: Option<String>) {
        let candidate = views::settings::resolve_typed(typed.as_deref());
        if !candidate.is_dir() {
            log::warn!(
                "settings: {} is not a directory; keeping {}",
                candidate.display(),
                self.root.display()
            );
            self.vstate.settings.not_a_directory = true;
            return;
        }
        self.controller.set_library_root(typed);
        // Never the candidate directly: `$PHOEBUS_LIBRARY` still outranks the setting, and
        // this is the one function that decides what is actually scanned.
        let root = phoebus_core::library_root_for(self.controller.configured_library_root());
        log::info!("settings: library root -> {}", root.display());

        self.controller.stop();
        self.root = root;
        self.library = Library::empty_with_covers(&self.root, self.dirs.covers_dir());
        // Nothing resolves against an empty library; the scan below resolves them again.
        self.controller.favorites.resolve(&self.library);
        self.fmt = Fmt::default();
        self.artwork.reset(self.dirs.covers_dir());
        self.vstate.library_changed();
        self.vstate.settings.reset_input();
        self.back_stack.clear();
        self.start_scan();
    }

    fn navigate(&mut self, view: View) {
        if view == self.view {
            return;
        }
        // The picker belongs to the page it was opened from. It blocks the pointer while it
        // is up, so the only way here is a keyboard shortcut — and coming back to the
        // playlist later to find the popup still standing would be a haunting, not a
        // convenience.
        self.vstate.playlist.close_picker();
        if self.back_stack.len() >= BACK_STACK_CAP {
            self.back_stack.remove(0);
        }
        let query = self.search.clone();
        self.back_stack.push(Back {
            view: std::mem::replace(&mut self.view, view),
            query,
        });
        self.remember_view();
    }

    /// Pop the back stack, skipping anything that is no longer a page.
    ///
    /// A stacked `View::Search` is only a page while it still has a query: the user can
    /// empty the sidebar field from another view, which leaves the stacked entry pointing
    /// at `SEARCH ""`. Restoring the query it was left with fixes the common case; an
    /// entry whose query was genuinely empty is dropped instead of shown.
    fn back(&mut self) {
        while let Some(entry) = self.back_stack.pop() {
            if entry.view == View::Search {
                if entry.query.trim().is_empty() {
                    continue;
                }
                if self.search != entry.query {
                    self.search = entry.query;
                    self.vstate.search.invalidate();
                }
            }
            self.view = entry.view;
            self.remember_view();
            return;
        }
        self.view = View::Albums;
        self.remember_view();
    }

    fn remember_view(&mut self) {
        if self.view != View::Search {
            self.controller.set_last_view(self.view.to_state());
        }
    }

    /// `Esc`, innermost first: dismiss the add-songs picker, then cancel a rename, then a
    /// delete confirmation, then the drawer, then search.
    ///
    /// The picker leads because it is a modal — it is literally on top of everything else,
    /// including a rename it may have been opened over, so it is what `Esc` is aimed at.
    /// It is unwound here rather than by `egui::Modal::should_close` on purpose: that helper
    /// *consumes* the key, which would take this ordering apart from the inside.
    fn escape(&mut self) {
        if self.vstate.playlist.close_picker() {
            return;
        }
        if self.vstate.playlist.cancel_rename() {
            return;
        }
        if self.vstate.confirm_delete.take().is_some() {
            return;
        }
        if self.queue_open {
            self.queue_open = false;
        } else if !self.search.is_empty() {
            self.search.clear();
            self.leave_search();
        }
    }

    fn leave_search(&mut self) {
        if self.view == View::Search {
            self.view = self.search_return.take().unwrap_or(View::Albums);
            self.remember_view();
        }
    }

    // ---- playlists -------------------------------------------------------------------

    fn create_playlist(&mut self) -> Option<u64> {
        let id = match self.controller.playlists.create(None) {
            Ok(id) => id,
            Err(e) => {
                log::warn!("playlists: could not create: {e:#}");
                return None;
            }
        };
        self.playlists_changed();
        Some(id)
    }

    fn add_to_playlist(&mut self, id: u64, tracks: &[TrackId]) {
        if self.is_demo(id) {
            return;
        }
        if let Err(e) = self
            .controller
            .playlists
            .append_tracks(id, &self.library, tracks)
        {
            log::warn!("playlists: could not add tracks: {e:#}");
        }
        self.playlists_changed();
    }

    /// After any playlist mutation: the resolved-row cache is stale, and so is the merged
    /// list the `--shot` tour's demo playlist lives in.
    fn playlists_changed(&mut self) {
        self.vstate.playlist.invalidate();
        if self.demo.is_some() {
            self.rebuild_playlists();
        }
    }

    /// Open a playlist and put its title into edit mode.
    fn start_rename(&mut self, id: u64) {
        let Some(name) = self.playlist_name(id) else {
            return;
        };
        self.navigate(View::Playlist(id));
        self.vstate.confirm_delete = None;
        self.vstate.playlist.start_rename(id, &name);
    }

    fn rename_playlist(&mut self, id: u64, name: &str) {
        if name.trim().is_empty() {
            return;
        }
        if self.is_demo(id) {
            if let Some(demo) = &mut self.demo {
                demo.name = name.trim().to_string();
            }
            self.rebuild_playlists();
            return;
        }
        if let Err(e) = self.controller.playlists.rename(id, name) {
            log::warn!("playlists: could not rename: {e:#}");
        }
        self.playlists_changed();
    }

    fn delete_playlist(&mut self, id: u64) {
        self.vstate.confirm_delete = None;
        if self.is_demo(id) {
            return;
        }
        if let Err(e) = self.controller.playlists.delete(id) {
            log::warn!("playlists: could not delete: {e:#}");
            return;
        }
        self.playlists_changed();
        // Never leave the user (or the back stack) staring at a deleted playlist.
        self.back_stack.retain(|e| e.view != View::Playlist(id));
        if self.view == View::Playlist(id) {
            self.view = View::Albums;
            self.remember_view();
        }
    }

    fn is_demo(&self, id: u64) -> bool {
        self.demo.as_ref().is_some_and(|d| d.id == id)
    }

    fn playlist_name(&self, id: u64) -> Option<String> {
        if let Some(demo) = &self.demo
            && demo.id == id
        {
            return Some(demo.name.clone());
        }
        self.controller.playlists.get(id).map(|p| p.name.clone())
    }

    fn rebuild_playlists(&mut self) {
        self.playlists_all = self.controller.playlists.playlists().to_vec();
        if let Some(demo) = &self.demo {
            self.playlists_all.push(demo.clone());
        }
    }

    // ---- favorites -------------------------------------------------------------------

    /// Heart / unheart one song. The store saves itself; all this owes it is the two caches
    /// that were derived from the old answer.
    ///
    /// The Favorites view's rows obviously change. The Albums view's FAVORITES section does
    /// not — a *track* is not an album, so a track heart cannot change which albums are
    /// hearted — and its cache is deliberately left alone.
    fn toggle_fav_track(&mut self, id: TrackId) {
        let hearted = self.controller.favorites.toggle_track(&self.library, id);
        log::debug!("favorites: track {id:?} -> {hearted}");
        self.vstate.favorites.invalidate();
    }

    /// Heart / unheart one album. The mirror image: the Albums view's FAVORITES section
    /// changes and the Favorites view's rows do not — hearting an album does not heart its
    /// songs, and UI-SPEC v1.3 lists only songs in that view.
    fn toggle_fav_album(&mut self, key: &phoebus_core::AlbumKey) {
        let hearted = self.controller.favorites.toggle_album(key);
        log::debug!("favorites: album {key} -> {hearted}");
        self.vstate.albums.invalidate();
    }

    // ---- panels ----------------------------------------------------------------------

    /// Two stacked regions, bottom-anchored: the `SETTINGS` nav row keeps its exact height
    /// whatever the playlist list does, so the scroller in the body gets everything that is
    /// left.
    ///
    /// `SETTINGS` is the last row (UI-SPEC v1.4 §Sidebar footer — the stats/`RESCAN` footer
    /// that used to sit under it is gone, both live in Settings now). Its region reserves a
    /// `PANEL_PAD` gap underneath, so the row ends where the panel's own horizontal padding
    /// says an edge is, instead of butting into the player bar's hairline.
    fn sidebar(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let settings_h = theme::CARD_TEXT_GAP + theme::ROW_NAV + theme::PANEL_PAD;
        let split = |y: f32| (full.bottom() - y).max(full.top());
        let body = Rect::from_min_max(full.min, egui::pos2(full.right(), split(settings_h)));
        let settings = Rect::from_min_max(egui::pos2(full.left(), body.bottom()), full.max);

        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
            self.sidebar_body(ui);
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(settings), |ui| {
            ui.add_space(theme::CARD_TEXT_GAP);
            if nav_row(ui, "SETTINGS", self.view == View::Settings, Row::Standalone).clicked() {
                self.actions.push(Action::Go(View::Settings));
            }
        });
    }

    fn sidebar_body(&mut self, ui: &mut Ui) {
        // On macOS the window has no title bar of its own, so the traffic lights float over
        // exactly this corner (UI-SPEC v1.2 §Window chrome). Everything else — the content
        // views, the drawer — reaches the top of the window unpadded; only the wordmark has
        // to get out of the way.
        ui.add_space(theme::PANEL_PAD + theme::TITLEBAR_PAD);
        // The wordmark is the one place the accent is not "playing / active / primary" —
        // UI-SPEC gives it to the logo outright. No dot, no other decoration.
        widgets::label_bold(
            ui,
            &widgets::spaced("PHOEBUS"),
            theme::font_sub(),
            theme::p().accent_text,
        );
        ui.add_space(theme::CARD_TEXT_GAP + 2.0);

        self.search_field(ui);
        ui.add_space(theme::VIEW_PAD * 0.6);

        section_label(ui, "LIBRARY");
        for (label, view) in [
            ("RECENTLY ADDED", View::RecentlyAdded),
            ("FAVORITES", View::Favorites),
            ("ARTISTS", View::Artists),
            ("ALBUMS", View::Albums),
            ("SONGS", View::Songs),
        ] {
            let active = self.view == view;
            if nav_row(ui, label, active, Row::Item).clicked() {
                self.actions.push(Action::Go(view));
            }
        }

        ui.add_space(theme::VIEW_PAD * 0.6);
        section_label(ui, "PLAYLISTS");

        let Phoebus {
            controller,
            actions,
            view,
            vstate,
            demo,
            playlists_all,
            ..
        } = self;
        let playlists: &[Playlist] = if demo.is_some() {
            playlists_all.as_slice()
        } else {
            controller.playlists.playlists()
        };
        egui::ScrollArea::vertical()
            .id_salt("sidebar-playlists")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for playlist in playlists {
                    if vstate.confirm_delete == Some(playlist.id) {
                        confirm_row(ui, playlist.id, actions);
                        continue;
                    }
                    let active = *view == View::Playlist(playlist.id);
                    let response = nav_row(ui, &playlist.name, active, Row::Name);
                    if response.clicked() {
                        actions.push(Action::Go(View::Playlist(playlist.id)));
                    }
                    let id = playlist.id;
                    // Through the same wrapper as every track menu, so the sidebar's two
                    // verbs are set in the type UI-SPEC v1.2 §Menus asks for.
                    response.context_menu(|ui| {
                        menus::styled(ui, |ui| {
                            if ui.button("Rename").clicked() {
                                actions.push(Action::StartRename(id));
                                ui.close();
                            }
                            if ui.button("Delete").clicked() {
                                actions.push(Action::AskDeletePlaylist(id));
                                ui.close();
                            }
                        });
                    });
                }
                if nav_row(ui, "+ NEW PLAYLIST", false, Row::Item).clicked() {
                    actions.push(Action::NewPlaylist);
                }
            });
    }

    fn search_field(&mut self, ui: &mut Ui) {
        // The magnifier is a size of its own, not the label's: a hint is `SIZE_SMALL`, and
        // an 11 px icon beside 11 px capitals is the mismatch this pass exists to remove.
        // `RichText` cannot hold two sizes, so the hint is a laid-out galley instead.
        let hint = widgets::icon_text(
            ui,
            theme::GLYPH_SEARCH,
            theme::ICON_INLINE,
            &widgets::spaced("SEARCH"),
            theme::font_small(),
        );
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(egui::WidgetText::Galley(hint))
                .font(egui::TextStyle::Body)
                .text_color(theme::p().text_hi)
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(8, 6)),
        );
        if self.focus_search {
            self.focus_search = false;
            response.request_focus();
        }
        if response.changed() {
            if self.search.trim().is_empty() {
                self.leave_search();
            } else if self.view != View::Search {
                self.search_return = Some(self.view.clone());
                self.view = View::Search;
            }
        }
    }

    fn player_bar(&mut self, ui: &mut Ui) {
        let state = player_bar::BarState {
            playing: self.controller.is_playing(),
            pos: self.controller.display_pos(),
            duration: self.controller.duration(),
            seekable: self.controller.seekable(),
            error: self.controller.playback_error().map(str::to_string),
            volume: self.controller.volume(),
            shuffle: self.controller.shuffle(),
            repeat: self.controller.repeat(),
            queue_open: self.queue_open,
        };
        let Phoebus {
            library,
            artwork,
            controller,
            fmt,
            actions,
            demo,
            playlists_all,
            ..
        } = self;
        let now = controller.now();
        let playlists: &[Playlist] = if demo.is_some() {
            playlists_all.as_slice()
        } else {
            controller.playlists.playlists()
        };
        let mut cx = Ctx {
            lib: library,
            art: artwork,
            playlists,
            favs: &controller.favorites,
            now,
            fmt,
            actions,
        };
        player_bar::show(ui, &mut cx, &state);
    }

    /// The Up Next drawer: the manual queue, then the rest of the context.
    fn queue_drawer(&mut self, ui: &mut Ui) {
        let items = self.controller.queue.upcoming(theme::QUEUE_MAX);
        let Phoebus {
            library,
            artwork,
            controller,
            fmt,
            actions,
            demo,
            playlists_all,
            ..
        } = self;
        let now = controller.now();
        let playlists: &[Playlist] = if demo.is_some() {
            playlists_all.as_slice()
        } else {
            controller.playlists.playlists()
        };
        let mut cx = Ctx {
            lib: library,
            art: artwork,
            playlists,
            favs: &controller.favorites,
            now,
            fmt,
            actions,
        };
        widgets::queue::drawer(ui, &mut cx, &items);
    }

    fn content(&mut self, ui: &mut Ui) {
        if self.scan.is_some() {
            self.scanning_note(ui);
            return;
        }
        // The Settings view is the way *out* of an empty library, so it must draw even when
        // there is nothing to show anywhere else.
        if self.library.is_empty() && self.view != View::Settings {
            let root = views::settings::display_path(&self.root);
            views::page(ui, |ui| {
                views::centered_note(
                    ui,
                    &[
                        "NO MUSIC IN",
                        &root,
                        "",
                        "EXPECTED LAYOUT: ARTIST / ALBUM / TRACK.M4A",
                        "SUPPORTED: M4A MP3 FLAC OGG WAV AIFF AAC",
                    ],
                );
            });
            return;
        }
        // Cloned so the destructuring below can take `controller` mutably: this is the only
        // thing the Settings view needs that lives inside it.
        let configured = self
            .controller
            .configured_library_root()
            .map(str::to_string);
        let Phoebus {
            library,
            artwork,
            controller,
            fmt,
            actions,
            view,
            search,
            vstate,
            demo,
            playlists_all,
            root,
            default_root,
            library_env,
            ..
        } = self;
        let info = views::settings::Info {
            active_root: root,
            default_root,
            env_override: library_env.as_deref(),
            configured: configured.as_deref(),
        };
        let now = controller.now();
        let playlists: &[Playlist] = if demo.is_some() {
            playlists_all.as_slice()
        } else {
            controller.playlists.playlists()
        };
        let mut cx = Ctx {
            lib: library,
            art: artwork,
            playlists,
            favs: &controller.favorites,
            now,
            fmt,
            actions,
        };
        views::route(ui, &mut cx, view, search, vstate, &info);
    }

    fn scanning_note(&mut self, ui: &mut Ui) {
        let spinner = ['|', '/', '-', '\\'];
        let index = (ui.input(|i| i.time) * 8.0) as usize % spinner.len();
        let detail =
            self.scan
                .as_ref()
                .map_or_else(String::new, |scan| match scan.progress.phase {
                    ScanPhase::Discovering => "LOOKING FOR FILES".to_string(),
                    ScanPhase::Reading => format!("{} TRACKS READ", scan.progress.tracks),
                    ScanPhase::Artwork => "EXTRACTING ARTWORK".to_string(),
                    ScanPhase::Done => "FINISHING".to_string(),
                });
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter();
        painter.text(
            rect.center() - Vec2::new(0.0, 12.0),
            Align2::CENTER_CENTER,
            format!(
                "{} {}",
                spinner[index],
                widgets::spaced("SCANNING LIBRARY…")
            ),
            theme::font_body(),
            theme::p().text_low,
        );
        painter.text(
            rect.center() + Vec2::new(0.0, 12.0),
            Align2::CENTER_CENTER,
            widgets::spaced(&detail),
            theme::font_small(),
            theme::p().text_low,
        );
        ui.allocate_space(rect.size());
    }

    // ---- screenshots -----------------------------------------------------------------

    fn shot_autoplay(&mut self) {
        let wants =
            self.shot.as_ref().is_some_and(|s| s.autoplay && !s.played) && self.library_ready();
        if !wants {
            return;
        }
        if let Some(shot) = &mut self.shot {
            shot.played = true;
        }
        let Some(key) = self.library.albums().first().cloned() else {
            log::warn!("shot: no album to auto-play");
            return;
        };
        let tracks = self.library.album_tracks(&key).to_vec();
        log::info!("shot: auto-playing {key}");
        self.controller.play_context(&self.library, tracks, 0);
        self.controller.seek(shots::SHOT_SEEK);
    }

    fn shot_request(&mut self, ctx: &egui::Context) {
        // Wait for the scan *and* for every requested cover, so the frame is settled.
        let ready = self.library_ready() && self.artwork.is_idle();
        let playing_ready = self.controller.display_pos() >= shots::SHOT_MIN_POS;
        let Some(shot) = &mut self.shot else {
            return;
        };
        shot.frames += 1;
        if shot.should_request(ready, playing_ready) {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(())));
        }
    }

    fn shot_receive(&mut self, ctx: &egui::Context) {
        if self.shot.is_none() && self.tour.is_none() {
            return;
        }
        let captured = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = captured else {
            return;
        };
        if let Some(shot) = &self.shot {
            if let Err(e) = shot.save(&image) {
                log::error!("shot: {e:#}");
            }
            self.controller.save_now();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let done = self
            .tour
            .as_mut()
            .is_some_and(|tour| tour.save_and_advance(&image));
        if done {
            log::info!("shot: tour complete");
            self.tour = None;
            self.controller.save_now();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    // ---- the `--shot` tour -----------------------------------------------------------

    /// Put the app into the state the current tour step wants to photograph.
    fn tour_setup(&mut self) {
        let Some(step) = self.tour.as_ref().and_then(Tour::current) else {
            return;
        };
        if !self.tour.as_ref().is_some_and(Tour::needs_setup) {
            return;
        }
        if !self.library_ready() || self.library.is_empty() {
            return;
        }
        log::info!("shot: setting up {step:?}");
        self.tour_reset();
        // Before the first step, not at the Favorites one: `albums.png` is shot long before
        // `favorites.png` and has to show the FAVORITES section.
        self.seed_demo_favorites();
        match step {
            Step::Recently => self.view = View::RecentlyAdded,
            Step::Albums => self.view = View::Albums,
            Step::Album => {
                if let Some(key) = self.library.albums().first().cloned() {
                    self.view = View::Album(key);
                }
            }
            Step::Artists => {
                self.vstate.artists = views::artists::State::default();
                self.view = View::Artists;
            }
            Step::Songs => self.view = View::Songs,
            Step::Favorites => self.view = View::Favorites,
            Step::Playlist => {
                if let Some(id) = self.tour_playlist() {
                    self.view = View::Playlist(id);
                }
            }
            // The same page with the modal up. The `+` buttons are inert on the demo
            // playlist (`add_to_playlist` refuses it), which is all this step needs: it is
            // photographing the surface, not using it.
            Step::AddSongs => {
                if let Some(id) = self.tour_playlist() {
                    self.view = View::Playlist(id);
                    self.vstate.playlist.picker.open();
                }
            }
            Step::Search => {
                self.search = shots::TOUR_QUERY.to_string();
                self.vstate.search.invalidate();
                self.view = View::Search;
            }
            Step::Settings => self.view = View::Settings,
            Step::Playing => self.tour_playing(),
        }
        if let Some(tour) = &mut self.tour {
            tour.applied();
        }
    }

    /// Clear everything a previous step may have left on screen.
    fn tour_reset(&mut self) {
        self.queue_open = false;
        self.search.clear();
        self.vstate.playlist.cancel_rename();
        self.vstate.playlist.close_picker();
        self.vstate.confirm_delete = None;
        // The Settings step is photographed in its default state: no half-typed root, no
        // error left over from a previous step.
        self.vstate.settings.reset_input();
    }

    /// The playlist the tour photographs: the first real one, or an ephemeral demo.
    fn tour_playlist(&mut self) -> Option<u64> {
        if let Some(first) = self.controller.playlists.playlists().first() {
            return Some(first.id);
        }
        if self.demo.is_none() {
            self.build_demo_playlist();
        }
        self.demo.as_ref().map(|d| d.id)
    }

    /// Heart a handful of things so `favorites.png` has something in it — but only if the
    /// user has hearted nothing at all, exactly as the demo *playlist* only appears when
    /// there are no real ones.
    ///
    /// The store is put into ephemeral mode FIRST, so nothing below can reach the disk:
    /// `Favorites::toggle_*` saves on every call, and the tour must not create a
    /// `favorites.json` (nor add to an existing one) in anybody's data directory. That is
    /// also why a tour with real favorites still switches the store to ephemeral — the tour
    /// never toggles in that case, but the guarantee should not depend on it.
    ///
    /// Idempotent: it is called at the top of every step's setup, and does nothing after
    /// the first.
    fn seed_demo_favorites(&mut self) {
        if self.controller.favorites.is_ephemeral() {
            return;
        }
        self.controller.favorites.set_ephemeral(true);
        if !self.controller.favorites.is_empty() {
            log::info!("shot: the library already has favorites; not seeding demo ones");
            return;
        }
        let albums: Vec<phoebus_core::AlbumKey> = self.library.albums().to_vec();
        for key in albums.iter().take(shots::DEMO_FAV_TRACK_ALBUMS) {
            let tracks = self.library.album_tracks(key).to_vec();
            for index in shots::DEMO_FAV_TRACKS {
                if let Some(id) = tracks.get(index) {
                    self.controller.favorites.toggle_track(&self.library, *id);
                }
            }
        }
        for key in albums
            .get(shots::DEMO_FAV_ALBUMS)
            .unwrap_or_default()
            .iter()
        {
            self.controller.favorites.toggle_album(key);
        }
        log::info!(
            "shot: seeded {} ephemeral demo favorites ({} albums, {} tracks) — not persisted",
            self.controller.favorites.track_count() + self.controller.favorites.album_count(),
            self.controller.favorites.album_count(),
            self.controller.favorites.track_count()
        );
        self.vstate.albums.invalidate();
        self.vstate.favorites.invalidate();
    }

    /// Invent a playlist that spans every album. It lives in memory only — the tour must
    /// never write to the user's `playlists.json`.
    fn build_demo_playlist(&mut self) {
        let albums: Vec<phoebus_core::AlbumKey> = self.library.albums().to_vec();
        if albums.is_empty() {
            return;
        }
        let mut entries: Vec<String> = Vec::with_capacity(shots::DEMO_PLAYLIST_LEN);
        for round in 0..shots::DEMO_PLAYLIST_LEN {
            let before = entries.len();
            for key in &albums {
                if entries.len() >= shots::DEMO_PLAYLIST_LEN {
                    break;
                }
                let tracks = self.library.album_tracks(key);
                if let Some(id) = tracks.get(round * 3)
                    && let Some(track) = self.library.track(*id)
                {
                    entries.push(track.rel_path.clone());
                }
            }
            if entries.len() == before {
                break;
            }
        }
        log::info!(
            "shot: built the ephemeral demo playlist with {} tracks",
            entries.len()
        );
        self.demo = Some(Playlist {
            id: shots::DEMO_PLAYLIST_ID,
            name: shots::DEMO_PLAYLIST.to_string(),
            entries,
            created_at: 0,
            modified_at: 1,
        });
        self.rebuild_playlists();
    }

    /// Load the first album's first track, park it paused at 30 s, and fill the drawer.
    fn tour_playing(&mut self) {
        let Some(key) = self.library.albums().first().cloned() else {
            return;
        };
        let tracks = self.library.album_tracks(&key).to_vec();
        let manual: Vec<TrackId> = self
            .library
            .albums()
            .get(1)
            .map(|other| {
                self.library
                    .album_tracks(other)
                    .iter()
                    .copied()
                    .take(shots::TOUR_MANUAL)
                    .collect()
            })
            .unwrap_or_default();
        self.view = View::Album(key);
        self.queue_open = true;
        self.controller.play_context(&self.library, tracks, 0);
        self.controller.queue.play_next(manual);
        self.controller.seek(shots::SHOT_SEEK);
        self.controller.pause();
    }

    fn tour_request(&mut self, ctx: &egui::Context) {
        let Some(step) = self.tour.as_ref().and_then(Tour::current) else {
            return;
        };
        let settled = self.library_ready() && self.artwork.is_idle();
        let ready = match step {
            Step::Playing => {
                self.controller.now().track.is_some()
                    && self.controller.display_pos() >= shots::TOUR_MIN_POS
            }
            _ => true,
        };
        if let Some(tour) = &mut self.tour {
            tour.frame(settled);
            if tour.should_request(ready) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(())));
            }
        }
    }
}

/// Keep the frame loop alive from [`Phoebus::logic`] alone, so playback goes on advancing
/// while the window is hidden (UI-SPEC v1.3 §Background liveness).
///
/// ## Why `logic` has to arm its own wake-up
///
/// While the window is hidden or minimized, eframe runs no egui pass at all: it calls
/// `Context::run_logic` (`update_logic_only`), which invokes `logic` and *not* `ui`. That
/// call begins by resetting the viewport's repaint delay to `Duration::MAX` — it consumes
/// any outstanding request precisely so that a fresh one from `logic` reaches the
/// integration — and then hands the result to winit, which sleeps until the next armed wake
/// or a real event. So whatever `logic` does not ask for, nobody asks for.
///
/// The pacing that keeps a playing app ticking used to be armed only at the end of `ui`.
/// Hiding the window therefore ran out the clock: the last armed wake fired one final logic
/// pass, that pass armed nothing, and the loop went to sleep for good. The audio engine
/// played on to the end of the track and posted `Event::Ended` into its channel, where it
/// sat undrained — [`Controller::poll`] never ran again — so the next track did not start
/// until the user brought the window back. That is the reported bug.
///
/// ## What the condition has to cover
///
/// `logic` drains two channels that nothing else drains and that cannot wake the loop by
/// themselves: the audio engine's events ([`Controller::poll`]) and the scan thread's
/// ([`Phoebus::poll_scan`]). Both have to hold the loop open, or they stall — a first scan
/// with the window hidden freezes at whatever count it had reached, which is the same bug
/// wearing different clothes. The other two sources need no help: the OS media keys call
/// `request_repaint` from their own callback thread when a button is pressed, and artwork
/// decoding arms its own faster tick from inside `Artwork::pump`.
///
/// ## Loaded, not playing
///
/// For the engine the condition is "a track is loaded", paused included. A paused app has
/// nothing to advance, so ticking is not needed for *playback*; it is needed because the
/// engine's channel is asynchronous and the play/pause flag is not. `pause` is a message to
/// the player thread, and an `Ended` — or a `Loaded`, or a failure — for the current
/// generation can already be in the channel when the flag goes false. A media-key pause
/// landing on a track that was ending at that moment is exactly that race, and it is a race
/// only a hidden window can lose: arming on `is_playing()` would switch the only thing that
/// drains the channel off in the same pass that queued its last event, with no `ui` left to
/// switch it back on. Arming on "loaded" cannot lose it. That flag clears only when
/// [`Controller::stop`] unloads the track, and `stop` bumps the generation that makes every
/// event still in flight a no-op — so the tick lives exactly as long as there is an event
/// worth draining, and an app with an empty queue still sleeps at the OS's pace.
///
/// The price of the wider condition is four wake-ups a second on a hidden, paused app, each
/// one a handful of drains of empty channels — against a queue that could otherwise strand a
/// track that ended mid-pause. (eframe floors hidden-window repaints at 100 ms, so this
/// 250 ms request is honoured as asked rather than sped up.)
///
/// `ui` keeps its own pacing: egui takes the *minimum* of everything requested during a
/// pass, so the two re-arms compose instead of fighting.
fn arm_background_repaint(ctx: &egui::Context, now: Now, scanning: bool) {
    if now.track.is_some() || scanning {
        ctx.request_repaint_after(Duration::from_millis(theme::REPAINT_MS));
    }
}

impl eframe::App for Phoebus {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.artwork.pump(ctx);
        self.poll_scan();
        self.controller.poll(&self.library);
        // Media keys become ordinary actions — and they must be APPLIED here, not left
        // for `ui`: while the window is hidden or minimized only `logic` runs, and a
        // pause pressed in the background would otherwise sit queued (audibly ignored)
        // until the window next opened. `apply_actions` is a no-op when nothing queued.
        self.media_keys.poll(&self.controller, &mut self.actions);
        self.apply_actions(ctx);
        self.shot_autoplay();
        self.tour_setup();
        self.shot_receive(ctx);

        let size = ctx.input(|i| i.viewport_rect().size());
        self.controller.set_window((size.x, size.y));
        self.controller.tick_save();
        // Catches everything the engine changed above (a track ending and advancing) even
        // on the frames where the window is hidden and `ui` never runs. Changes the *user*
        // made are caught by the second call, at the end of `ui`.
        self.media_keys.sync(&self.controller, &self.library);
        // LAST, so it sees the state this pass produced: a track that just ended and
        // advanced is still loaded and keeps ticking; a queue that ran out stops.
        arm_background_repaint(ctx, self.controller.now(), self.scan.is_some());
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.controller.shortcuts(&ctx, &mut self.actions);

        // FIRST, so it spans the full window width and everything else stacks above it
        // (UI-SPEC §Layout). egui hands each panel what is left of the parent rect, so
        // panel order *is* the layout: bottom bar, then sidebar, then drawer, then content.
        egui::Panel::bottom("player-bar")
            .exact_size(theme::PLAYER_BAR_H)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::p().bg1)
                    .inner_margin(egui::Margin::symmetric(theme::PANEL_PAD as i8, 0)),
            )
            .show(ui, |ui| self.player_bar(ui));

        // `default_size` + `size_range` rather than `exact_size`: the latter pins the range
        // to a single point, which makes egui's own resize drag a no-op (UI-SPEC v1.4
        // §Panel widths). The default is what `state.json` remembers, so a dragged width is
        // already in place on the first frame; from then on egui's `PanelState` carries it
        // and we read the laid-out width back out to persist it.
        //
        // `set_min_width` is what replaces the width floor `exact_size` used to give for
        // free: with a real range, egui only forces the frame out to the panel's *minimum*,
        // so a body that did not fill would shrink the frame — and with it the width read
        // back on the next line and written to `state.json` — a little more every frame.
        let sidebar = egui::Panel::left("sidebar")
            .default_size(self.controller.sidebar_w())
            .size_range(theme::SIDEBAR_W.min..=theme::SIDEBAR_W.max)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(theme::p().bg1)
                    .inner_margin(egui::Margin::symmetric(theme::PANEL_PAD as i8, 0)),
            )
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                self.sidebar(ui);
            });
        self.controller.set_sidebar_w(sidebar.response.rect.width());

        if self.queue_open {
            let drawer = egui::Panel::right("up-next")
                .default_size(self.controller.queue_w())
                .size_range(theme::QUEUE_W.min..=theme::QUEUE_W.max)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(theme::p().bg1)
                        .inner_margin(egui::Margin::symmetric(theme::PANEL_PAD as i8, 0)),
                )
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    self.queue_drawer(ui);
                });
            self.controller.set_queue_w(drawer.response.rect.width());
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::p().bg0))
            .show(ui, |ui| self.content(ui));

        self.apply_actions(&ctx);
        // After the actions, not before: pausing stops the repaint timer, so a state
        // change pushed "next frame" would wait for the next unrelated wake-up. `sync`
        // only talks to the OS when something changed, so calling it twice a frame costs
        // one comparison.
        self.media_keys.sync(&self.controller, &self.library);
        self.shot_request(&ctx);
        self.tour_request(&ctx);

        if self.controller.is_playing() || self.scan.is_some() {
            ctx.request_repaint_after(Duration::from_millis(theme::REPAINT_MS));
        }
        if self.shot.is_some() || self.tour.is_some() {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.controller.save_now();
        // Leave no stale Now Playing card behind, and give the media keys back to whatever
        // the user plays next.
        self.media_keys.shutdown();
    }
}

/// What a sidebar row belongs to, which is what decides how far in it sits.
///
/// UI-SPEC v1.2 §Sidebar sections: rows under a section label are indented relative to it,
/// so `LIBRARY` and `PLAYLISTS` read as headings of a group rather than as more rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    /// One of the app's own words under a section label: indented and letter-spaced.
    Item,
    /// A playlist the user named: indented, but printed verbatim — the letter-spacing is
    /// for Phoebus's vocabulary, not for someone's title.
    Name,
    /// A row that belongs to no section: flush with the panel. `SETTINGS` is the only one
    /// — `RESCAN` moved into Settings itself with the sidebar footer (UI-SPEC v1.4).
    Standalone,
}

impl Row {
    fn indent(self) -> f32 {
        match self {
            Row::Item | Row::Name => theme::SECTION_INDENT,
            Row::Standalone => 0.0,
        }
    }
}

/// A sidebar section heading — `TEXT_LOW`, letter-spaced, one step below `Small`
/// (UI-SPEC v1.2 §Sidebar sections), with the gap its rows want underneath.
///
/// Not `widgets::micro`: that is the size every column header and in-view label uses, and
/// only the sidebar's two section labels shrink.
fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(widgets::spaced(text))
            .font(theme::font_micro())
            .color(theme::p().text_low),
    );
    ui.add_space(theme::CARD_TEXT_GAP * 0.5);
}

/// A sidebar row: `ACCENT` text plus a 2 px `ACCENT` left bar when active, `TEXT_MID`
/// otherwise, `TEXT_HI` on hover.
///
/// The whole width stays clickable however far the label is indented, and the active bar
/// stays welded to the panel edge — it marks the row, not the group. Welded, not flush:
/// [`theme::ACTIVE_BAR_INSET`] leaves a sliver of panel to its left so the mark reads as
/// sitting at the window's edge rather than as running off it.
fn nav_row(ui: &mut Ui, label: &str, active: bool, kind: Row) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), theme::ROW_NAV),
        Sense::click(),
    );
    let color = if active {
        theme::p().accent_text
    } else if response.hovered() {
        theme::p().text_hi
    } else {
        theme::p().text_mid
    };
    if active {
        let bar_x = rect.left() - theme::PANEL_PAD + theme::ACTIVE_BAR_INSET;
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(bar_x, rect.top() + 3.0),
                egui::pos2(bar_x + theme::ACTIVE_BAR_W, rect.bottom() - 3.0),
            ),
            egui::CornerRadius::ZERO,
            theme::p().accent_text,
        );
    }
    let text = if kind == Row::Name {
        label.to_string()
    } else {
        widgets::spaced(label)
    };
    let indent = kind.indent();
    widgets::text_left(
        ui,
        egui::pos2(rect.left() + indent, rect.center().y),
        &text,
        theme::font_small(),
        color,
        rect.width() - indent,
    );
    response
}

/// The sidebar's two-click delete confirmation: `DELETE?  YES  NO`, in place of the
/// playlist row. No modal, no dialog — the row asks and the row answers.
fn confirm_row(ui: &mut Ui, id: u64, actions: &mut Vec<Action>) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), theme::ROW_NAV),
        Sense::hover(),
    );
    // Indented like the playlist row it stands in for, so the list does not jump.
    let indent = Row::Name.indent();
    widgets::text_left(
        ui,
        egui::pos2(rect.left() + indent, rect.center().y),
        &widgets::spaced("DELETE?"),
        theme::font_small(),
        theme::p().text_low,
        rect.width() * 0.6 - indent,
    );
    let mut right = rect.right();
    for (label, action) in [
        ("NO", Action::CancelDelete),
        ("YES", Action::DeletePlaylist(id)),
    ] {
        let text = widgets::spaced(label);
        let galley = widgets::truncated(
            ui,
            &text,
            theme::font_small(),
            theme::p().text_mid,
            rect.width(),
        );
        let hit = Rect::from_min_max(
            egui::pos2(right - galley.size().x - 4.0, rect.top()),
            egui::pos2(right, rect.bottom()),
        );
        let response = ui.interact(hit, ui.id().with(("confirm", id, label)), Sense::click());
        let color = if label == "YES" {
            theme::hover_color(
                response.hovered(),
                theme::p().text_mid,
                theme::p().accent_text,
            )
        } else {
            theme::hover_color(response.hovered(), theme::p().text_low, theme::p().text_hi)
        };
        ui.painter().galley(
            egui::pos2(
                right - galley.size().x,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley.clone(),
            color,
        );
        if response.clicked() {
            actions.push(action);
        }
        right -= galley.size().x + theme::CARD_TEXT_GAP + 2.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The ceiling UI-SPEC v1.3 §Background liveness puts on a loaded app's wake-ups.
    const CEILING: Duration = Duration::from_millis(theme::REPAINT_MS);

    fn track() -> Now {
        Now {
            track: Some(TrackId::for_rel_path("HOME/Odyssey/01 Intro.m4a")),
            playing: true,
        }
    }

    /// What the *visible* path reports: run three egui passes with nothing on screen but
    /// the liveness re-arm, and return the repaint the last one asked for. Three passes
    /// because egui's own first frames (fonts, textures, sizing) request repaints of their
    /// own — measuring those would prove nothing about our request.
    fn repaint_delay(now: Now, scanning: bool) -> Duration {
        let ctx = egui::Context::default();
        let mut delay = Duration::ZERO;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| arm_background_repaint(ui.ctx(), now, scanning));
            delay = out
                .viewport_output
                .values()
                .map(|v| v.repaint_delay)
                .min()
                .unwrap_or(Duration::MAX);
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }
        delay
    }

    /// UI-SPEC v1.3 §Background liveness: after a logic pass with a track loaded, the
    /// viewport must be asking for another frame within 250 ms — and with nothing loaded it
    /// must be asking for nothing at all.
    ///
    /// This is the pacing contract read back off a real `egui::Context` rather than off the
    /// source: it is what decides whether `Controller::poll` runs again in time to see the
    /// engine's `Ended` and start the next track.
    #[test]
    fn a_loaded_track_keeps_the_frame_loop_alive() {
        let playing = repaint_delay(track(), false);
        assert!(
            playing <= CEILING,
            "UI-SPEC v1.3 ceiling, got {playing:?} — a hidden window would stall here"
        );
        // egui subtracts one predicted frame from every `request_repaint_after` so it does
        // not overshoot, so what comes back is `250 ms - 1/60 s` ≈ 233 ms. Much shorter
        // would mean something other than the liveness re-arm is pacing the loop.
        assert!(
            playing >= CEILING.saturating_sub(Duration::from_millis(25)),
            "something other than the liveness re-arm is pacing this: {playing:?}"
        );

        // Paused counts as loaded: the engine's channel is asynchronous and the play/pause
        // flag is not, so an `Ended` can already be queued when the flag goes false.
        let paused = repaint_delay(
            Now {
                playing: false,
                ..track()
            },
            false,
        );
        assert!(
            paused <= CEILING,
            "a paused track is still loaded, got {paused:?}"
        );

        // A scan in flight is the app's other undrained channel: its progress and its
        // finished library reach `poll_scan` only on a pass that happens, so a hidden first
        // run would otherwise freeze mid-count.
        let scanning = repaint_delay(Now::default(), true);
        assert!(
            scanning <= CEILING,
            "a scan in flight must keep the loop alive, got {scanning:?}"
        );

        // Nothing loaded and nothing scanning: nothing in flight to drain, so nothing to
        // wake up for. `stop` bumps the generation, which voids anything still in the
        // channel.
        let idle = repaint_delay(Now::default(), false);
        assert!(
            idle > Duration::from_secs(1),
            "an idle app must not busy-tick, got {idle:?}"
        );
    }

    /// The hidden-window path itself, with no egui pass anywhere in it.
    ///
    /// eframe calls `Context::run_logic` (never `run_ui`) while the window is hidden or
    /// minimized, and that call clears the viewport's repaint delay on entry so that a
    /// fresh request from `logic` reaches the integration. The integration hears about it
    /// only through the repaint callback — the same one eframe's winit runner uses to
    /// schedule the next wake — so that callback is what this test listens to. Before the
    /// fix nothing called it from here and winit slept until the user reopened the window.
    #[test]
    fn a_hidden_window_logic_pass_schedules_its_own_wake() {
        let ctx = egui::Context::default();
        let armed: Arc<Mutex<Vec<Duration>>> = Arc::default();
        let sink = Arc::clone(&armed);
        ctx.set_request_repaint_callback(move |info| {
            if let Ok(mut armed) = sink.lock() {
                armed.push(info.delay);
            }
        });

        let input = egui::RawInput::default();
        // The first logic-only pass consumes the repaint egui arms for every new context;
        // the second is the steady state a hidden app actually lives in.
        for _ in 0..2 {
            armed.lock().expect("no panic held this lock").clear();
            let _ = ctx.run_logic(&input, |ctx| arm_background_repaint(ctx, track(), false));
        }
        let armed = armed.lock().expect("no panic held this lock").clone();
        assert!(
            armed.iter().any(|d| *d <= CEILING),
            "a logic-only pass armed no wake-up ({armed:?}): the loop would sleep forever \
             and `Ended` would never be drained"
        );

        // And the same pass with nothing loaded asks the integration for nothing.
        let quiet: Arc<Mutex<Vec<Duration>>> = Arc::default();
        let sink = Arc::clone(&quiet);
        ctx.set_request_repaint_callback(move |info| {
            if let Ok(mut quiet) = sink.lock() {
                quiet.push(info.delay);
            }
        });
        let _ = ctx.run_logic(&input, |ctx| {
            arm_background_repaint(ctx, Now::default(), false)
        });
        let quiet = quiet.lock().expect("no panic held this lock").clone();
        assert!(
            quiet.is_empty(),
            "idle logic pass asked for frames: {quiet:?}"
        );
    }

    /// The round trip the two resizable panels live on: the persisted width goes in as
    /// `default_size`, and the width that comes back out of the panel's rect — which is
    /// what gets persisted again — has to be the very same number.
    ///
    /// If it were off by the separator line's reserved point, or by the frame's margins,
    /// `set_sidebar_w` would write a slightly smaller width every frame and the sidebar
    /// would visibly creep shut over a session. This is the egui contract that rules that
    /// out, read back off a real pass rather than off the panel source.
    ///
    /// Note the `set_min_width` in the bodies: it is the one thing `ui` has to do that
    /// `exact_size` used to do for it. Drop it here and this test fails at the panel's
    /// *minimum* width, which is precisely the creep it exists to prevent.
    #[test]
    fn a_panel_hands_back_exactly_the_width_it_was_given() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let frame = || {
            egui::Frame::new()
                .fill(theme::p().bg1)
                .inner_margin(egui::Margin::symmetric(theme::PANEL_PAD as i8, 0))
        };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 820.0),
            )),
            ..Default::default()
        };
        // Two passes: the first has no `PanelState` and uses `default_size`, the second
        // reads the size back off the state the first stored. Both must agree.
        for _ in 0..2 {
            let mut out = ctx.run_ui(input.clone(), |ui| {
                let left = egui::Panel::left("sidebar")
                    .default_size(theme::SIDEBAR_W.default)
                    .size_range(theme::SIDEBAR_W.min..=theme::SIDEBAR_W.max)
                    .resizable(true)
                    .frame(frame())
                    .show(ui, |ui| ui.set_min_width(ui.available_width()));
                assert_eq!(left.response.rect.width(), theme::SIDEBAR_W.default);
                let right = egui::Panel::right("up-next")
                    .default_size(theme::QUEUE_W.default)
                    .size_range(theme::QUEUE_W.min..=theme::QUEUE_W.max)
                    .resizable(true)
                    .frame(frame())
                    .show(ui, |ui| ui.set_min_width(ui.available_width()));
                assert_eq!(right.response.rect.width(), theme::QUEUE_W.default);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }
    }

    /// UI-SPEC v1.4 §Panel widths: the sidebar's floor is the width its longest row still
    /// needs — a divider the user can drag must never be able to clip the app's own
    /// vocabulary down to `RECENTLY ADD…`.
    ///
    /// Measured against the real font rather than asserted from the source, because what
    /// makes these labels wide is the hair space `widgets::spaced` puts between every pair
    /// of letters. `SIDEBAR_W.min` has to stay above the widest of them plus the row's
    /// indent and the panel's two paddings.
    #[test]
    fn the_sidebar_floor_fits_its_longest_nav_row() {
        /// Every label the sidebar lays out at `font_small()`, longest first.
        const ROWS: [&str; 7] = [
            "RECENTLY ADDED",
            "+ NEW PLAYLIST",
            "FAVORITES",
            "SETTINGS",
            "ARTISTS",
            "ALBUMS",
            "SONGS",
        ];

        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut widest = (0.0_f32, "");
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(theme::SIDEBAR_W.default, 820.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            for label in ROWS {
                let galley = ui.painter().layout_no_wrap(
                    widgets::spaced(label),
                    theme::font_small(),
                    Color32::WHITE,
                );
                if galley.size().x > widest.0 {
                    widest = (galley.size().x, label);
                }
            }
        });
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        out.textures_delta.clear();

        let (width, label) = widest;
        let needed = 2.0 * theme::PANEL_PAD + theme::SECTION_INDENT + width;
        assert!(
            needed <= theme::SIDEBAR_W.min,
            "{label:?} needs {needed} px (label {width}, indent {}, padding 2×{}), \
             but the sidebar floor is {}",
            theme::SECTION_INDENT,
            theme::PANEL_PAD,
            theme::SIDEBAR_W.min
        );
    }
}
