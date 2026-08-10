//! Favorites: every hearted song in one list (UI-SPEC v1.3 §Favorites).
//!
//! The view owns nothing. The favourites themselves live in `phoebus_core::Favorites`, the
//! app's copy of `favorites.json`; all this page adds is a [`State`] cache, for the same
//! reason the Songs table has one: `Favorites::track_ids` walks the whole library to put
//! the hearted tracks back into `tracks_sorted` order, and that is an O(tracks) pass with a
//! hash probe each — fine once per toggle, absurd once per frame at 60 fps.

use egui::Ui;
use phoebus_core::TrackId;

use crate::nav::{self, Action, Ctx};
use crate::theme;
use crate::views;
use crate::widgets::{self, menus, song_row};

/// UI-SPEC v1.3's empty state, verbatim.
const EMPTY: &str = "NO FAVORITES YET — HOVER A SONG AND CLICK THE HEART";

/// The resolved rows, plus the total the header shows — recomputed only when the favourites
/// or the library actually move.
pub struct State {
    rows: Vec<TrackId>,
    total: std::time::Duration,
    dirty: bool,
    selected: Option<TrackId>,
}

impl Default for State {
    fn default() -> State {
        State {
            rows: Vec::new(),
            total: std::time::Duration::ZERO,
            // Nothing has been resolved yet, and "no favourites" and "not looked yet" are
            // the same empty `Vec` — so a fresh state has to start dirty or the view opens
            // on a permanent empty state.
            dirty: true,
            selected: None,
        }
    }
}

impl State {
    /// Drop the cache. Called on every favourite toggle and after every scan — the first
    /// changes the membership, the second changes what the ids mean.
    pub fn invalidate(&mut self) {
        self.dirty = true;
        self.rows = Vec::new();
        self.total = std::time::Duration::ZERO;
        self.selected = None;
    }

    /// True while the next [`show`] would re-resolve. Read by the cache test.
    #[cfg(test)]
    fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State) {
    // Copied out of `cx` first so the row slice does not borrow `cx` itself.
    let (lib, favs) = (cx.lib, cx.favs);
    let State {
        rows,
        total,
        dirty,
        selected,
    } = st;
    if *dirty {
        *rows = favs.track_ids(lib);
        *total = rows
            .iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.duration)
            .sum();
        *dirty = false;
    }
    let (rows, total) = (&*rows, *total);
    let current = *selected;
    let mut selection: Option<TrackId> = None;

    views::page(ui, |ui| {
        views::heading(ui, "FAVORITES");
        views::subheading(
            ui,
            &format!("{} SONGS{}{}", rows.len(), theme::SEP, nav::minutes(total)),
        );
        ui.add_space(theme::VIEW_PAD);
        if rows.is_empty() {
            views::centered_note(ui, &[EMPTY]);
            return;
        }
        // The same pair as an album header, with the same split: `PLAY` clears shuffle and
        // plays linearly, `SHUFFLE` rolls a fresh uniform order every press (UI-SPEC v1.2
        // §Shuffle correctness).
        ui.horizontal(|ui| {
            if widgets::primary_button(ui, theme::GLYPH_PLAY, "PLAY").clicked() {
                cx.act(Action::PlayCollection(rows.to_vec()));
            }
            if widgets::secondary_button(ui, theme::GLYPH_SHUFFLE, "SHUFFLE").clicked() {
                cx.act(Action::Play {
                    tracks: rows.to_vec(),
                    index: 0,
                    shuffle: true,
                });
            }
        });
        ui.add_space(theme::VIEW_PAD);

        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .id_salt("favorites-rows")
            .auto_shrink([false, false])
            .show_rows(ui, widgets::ROW_H, rows.len(), |ui, range| {
                for index in range {
                    let Some(&id) = rows.get(index) else {
                        break;
                    };
                    let row = song_row::show(ui, cx, id, current == Some(id));
                    if row.response.clicked() {
                        selection = Some(id);
                    }
                    if row.response.double_clicked() || row.lead == song_row::Lead::PlayRow {
                        cx.act(Action::Play {
                            tracks: rows.to_vec(),
                            index,
                            shuffle: false,
                        });
                    }
                    if row.lead == song_row::Lead::TogglePlay {
                        cx.act(Action::TogglePlay);
                    }
                    row.response.context_menu(|ui| {
                        menus::track_menu(ui, cx, &[id], Some(menus::Nav::both(id)));
                    });
                    // The `⋯` button opens the same menu on a LEFT click.
                    egui::Popup::menu(&row.more).show(|ui| {
                        menus::track_menu(ui, cx, &[id], Some(menus::Nav::both(id)));
                    });
                }
            });
    });
    if let Some(id) = selection {
        *selected = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoebus_core::{Favorites, Library, Track};

    fn lib() -> Library {
        let mut tracks = Vec::new();
        for (i, rel) in [
            "HOME/Odyssey/01 Intro.m4a",
            "HOME/Odyssey/02 Resonance.m4a",
            "Woodkid/S16/01 Goliath.m4a",
        ]
        .iter()
        .enumerate()
        {
            let mut t = Track::new(rel);
            t.duration = std::time::Duration::from_secs(60 * (i as u64 + 1));
            t.refresh_key();
            tracks.push(t);
        }
        Library::build("/lib", tracks)
    }

    fn favorites(lib: &Library, hearted: &[usize]) -> Favorites {
        let mut favs = Favorites::load_from(std::path::Path::new(
            "/nonexistent/phoebus-favorites-test/favorites.json",
        ));
        favs.set_ephemeral(true);
        for i in hearted {
            favs.toggle_track(lib, lib.tracks_sorted()[*i]);
        }
        favs
    }

    /// The rows are resolved once per change, never per frame — the whole reason this view
    /// has a [`State`] at all. `Favorites::track_ids` walks `tracks_sorted`, so a library
    /// of 3 400 songs would otherwise cost 3 400 hash probes every 16 ms.
    #[test]
    fn rows_are_cached_until_a_toggle_or_a_rescan() {
        let l = lib();
        let favs = favorites(&l, &[0, 2]);
        let mut st = State::default();
        assert!(st.is_dirty(), "a fresh state has resolved nothing yet");

        // One resolve, done by hand exactly as `show` does it.
        st.rows = favs.track_ids(&l);
        st.total = st
            .rows
            .iter()
            .filter_map(|id| l.track(*id))
            .map(|t| t.duration)
            .sum();
        st.dirty = false;
        assert_eq!(st.rows.len(), 2);
        assert_eq!(st.total, std::time::Duration::from_secs(60 + 180));
        assert!(!st.is_dirty(), "…and it stays resolved");

        st.invalidate();
        assert!(st.is_dirty(), "a toggle or a rescan re-resolves");
        assert!(st.rows.is_empty(), "…and drops the ids it was holding");
    }

    /// The rows come out in `tracks_sorted` order, which is what makes the view's list the
    /// same order as the Songs table and a usable `PLAY` context.
    #[test]
    fn rows_follow_the_library_order_not_the_click_order() {
        let l = lib();
        // Hearted last-first: Goliath (index 2) before Intro (index 0).
        let favs = favorites(&l, &[2, 0]);
        let rows = favs.track_ids(&l);
        assert_eq!(
            rows,
            vec![l.tracks_sorted()[0], l.tracks_sorted()[2]],
            "the view orders by album_artist → album → disc → track, not by when it was \
             hearted"
        );
    }
}
