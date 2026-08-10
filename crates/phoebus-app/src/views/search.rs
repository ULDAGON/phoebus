//! Search: `ARTISTS`, `ALBUMS` and `SONGS`, each section shown only when it has hits.
//!
//! The query changes as the user types, so the results are cached and only recomputed when
//! the query (or the library) actually changes — a search across a big library must not
//! run once per frame at 60 fps.

use egui::{Sense, Ui};
use phoebus_core::{SearchResults, TrackId};

use crate::nav::{Action, Ctx, View};
use crate::theme;
use crate::views;
use crate::widgets::{self, album_card, menus, song_row};

/// Cached results for the last query.
#[derive(Default)]
pub struct State {
    query: String,
    title: String,
    results: SearchResults,
    fresh: bool,
    selected: Option<TrackId>,
}

impl State {
    /// Drop the cache (a rescan invalidates every hit in it).
    pub fn invalidate(&mut self) {
        self.fresh = false;
    }

    /// Recompute the hits if the query or the library moved under us.
    fn refresh(&mut self, lib: &phoebus_core::Library, query: &str) {
        if self.fresh && self.query == query {
            return;
        }
        self.query = query.to_string();
        self.title = format!("SEARCH “{}”", query.trim());
        self.results = phoebus_core::search(lib, query);
        self.fresh = true;
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State, query: &str) {
    let lib = cx.lib;
    st.refresh(lib, query);
    let State {
        title,
        results,
        selected,
        ..
    } = st;
    let current = *selected;
    let mut selection: Option<TrackId> = None;

    views::page(ui, |ui| {
        views::heading(ui, title);
        if results.is_empty() {
            views::centered_note(ui, &["NO RESULTS"]);
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("search-results")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !results.artists.is_empty() {
                    views::section(ui, "ARTISTS");
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for name in &results.artists {
                        artist_row(ui, cx, name);
                    }
                    ui.add_space(theme::SECTION_GAP);
                }
                if !results.albums.is_empty() {
                    views::section(ui, "ALBUMS");
                    album_card::grid_inline(ui, cx, &results.albums);
                    ui.add_space(theme::SECTION_GAP);
                }
                if !results.tracks.is_empty() {
                    views::section(ui, "SONGS");
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (index, id) in results.tracks.iter().enumerate() {
                        let row = song_row::show(ui, cx, *id, current == Some(*id));
                        if row.response.clicked() {
                            selection = Some(*id);
                        }
                        if row.response.double_clicked() || row.lead == song_row::Lead::PlayRow {
                            cx.act(Action::Play {
                                tracks: results.tracks.clone(),
                                index,
                                shuffle: false,
                            });
                        }
                        if row.lead == song_row::Lead::TogglePlay {
                            cx.act(Action::TogglePlay);
                        }
                        row.response.context_menu(|ui| {
                            menus::track_menu(ui, cx, &[*id], Some(menus::Nav::both(*id)));
                        });
                        // The `⋯` button opens the same menu on a LEFT click.
                        egui::Popup::menu(&row.more).show(|ui| {
                            menus::track_menu(ui, cx, &[*id], Some(menus::Nav::both(*id)));
                        });
                    }
                }
            });
    });
    if let Some(id) = selection {
        *selected = Some(id);
    }
}

/// One `ARTISTS` row: name, album and song counts, click to open the Artists view on it.
fn artist_row(ui: &mut Ui, cx: &mut Ctx, name: &str) {
    let lib = cx.lib;
    let (rect, response) = widgets::row(ui, theme::ROW_NAV + 6.0, Sense::click());
    widgets::row_background(ui, rect, response.hovered(), false);
    widgets::hairline_bottom(ui, rect);
    let color = theme::hover_color(
        response.hovered(),
        theme::p().text_hi,
        theme::p().accent_text,
    );
    widgets::text_left(
        ui,
        egui::pos2(rect.left(), rect.center().y),
        name,
        theme::font_body(),
        color,
        rect.width() * 0.6,
    );
    if let Some(artist) = lib.artist(name) {
        ui.painter().text(
            egui::pos2(rect.right(), rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!(
                "{} ALBUMS{}{} SONGS",
                artist.album_keys.len(),
                theme::SEP,
                artist.track_count
            ),
            theme::font_small(),
            theme::p().text_low,
        );
    }
    if response.clicked() {
        cx.act(Action::GoArtist(name.to_string()));
        cx.act(Action::Go(View::Artists));
    }
}
