//! Albums: every album in the library, sorted by artist then title — under a `FAVORITES`
//! section holding the hearted ones, whenever there are any (UI-SPEC §Favorites).
//!
//! **A hearted album is on screen twice, and that is the point.** The section is a
//! shortcut, not a move: the grid below it stays the one complete A→Z wall the user has
//! learned where things are in, so hearting an album never makes it disappear from the place
//! they reach for it. Lifting the album out of the grid — pinning — is what this replaces.
//!
//! **Only this view sections.** Recently Added is ordered by `added_at` and that order *is*
//! the point of the page; the Artists page and the Search grid show a subset of one artist /
//! one query, where a favourites block would only repeat rows the user is already reading
//! top to bottom.

use std::ops::Range;

use egui::{Rect, Ui, UiBuilder, Vec2};
use phoebus_core::{AlbumKey, Favorites, Library};

use crate::nav::Ctx;
use crate::theme;
use crate::views;
use crate::widgets::album_card;

/// Scroller salt. Both layouts below use it, so hearting the first album — which swaps one
/// layout for the other — leaves the user where they were scrolled to.
const GRID_ID: &str = "albums-grid";

/// Id salts for the two grids of the sectioned layout.
///
/// A hearted album is laid out twice and each copy owns a hover state, a play badge, a heart
/// and a context menu — all of which egui derives from the card's auto id. Separate salts are
/// what keep the two copies from being the same widget.
const FAVORITES_ID: &str = "albums-favorites";
const ALL_ID: &str = "albums-all";

/// The hearted albums, cached.
///
/// Filtering the library allocates a `Vec<AlbumKey>` and clones a two-`String` key into every
/// hearted slot — for an answer that only changes when a heart is clicked or a scan lands. So
/// it is computed on change and cached here, exactly like `songs::State`'s sort order, and for
/// exactly the same reason.
#[derive(Default)]
pub struct State {
    hearted: Vec<AlbumKey>,
    fresh: bool,
}

impl State {
    /// Drop the cached section. Called by [`Action::ToggleFavAlbum`](crate::nav::Action) and
    /// by every rescan — one changes which albums are hearted, the other changes which albums
    /// there are.
    pub fn invalidate(&mut self) {
        self.hearted = Vec::new();
        self.fresh = false;
    }

    /// The hearted albums, in [`Library::albums`] order — the section reads like the grid.
    ///
    /// With nothing hearted this owns nothing at all and the caller draws the plain grid. An
    /// empty slice is also what a library holding none of the hearted albums yields
    /// (favourites outlive the library they were hearted in), which is why the caller tests
    /// the slice and not [`Favorites::album_count`].
    fn hearted<'a>(&'a mut self, lib: &Library, favs: &Favorites) -> &'a [AlbumKey] {
        if favs.album_count() == 0 {
            return &[];
        }
        if !self.fresh {
            self.hearted = lib
                .albums()
                .iter()
                .filter(|key| favs.is_album(key))
                .cloned()
                .collect();
            self.fresh = true;
        }
        &self.hearted
    }

    /// What the `FAVORITES` section held the last time the view was drawn.
    ///
    /// The section is built once and cached, so this is what was on screen — which makes it
    /// the only part of the sectioned layout the integration test in [`crate::views`] can
    /// see, that test having a `ViewState` and no pixels.
    #[cfg(test)]
    pub fn section(&self) -> &[AlbumKey] {
        &self.hearted
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State) {
    views::page(ui, |ui| {
        views::heading(ui, "ALBUMS");
        if cx.lib.album_count() == 0 {
            views::centered_note(ui, &["NO ALBUMS YET"]);
            return;
        }
        // Copy the shared references out of `cx` so the slices do not borrow `cx` itself.
        let (lib, favs) = (cx.lib, cx.favs);
        let hearted = st.hearted(lib, favs);
        if hearted.is_empty() {
            // Nothing hearted, no chrome: the plain grid, virtualized by `show_rows`.
            album_card::grid(ui, cx, lib.albums(), GRID_ID);
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt(GRID_ID)
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let g = Geom::measure(ui);
                views::section(ui, "FAVORITES");
                band(ui, cx, hearted, g, viewport, FAVORITES_ID);
                ui.add_space(theme::SECTION_GAP);
                views::section(ui, "ALL ALBUMS");
                // Every album, the hearted ones included: a grid that is not the whole
                // library is not a place anything can be found in.
                band(ui, cx, lib.albums(), g, viewport, ALL_ID);
            });
    });
}

/// The card geometry both bands lay out on.
///
/// It mirrors `album_card`'s private `metrics`, because a heading and two grids cannot share
/// one [`egui::ScrollArea::show_rows`] — that reserves a single uniform row height for the
/// whole scroll range and has nowhere to put the heading. The mirror has to agree with the
/// original or a band's rows land at the wrong `y`; `width` is measured once and handed to
/// both bands so at least the two of them can never disagree with each other.
#[derive(Clone, Copy)]
struct Geom {
    columns: usize,
    /// Card height plus the gutter under it — the pitch `album_card`'s rows advance by.
    row_h: f32,
    width: f32,
}

impl Geom {
    fn measure(ui: &Ui) -> Geom {
        let width = ui.available_width();
        let columns =
            (((width + theme::GRID_GUTTER) / (theme::CARD_W + theme::GRID_GUTTER)).floor()
                as usize)
                .max(1);
        let gutters = (columns as f32 - 1.0) * theme::GRID_GUTTER;
        let card_w = ((width - gutters) / columns as f32).min(width).max(48.0);
        Geom {
            columns,
            // Never zero: the row pitch is a divisor below.
            row_h: (album_card::card_height(ui, card_w) + theme::GRID_GUTTER).max(1.0),
            width,
        }
    }
}

/// One grid inside the shared scroller: reserve every row's height, draw only the rows the
/// viewport shows.
///
/// Skipping the off-screen rows is not only about layout cost. Every card that is drawn asks
/// the cover cache for its texture, and that cache is capped
/// ([`crate::artwork`]'s `MAX_TEXTURES`) at a few more covers than one screen holds — drawing
/// a whole library's worth of cards would evict what is on screen once per frame and leave
/// the loader thread thrashing forever.
///
/// `viewport` is the visible rectangle in the scroller's content coordinates, where `0` is
/// the top of the content; `id_salt` separates this band's cards from the other band's.
///
/// **Both calls are unconditional, empty range or not.** `allocate_space` and `scope_builder`
/// each advance the shared parent ui's auto-id counter by one, and the second band's own
/// `id_salt` does not insulate it from that: egui salts a child ui with
/// `parent.id.with(salt).with(parent.next_auto_id_salt)`. Skipping the scope on a frame when
/// this band shows nothing would therefore renumber every card of the band *below* it — the
/// exact id migration the skip-ahead below exists to prevent, only all at once and for the
/// whole grid.
fn band(ui: &mut Ui, cx: &mut Ctx, keys: &[AlbumKey], g: Geom, viewport: Rect, id_salt: &str) {
    let rows = keys.len().div_ceil(g.columns);
    let (_, area) = ui.allocate_space(Vec2::new(g.width, rows as f32 * g.row_h));
    // Content coordinates measured from the top of the scroll content, which is where this
    // ui's `max_rect` starts — the same origin egui's own `show_rows` reckons from.
    let top = area.top() - ui.max_rect().top();
    let range = visible_rows(rows, g.row_h, viewport.min.y - top, viewport.max.y - top);
    let rect = Rect::from_min_max(
        egui::pos2(area.left(), area.top() + range.start as f32 * g.row_h),
        egui::pos2(area.right(), area.top() + range.end as f32 * g.row_h),
    );
    let first = range.start * g.columns;
    let last = (range.end * g.columns).min(keys.len());
    ui.scope_builder(UiBuilder::new().max_rect(rect).id_salt(id_salt), |ui| {
        if range.is_empty() {
            return;
        }
        // Pretend the skipped rows were laid out, so a card keeps the id it had before the
        // scroll — otherwise an open context menu would migrate to whatever row took its
        // place and act on that card's album. `show_rows` does this for the plain grid, for
        // the same reason. One id per row: `grid_row`'s `horizontal` scope. The gutter
        // `add_space` after it costs none — it only moves the cursor.
        ui.skip_ahead_auto_ids(range.start);
        album_card::grid_inline(ui, cx, &keys[first..last]);
    });
}

/// Which rows of a `rows`-row band a viewport reaching from `top` to `bottom` shows.
///
/// Both bounds are in the band's own coordinates, so `0.0` is the top of its first row and a
/// band scrolled past has a negative `bottom`. One row of slack at the far end, like
/// `ScrollArea::show_rows`.
fn visible_rows(rows: usize, row_h: f32, top: f32, bottom: f32) -> Range<usize> {
    let first = (top / row_h).floor();
    let last = (bottom / row_h).ceil() + 1.0;
    if last <= 0.0 || first >= rows as f32 {
        return 0..0;
    }
    (first.max(0.0) as usize).min(rows)..(last.max(0.0) as usize).min(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoebus_core::Track;

    fn lib() -> Library {
        let mut tracks = Vec::new();
        for rel in [
            "Ann/Aleph/01 a.m4a",
            "Bob/Beth/01 b.m4a",
            "Cal/Gimel/01 c.m4a",
        ] {
            let mut t = Track::new(rel);
            t.refresh_key();
            tracks.push(t);
        }
        Library::build("/lib", tracks)
    }

    fn favorites() -> Favorites {
        let mut favs = Favorites::load_from(std::path::Path::new(
            "/nonexistent/phoebus-albums-test/favorites.json",
        ));
        favs.set_ephemeral(true);
        favs
    }

    /// Nothing hearted: no section, no `Vec`, no clones.
    #[test]
    fn an_unhearted_library_has_no_section_and_allocates_nothing() {
        let l = lib();
        let favs = favorites();
        let mut st = State::default();
        assert!(st.hearted(&l, &favs).is_empty());
        assert!(
            st.hearted.is_empty(),
            "the common case must not allocate a section"
        );
    }

    /// A hearted album reaches the section in library order — and stays in the grid, which is
    /// the whole change: the section is a copy, not a move.
    #[test]
    fn hearting_fills_the_section_and_leaves_the_grid_untouched() {
        let l = lib();
        let mut favs = favorites();
        let sorted: Vec<AlbumKey> = l.albums().to_vec();
        // The last album hearted first: neither the section's order nor the grid's may come
        // from the order the hearts were clicked in.
        favs.toggle_album(&sorted[2]);
        favs.toggle_album(&sorted[1]);

        let mut st = State::default();
        assert_eq!(
            st.hearted(&l, &favs),
            &sorted[1..3],
            "the section is sorted like the grid, not like the clicks"
        );
        assert_eq!(
            l.albums(),
            sorted.as_slice(),
            "and the grid below it still holds every album, hearted ones included"
        );

        // The cache is built once and only rebuilt when something invalidates it, which is
        // what every toggle and every rescan does.
        st.hearted[0] = sorted[0].clone();
        assert_eq!(
            st.hearted(&l, &favs)[0],
            sorted[0],
            "the cache was rebuilt when nothing had changed"
        );
        st.invalidate();
        assert_eq!(st.hearted(&l, &favs)[0], sorted[1]);

        // Unhearting the last favourite takes the section away entirely.
        favs.toggle_album(&sorted[1]);
        favs.toggle_album(&sorted[2]);
        st.invalidate();
        assert!(st.hearted(&l, &favs).is_empty());
    }

    /// A favourite hearted in some other library is kept in `favorites.json` but has no card
    /// to draw, so the section must not appear for it.
    #[test]
    fn a_favourite_this_library_does_not_hold_draws_no_section() {
        let l = lib();
        let mut favs = favorites();
        favs.toggle_album(&AlbumKey::new("Ghost", "Nowhere"));

        let mut st = State::default();
        assert_eq!(favs.album_count(), 1, "it is still hearted…");
        assert!(
            st.hearted(&l, &favs).is_empty(),
            "…and still has nothing to show"
        );
    }

    /// Everything the two bands' card ids rest on is an id *count*, and egui documents none
    /// of it. So it is pinned here: lay a shape out, then lay out the same number of ids with
    /// `skip_ahead_auto_ids`, and the auto id both passes arrive at has to be the same one.
    ///
    /// Two counts, and the second is why the empty band still opens its scope. A band that
    /// drew nothing used to cost one id instead of two, which silently renumbered every card
    /// of the band below it on the frame the favourites section scrolled off the top.
    fn auto_id_after(shape: impl Fn(&mut Ui)) -> egui::Id {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };
        let mut id = None;
        let mut out = ctx.run_ui(input, |ui| {
            shape(ui);
            id = Some(ui.next_auto_id());
        });
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        out.textures_delta.clear();
        id.expect("the ui ran")
    }

    #[test]
    fn a_grid_row_costs_one_auto_id_and_a_band_costs_two_drawn_or_not() {
        // `grid_row`: one `horizontal` scope, and a gutter `add_space` that costs nothing.
        assert_eq!(
            auto_id_after(|ui| {
                ui.horizontal(|_| {});
                ui.add_space(theme::GRID_GUTTER);
            }),
            auto_id_after(|ui| ui.skip_ahead_auto_ids(1)),
            "a grid row no longer costs exactly the one id `band` skips ahead by"
        );

        // `band` itself: `allocate_space` plus the scope, whether or not the scope draws.
        let band_shape = |draws: bool| {
            move |ui: &mut Ui| {
                let (_, area) = ui.allocate_space(Vec2::new(100.0, 100.0));
                ui.scope_builder(UiBuilder::new().max_rect(area).id_salt(ALL_ID), |ui| {
                    if draws {
                        ui.horizontal(|_| {});
                    }
                });
            }
        };
        let two = auto_id_after(|ui| ui.skip_ahead_auto_ids(2));
        assert_eq!(auto_id_after(band_shape(true)), two);
        assert_eq!(
            auto_id_after(band_shape(false)),
            two,
            "an empty band changed what the band after it is numbered from"
        );
    }

    /// The row window each band draws. The bands share one scroller, so every one of these
    /// cases happens on an ordinary scroll: the favourites band leaves through the top while
    /// the albums band is still arriving from the bottom.
    #[test]
    fn a_band_draws_the_rows_the_viewport_reaches_and_no_others() {
        // Unscrolled: from the first row to one past the last visible one.
        assert_eq!(visible_rows(10, 100.0, 0.0, 250.0), 0..4);
        // Scrolled into the middle of the band.
        assert_eq!(visible_rows(10, 100.0, 250.0, 500.0), 2..6);
        // Clamped at the end rather than running off it.
        assert_eq!(visible_rows(10, 100.0, 850.0, 1100.0), 8..10);
        // Entirely above the viewport (scrolled past) and entirely below it (not reached).
        assert_eq!(visible_rows(10, 100.0, 1000.0, 1250.0), 0..0);
        assert_eq!(visible_rows(10, 100.0, -500.0, -250.0), 0..0);
        // A band shorter than the viewport is drawn whole, and an empty one is not drawn.
        assert_eq!(visible_rows(2, 100.0, -50.0, 700.0), 0..2);
        assert_eq!(visible_rows(0, 100.0, 0.0, 700.0), 0..0);
    }
}
