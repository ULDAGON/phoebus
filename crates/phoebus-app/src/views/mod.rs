//! The content router, the chrome every view shares, and the little bag of per-view UI
//! state that has to survive between frames ([`ViewState`]).
//!
//! Views are pure functions of the library plus [`ViewState`]: they never touch the
//! controller, they push [`crate::nav::Action`]s instead. That is what keeps sorting,
//! selection and inline renames out of the playback code.

pub mod album;
pub mod albums;
pub mod artists;
pub mod favorites;
pub mod playlist;
pub mod recently;
pub mod search;
pub mod settings;
pub mod songs;

use egui::{Align2, Margin, Ui};

use crate::nav::{Ctx, View};
use crate::theme;
use crate::widgets;

/// Everything a view remembers between frames: which artist is selected, how the song
/// table is sorted, which playlist name is being edited.
///
/// It lives in the app (not in egui's temp memory) because the sorted song order is a
/// `Vec` of every track in the library — far too big to clone in and out of `ui.data()`
/// once per frame.
#[derive(Default)]
pub struct ViewState {
    /// Artist requested by `Go to Artist`, consumed by the Artists view.
    pub pending_artist: Option<String>,
    /// Artists view.
    pub artists: artists::State,
    /// Albums grid (the hearted-first order, cached).
    pub albums: albums::State,
    /// Favorites view (resolved rows, cached).
    pub favorites: favorites::State,
    /// Songs table (sort column + cached order).
    pub songs: songs::State,
    /// Playlist detail (inline rename).
    pub playlist: playlist::State,
    /// Search results cache.
    pub search: search::State,
    /// Settings (the library-root input and its inline error).
    pub settings: settings::State,
    /// The playlist the sidebar is asking the user to confirm deleting.
    pub confirm_delete: Option<u64>,
}

impl ViewState {
    /// Throw away everything that was derived from the old library. Called when a scan
    /// finishes, because track ids, sort orders and search hits are all stale by then.
    pub fn library_changed(&mut self) {
        // The artist *selection* is an index into a list that no longer exists; the width
        // the user dragged the split to is not derived from the library at all and has to
        // outlive the rescan, or a `RESCAN` would silently undo it.
        self.artists = artists::State {
            list_w: self.artists.list_w,
            ..artists::State::default()
        };
        self.albums.invalidate();
        self.favorites.invalidate();
        self.songs.invalidate();
        self.search.invalidate();
        self.playlist.invalidate();
    }
}

/// Draw the view that `view` selects.
///
/// `settings` is the one bundle of facts no view can derive from the [`Ctx`]: which library
/// root is in force and where it came from. It travels as a parameter rather than in `Ctx`
/// because exactly one view reads it.
pub fn route(
    ui: &mut Ui,
    cx: &mut Ctx,
    view: &View,
    query: &str,
    st: &mut ViewState,
    settings: &settings::Info,
) {
    match view {
        View::RecentlyAdded => recently::show(ui, cx),
        View::Favorites => favorites::show(ui, cx, &mut st.favorites),
        View::Albums => albums::show(ui, cx, &mut st.albums),
        View::Album(key) => album::show(ui, cx, key),
        View::Artists => artists::show(ui, cx, st),
        View::Songs => songs::show(ui, cx, &mut st.songs),
        View::Playlist(id) => playlist::show(ui, cx, &mut st.playlist, *id),
        View::Search => search::show(ui, cx, &mut st.search, query),
        View::Settings => settings::show(ui, cx, &mut st.settings, settings),
    }
}

/// 24 px padding around a view's content, and [`theme::SCROLL_GAP`] between anything that
/// scrolls inside it and its scrollbar.
///
/// The gap is set here, on the one `Ui` every view is built in, rather than at each
/// `ScrollArea`: a row is allocated across `ui.available_width()`, which egui has already
/// shortened by what the bar reserves, so one number insets the right end of every row in
/// every list — including the hover/selection highlight, which is the row rect — and a view
/// that grows a new list inherits it without knowing about any of this.
pub fn page(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .inner_margin(Margin::same(theme::VIEW_PAD as i8))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().scroll.bar_inner_margin = theme::SCROLL_GAP;
            add(ui);
        });
}

/// A view title: Heading, `TEXT_HI`.
pub fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::font_heading())
            .color(theme::p().text_hi),
    );
    ui.add_space(theme::CARD_TEXT_GAP * 2.0);
}

/// A `TEXT_LOW` metadata line under a heading (`3 ALBUMS · 38 SONGS`).
pub fn subheading(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::font_small())
            .color(theme::p().text_low),
    );
}

/// A section micro-label with a hairline under it — the Search view's `ARTISTS` /
/// `ALBUMS` / `SONGS` dividers.
pub fn section(ui: &mut Ui, label: &str) {
    widgets::micro(ui, label);
    ui.add_space(4.0);
    let rect = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.right(), rect.top()),
        ],
        theme::hairline(),
    );
    ui.add_space(theme::CARD_TEXT_GAP);
}

/// A centred `TEXT_LOW` message filling the whole content area.
///
/// The first line is Body, the rest Small. Lines that are pure micro-labels (no lowercase)
/// get the letter-spaced treatment; anything carrying lowercase — a path, a name — is left
/// alone and truncated to the available width.
pub fn centered_note(ui: &mut Ui, lines: &[&str]) {
    let rect = ui.available_rect_before_wrap();
    let line_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let total = line_h * lines.len() as f32;
    let max_width = (rect.width() - 2.0 * theme::VIEW_PAD).max(1.0);
    let mut y = rect.center().y - total * 0.5 + line_h * 0.5;
    for (i, line) in lines.iter().enumerate() {
        let font = if i == 0 {
            theme::font_body()
        } else {
            theme::font_small()
        };
        let text = if line.chars().any(char::is_lowercase) {
            (*line).to_string()
        } else {
            widgets::spaced(line)
        };
        let galley = widgets::truncated(ui, &text, font, theme::p().text_low, max_width);
        let pos = Align2::CENTER_CENTER.anchor_size(egui::pos2(rect.center().x, y), galley.size());
        ui.painter().galley(pos.min, galley, theme::p().text_low);
        y += line_h;
    }
    ui.allocate_space(rect.size());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::Artwork;
    use crate::nav::{Action, Fmt, Now};
    use crate::theme;
    use phoebus_core::{AlbumKey, Library, Playlist, Track, UpNext};
    use std::time::Duration;

    fn make(rel: &str, title: &str, artist: &str, album: &str, secs: u64) -> Track {
        let mut t = Track::new(rel);
        t.title = title.to_string();
        t.artist = artist.to_string();
        t.album_artist = artist.to_string();
        t.album = album.to_string();
        t.duration = Duration::from_secs(secs);
        t.refresh_key();
        t
    }

    fn library() -> Library {
        // Track 3 is the shape that used to misdirect `Go to Artist`: a guest credit whose
        // album artist — the only thing `Library::artists()` groups by — is someone else.
        let mut guest = make(
            "HOME/Odyssey/13 Resonance.m4a",
            "Resonance (Remix)",
            "HAZH",
            "Odyssey",
            201,
        );
        guest.album_artist = "HOME".to_string();
        guest.refresh_key();
        Library::build(
            "/lib",
            vec![
                make("HOME/Odyssey/01 Intro.m4a", "Intro", "HOME", "Odyssey", 189),
                make(
                    "HOME/Odyssey/02 Native.m4a",
                    "Native",
                    "HOME",
                    "Odyssey",
                    242,
                ),
                guest,
                make(
                    "Woodkid/S16/01 Goliath.m4a",
                    "Goliath",
                    "Woodkid",
                    "S16",
                    230,
                ),
            ],
        )
    }

    fn playlists() -> Vec<Playlist> {
        vec![
            Playlist {
                id: 1,
                name: "Late Night".into(),
                entries: vec![
                    "HOME/Odyssey/01 Intro.m4a".into(),
                    "Woodkid/S16/01 Goliath.m4a".into(),
                    "HOME/Odyssey/01 Intro.m4a".into(),
                ],
                created_at: 0,
                modified_at: 1,
            },
            // The state `+ NEW PLAYLIST` drops the user into.
            Playlist {
                id: 2,
                name: "Playlist 2".into(),
                entries: Vec::new(),
                created_at: 0,
                modified_at: 1,
            },
        ]
    }

    /// Lay a view out twice headlessly (egui's tables need a sizing pass) and return the
    /// actions it raised. Panics in any view — a mis-counted table column, a bad borrow of
    /// `Ctx` — surface here rather than in front of the user.
    fn render(view: &View, query: &str, st: &mut ViewState, lib: &Library, pls: &[Playlist]) {
        render_with(
            view,
            query,
            st,
            lib,
            pls,
            &plain_info(),
            &crate::nav::test_favorites(),
        );
    }

    fn plain_info() -> settings::Info<'static> {
        settings::Info {
            active_root: std::path::Path::new("/lib"),
            default_root: std::path::Path::new("/home/nobody/.phoebus"),
            env_override: None,
            configured: None,
        }
    }

    fn render_with(
        view: &View,
        query: &str,
        st: &mut ViewState,
        lib: &Library,
        pls: &[Playlist],
        info: &settings::Info,
        favs: &phoebus_core::Favorites,
    ) {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut art = Artwork::new();
        let fmt = Fmt::build(lib);
        let mut actions: Vec<Action> = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1280.0, 820.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            let mut out = ctx.run_ui(input.clone(), |ui| {
                let mut cx = Ctx {
                    lib,
                    art: &mut art,
                    playlists: pls,
                    favs,
                    now: Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                route(ui, &mut cx, view, query, st, info);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }
    }

    #[test]
    fn every_view_lays_itself_out() {
        let lib = library();
        let pls = playlists();
        let mut st = ViewState::default();
        for (view, query) in [
            (View::RecentlyAdded, ""),
            // Empty, so the `NO FAVORITES YET — …` state is laid out here; the populated
            // one has a test of its own below.
            (View::Favorites, ""),
            (View::Albums, ""),
            (View::Album(AlbumKey::new("HOME", "Odyssey")), ""),
            (View::Artists, ""),
            (View::Songs, ""),
            (View::Playlist(1), ""),
            // A brand-new, empty playlist: header, disabled buttons, `EMPTY — …` note.
            (View::Playlist(2), ""),
            // "o" hits every section, so the inline album grid inside the search scroller
            // is laid out too.
            (View::Search, "o"),
            (View::Settings, ""),
        ] {
            render(&view, query, &mut st, &lib, &pls);
        }
        let hits = phoebus_core::search(&lib, "o");
        assert!(
            !hits.artists.is_empty() && !hits.albums.is_empty() && !hits.tracks.is_empty(),
            "the smoke test must exercise all three search sections"
        );
    }

    #[test]
    fn views_survive_an_empty_library_and_missing_targets() {
        let lib = Library::empty("/lib");
        let mut st = ViewState::default();
        for (view, query) in [
            (View::Artists, ""),
            (View::Favorites, ""),
            (View::Songs, ""),
            (View::Album(AlbumKey::new("Nobody", "Nothing")), ""),
            (View::Playlist(404), ""),
            (View::Search, "zzz"),
            (View::Settings, ""),
        ] {
            render(&view, query, &mut st, &lib, &[]);
        }
    }

    /// Settings has three states the tour cannot photograph: the input pre-filled from a
    /// configured root, the inline `NOT A DIRECTORY` error, and the disabled/locked form
    /// when `$PHOEBUS_LIBRARY` is in force. Each one changes the layout, so each one is laid
    /// out here.
    #[test]
    fn the_settings_view_renders_every_library_state() {
        let lib = library();
        let favs = crate::nav::test_favorites();
        let mut st = ViewState::default();

        let configured = settings::Info {
            configured: Some("~/Music/Media"),
            ..plain_info()
        };
        render_with(&View::Settings, "", &mut st, &lib, &[], &configured, &favs);
        assert_eq!(
            st.settings.path.as_deref(),
            Some("~/Music/Media"),
            "the input pre-fills with the user's own spelling"
        );

        st.settings.not_a_directory = true;
        render_with(&View::Settings, "", &mut st, &lib, &[], &configured, &favs);
        assert!(
            st.settings.not_a_directory,
            "the error stays until it is fixed"
        );

        st.settings.reset_input();
        assert!(st.settings.path.is_none() && !st.settings.not_a_directory);

        let locked = settings::Info {
            env_override: Some("/elsewhere/Music"),
            ..configured
        };
        render_with(&View::Settings, "", &mut st, &lib, &[], &locked, &favs);
        assert_eq!(
            st.settings.path.as_deref(),
            Some("/lib"),
            "a locked input shows what is actually being scanned"
        );
    }

    /// UI-SPEC §Favorites, on the real widget tree: the view lays out populated as well as
    /// empty, its `PLAY` hands the queue the favourites list *in `tracks_sorted` order*, and
    /// the Albums view puts the hearted album in a `FAVORITES` section without taking it out
    /// of the grid.
    ///
    /// Both halves are here because they are the two places a favourite changes what is on
    /// screen, and both go through the same `ViewState` caches — which is the part a
    /// per-view unit test cannot see. Laying `View::Albums` out at all is half the point of
    /// the second half: the sectioned layout draws the hearted album twice, and two cards for
    /// one album is exactly the shape an egui id clash takes.
    #[test]
    fn the_favorites_view_and_the_sectioned_albums_grid_render() {
        let lib = library();
        let mut favs = crate::nav::test_favorites();
        // Hearted out of order, and the second album rather than the first: the view has to
        // re-sort, and a section that merely echoed the click would show the wrong album.
        let ids = lib.tracks_sorted();
        favs.toggle_track(&lib, ids[3]);
        favs.toggle_track(&lib, ids[0]);
        let second = lib.albums()[1].clone();
        favs.toggle_album(&second);

        let mut st = ViewState::default();
        for view in [View::Favorites, View::Albums] {
            render_with(&view, "", &mut st, &lib, &[], &plain_info(), &favs);
        }

        assert_eq!(
            favs.track_ids(&lib),
            vec![ids[0], ids[3]],
            "the rows and the PLAY context are in library order, not click order"
        );
        assert_eq!(
            st.albums.section(),
            std::slice::from_ref(&second),
            "the FAVORITES section is not the one hearted album"
        );
        assert!(
            lib.albums().len() > 1 && lib.albums().contains(&second),
            "…and ALL ALBUMS below it is still the whole library, that album included"
        );

        // And once the favourites are gone the view falls back to its empty state — the
        // caches must not keep showing rows that are no longer hearted.
        favs.toggle_track(&lib, ids[0]);
        favs.toggle_track(&lib, ids[3]);
        st.favorites.invalidate();
        render_with(
            &View::Favorites,
            "",
            &mut st,
            &lib,
            &[],
            &plain_info(),
            &favs,
        );
        assert!(favs.track_ids(&lib).is_empty());
    }

    #[test]
    fn the_playlist_view_renders_its_inline_rename() {
        let lib = library();
        let pls = playlists();
        let mut st = ViewState::default();
        st.playlist.start_rename(1, "Late Night");
        render(&View::Playlist(1), "", &mut st, &lib, &pls);
        assert!(st.playlist.rename.is_some(), "still editing");
        assert!(st.playlist.cancel_rename());
        assert!(!st.playlist.cancel_rename());
        render(&View::Playlist(1), "", &mut st, &lib, &pls);
    }

    /// UI-SPEC v1.4 §Add songs, on the real widget tree: the picker lays itself out over a
    /// playlist page (populated *and* empty, which is where it matters most), and the
    /// invalidations the app fires around it leave it standing.
    ///
    /// The `+` clicks themselves are driven in `widgets::song_picker`'s own tests; what is
    /// checked here is the half only the page can break — that opening a modal from inside
    /// `views::page` does not disturb the page, and that
    /// `Phoebus::playlists_changed` → `State::invalidate`, which runs after *every* add,
    /// does not close the popup it was triggered from.
    #[test]
    fn the_playlist_view_lays_out_the_add_songs_picker() {
        let lib = library();
        let pls = playlists();
        let mut st = ViewState::default();
        st.playlist.picker.open();

        for playlist in [1, 2] {
            render(&View::Playlist(playlist), "", &mut st, &lib, &pls);
            assert!(
                st.playlist.picker.is_open(),
                "playlist {playlist}: drawing the page must not dismiss the popup"
            );
        }

        // What every `+` triggers, one round-trip later.
        st.playlist.invalidate();
        assert!(st.playlist.picker.is_open(), "an add must not close it");
        render(&View::Playlist(1), "", &mut st, &lib, &pls);

        // A rescan drops the caches too, and still leaves the modal alone.
        st.library_changed();
        assert!(st.playlist.picker.is_open());
        render(&View::Playlist(1), "", &mut st, &lib, &pls);

        assert!(st.playlist.close_picker(), "…and `Esc` takes it down");
        assert!(!st.playlist.close_picker(), "…exactly once");
        render(&View::Playlist(1), "", &mut st, &lib, &pls);
    }

    #[test]
    fn the_artists_view_consumes_a_pending_artist() {
        let lib = library();
        let mut st = ViewState {
            pending_artist: Some("Woodkid".to_string()),
            ..Default::default()
        };
        render(&View::Artists, "", &mut st, &lib, &[]);
        assert!(st.pending_artist.is_none(), "consumed exactly once");
        assert_eq!(st.artists.selected, Some(1), "Woodkid is the second artist");

        // Casing is not a reason to fail: the index is keyed case-insensitively.
        st.pending_artist = Some("  woodKID ".to_string());
        render(&View::Artists, "", &mut st, &lib, &[]);
        assert_eq!(st.artists.selected, Some(1));

        // A name with no page must clear the selection rather than leave the user on
        // whatever was selected before — which on a fresh start is artist [0].
        st.pending_artist = Some("Nobody".to_string());
        render(&View::Artists, "", &mut st, &lib, &[]);
        assert_eq!(st.artists.selected, None, "no page, no selection");

        assert_eq!(
            artists::State::default().selected,
            Some(0),
            "arriving from the sidebar still opens on the first artist"
        );
    }

    /// UI-SPEC v1.4 §Panel widths, the hand-rolled one.
    ///
    /// The sidebar and Up Next dividers are `egui::Panel`'s own resize handle and egui
    /// tests that; the Artists split is ours, so the whole gesture is driven here — press
    /// on the divider, drag it right, release — and both halves of the contract are read
    /// back: the view's own width moved, and the width went out as an `Action` for the app
    /// to persist. Widths are absolute points measured from the left edge of the page's
    /// content, so on a 1280 px window with 24 px page padding the divider at rest sits at
    /// `24 + 260 + 12`.
    #[test]
    fn dragging_the_artists_split_moves_it_and_sends_the_new_width_out() {
        let lib = library();
        let mut st = ViewState::default();
        assert_eq!(st.artists.list_w, theme::ARTIST_LIST_W.default);

        let rest_x = theme::VIEW_PAD + theme::ARTIST_LIST_W.default + theme::VIEW_PAD * 0.5;
        let actions = drag_x(&View::Artists, &mut st, &lib, rest_x, rest_x + 84.0);
        assert_eq!(
            st.artists.list_w,
            theme::ARTIST_LIST_W.default + 84.0,
            "the list follows the pointer point for point"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::SetArtistListW(w) if *w == st.artists.list_w)),
            "the dragged width has to reach the app to be persisted: {actions:?}"
        );

        // The floor holds: dragging past it stops at the floor, not at the pointer, and
        // never at zero.
        let from = theme::VIEW_PAD + st.artists.list_w + theme::VIEW_PAD * 0.5;
        drag_x(&View::Artists, &mut st, &lib, from, theme::VIEW_PAD - 200.0);
        assert_eq!(st.artists.list_w, theme::ARTIST_LIST_W.min);

        // A rescan throws the selection away and keeps the width.
        st.library_changed();
        assert_eq!(st.artists.list_w, theme::ARTIST_LIST_W.min);
        assert_eq!(st.artists.selected, Some(0));
    }

    /// Press at `from_x`, drag to `to_x`, release — five headless passes, because the
    /// handle is registered on one pass and read on the next (exactly like
    /// `egui::Panel`'s), so the move is delivered twice. Returns everything the view
    /// raised.
    fn drag_x(
        view: &View,
        st: &mut ViewState,
        lib: &Library,
        from_x: f32,
        to_x: f32,
    ) -> Vec<Action> {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut art = Artwork::new();
        let fmt = Fmt::build(lib);
        let mut actions: Vec<Action> = Vec::new();
        let favs = crate::nav::test_favorites();
        let info = plain_info();
        let y = 400.0;
        let press = |x: f32, pressed: bool| egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let moved = |x: f32| egui::Event::PointerMoved(egui::pos2(x, y));
        for events in [
            vec![moved(from_x)],
            vec![press(from_x, true)],
            vec![moved(to_x)],
            vec![moved(to_x)],
            vec![press(to_x, false)],
        ] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now: Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                route(ui, &mut cx, view, "", st, &info);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }
        actions
    }

    /// Hover the Artists divider at `x` and report the cursor the frame asked for. Two
    /// passes for the same reason [`drag_x`] needs five: the handle is registered on one
    /// pass and only answers `hovered()` from the next.
    fn hover_cursor(st: &mut ViewState, lib: &Library, x: f32) -> egui::CursorIcon {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut art = Artwork::new();
        let fmt = Fmt::build(lib);
        let mut actions: Vec<Action> = Vec::new();
        let favs = crate::nav::test_favorites();
        let info = plain_info();
        let mut cursor = egui::CursorIcon::Default;
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                events: vec![egui::Event::PointerMoved(egui::pos2(x, 400.0))],
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now: Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                route(ui, &mut cx, &View::Artists, "", st, &info);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
            cursor = out.platform_output.cursor_icon;
        }
        cursor
    }

    /// UI-SPEC v1.4 §Panel widths: the hand-rolled divider copies `egui::Panel`'s handle,
    /// and egui's stops promising both directions once the panel is pinned — at the floor
    /// it can only grow, at the ceiling only shrink (`Panel::cursor_icon`). The list is the
    /// left half, so growing it points east and shrinking it points west.
    #[test]
    fn the_artist_divider_points_at_the_only_way_left_to_drag() {
        let lib = library();
        let mut st = ViewState::default();
        // Divider x for a list `w` wide, the same arithmetic the drag test uses.
        let at = |w: f32| theme::VIEW_PAD + w + theme::VIEW_PAD * 0.5;

        assert_eq!(
            hover_cursor(&mut st, &lib, at(theme::ARTIST_LIST_W.default)),
            egui::CursorIcon::ResizeHorizontal,
            "free to go either way in the middle of the range"
        );

        st.artists.list_w = theme::ARTIST_LIST_W.min;
        assert_eq!(
            hover_cursor(&mut st, &lib, at(theme::ARTIST_LIST_W.min)),
            egui::CursorIcon::ResizeEast,
            "at the floor there is nowhere to go but wider"
        );

        // 1280 px window less the page's two VIEW_PADs is the content this view splits.
        let ceiling = artists::Split::ceiling(1280.0 - 2.0 * theme::VIEW_PAD);
        st.artists.list_w = ceiling;
        assert_eq!(
            hover_cursor(&mut st, &lib, at(ceiling)),
            egui::CursorIcon::ResizeWest,
            "at the ceiling there is nowhere to go but narrower"
        );
    }

    /// The bug this pins: the ARTIST cell of the Songs table (and every `Go to Artist`)
    /// used to hand over the *track* artist, which is resolved against an index built from
    /// *album* artists — so clicking `HAZH` landed on `HOME` with no hint anything went
    /// wrong. Every raise site goes through `nav::artist_target` now.
    #[test]
    fn go_to_artist_targets_a_page_that_exists() {
        let lib = library();
        let guest = lib
            .tracks_sorted()
            .iter()
            .copied()
            .find(|id| lib.track(*id).is_some_and(|t| t.artist == "HAZH"))
            .expect("the guest-credit track");

        let target = crate::nav::artist_target(&lib, guest).expect("a target");
        assert_eq!(target, "HOME", "the album artist is the page that exists");

        let mut st = ViewState {
            pending_artist: Some(target),
            ..Default::default()
        };
        render(&View::Artists, "", &mut st, &lib, &[]);
        assert_eq!(st.artists.selected, Some(0), "HOME is the first artist");

        // The raw tag is what used to be sent; it resolves to nothing at all.
        assert_eq!(
            artists::State::default().selected,
            Some(0),
            "…which is precisely why it must not be sent: it would look like a hit"
        );
        st.pending_artist = Some("HAZH".to_string());
        render(&View::Artists, "", &mut st, &lib, &[]);
        assert_eq!(st.artists.selected, None);
    }

    #[test]
    fn the_queue_drawer_renders_rows_and_its_empty_state() {
        let lib = library();
        let fmt = Fmt::build(&lib);
        let ids: Vec<_> = lib.tracks_sorted().to_vec();
        let items = vec![
            UpNext {
                id: ids[0],
                manual: true,
            },
            UpNext {
                id: ids[1],
                manual: false,
            },
        ];
        let favs = crate::nav::test_favorites();
        for rows in [items.as_slice(), &[]] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut art = Artwork::new();
            let mut actions: Vec<Action> = Vec::new();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(theme::QUEUE_W.default, 820.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib: &lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now: Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                crate::widgets::queue::drawer(ui, &mut cx, rows);
            });
            out.textures_delta.clear();
        }
    }
}
