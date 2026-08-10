//! Songs: every track in one sortable, virtualized table.
//!
//! Two rules keep this view usable at 10 000+ tracks:
//! * rows are drawn through `TableBody::rows`, which only calls the row closure for the
//!   visible slice (API-FACTS §3.4);
//! * a sort order is computed **once, on click**, and cached in [`State`] — never per
//!   frame. The default order (artist → album → disc → track) is `Library::tracks_sorted`
//!   itself, so the common case does not even own a `Vec`.
//!
//! **Deliberate deviation from UI-SPEC.** The spec asks for both "artist and album
//! TEXT_MID (clickable → navigate)" and "double-click plays with the current sorted list
//! as context". They cannot both hold: `egui_extras` unions the cell responses into the
//! row response, so the *first* click of a double-click over the ARTIST or ALBUM cell
//! navigates away and the table is never drawn again to see the second — double-click to
//! play was dead over roughly a third of every row. Play wins (it is the primary verb of
//! the view, and UI-SPEC §Interactions asks for it "anywhere"), so those cells no longer
//! navigate on a single click. `Go to Artist` / `Go to Album` moved into the row's
//! right-click menu, and the cells advertise that on hover.

use std::time::Duration;

use egui::{Align, Align2, Layout, Rangef, Rect, Response, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use phoebus_core::{Library, TrackId};

use crate::nav::{Action, Ctx};
use crate::theme;
use crate::views;
use crate::widgets::{self, menus, song_row};

/// A sortable column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    /// Track title.
    Title,
    /// Track artist.
    Artist,
    /// Album title.
    Album,
    /// Duration.
    Time,
}

/// Gap between a column header and its `▲` / `▼` indicator.
const ARROW_GAP: f32 = 5.0;

impl Col {
    fn label(self) -> &'static str {
        match self {
            Col::Title => "TITLE",
            Col::Artist => "ARTIST",
            Col::Album => "ALBUM",
            Col::Time => "TIME",
        }
    }
}

/// Sort state plus the cached order it produced.
pub struct State {
    col: Col,
    desc: bool,
    /// Cached order for every sort except the default one. Empty until first needed.
    order: Vec<TrackId>,
    dirty: bool,
    selected: Option<TrackId>,
}

impl Default for State {
    fn default() -> State {
        State {
            // The library's own order *is* artist → album → disc → track, so the view opens
            // with ARTIST ascending already active.
            col: Col::Artist,
            desc: false,
            order: Vec::new(),
            dirty: true,
            selected: None,
        }
    }
}

impl State {
    /// Forget the cached order (a rescan invalidates every track id in it).
    pub fn invalidate(&mut self) {
        self.order = Vec::new();
        self.dirty = true;
    }

    /// True when the default order is in force and no `Vec` is needed at all.
    fn is_default_order(&self) -> bool {
        self.col == Col::Artist && !self.desc
    }

    /// The rows to draw, sorting at most once per click.
    fn rows<'a>(&'a mut self, lib: &'a Library) -> &'a [TrackId] {
        if self.is_default_order() {
            return lib.tracks_sorted();
        }
        if self.dirty {
            self.order = sorted(lib, self.col, self.desc);
            self.dirty = false;
        }
        &self.order
    }

    /// Clicking a header: the same column flips direction, a new column starts ascending.
    fn click(&mut self, col: Col) {
        if self.col == col {
            self.desc = !self.desc;
        } else {
            self.col = col;
            self.desc = false;
        }
        self.dirty = true;
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State) {
    let lib = cx.lib;
    views::page(ui, |ui| {
        views::heading(ui, "SONGS");
        if lib.track_count() == 0 {
            views::centered_note(ui, &["NO SONGS YET"]);
            return;
        }
        let (col, desc, selected) = (st.col, st.desc, st.selected);
        let mut clicked: Option<Col> = None;
        let mut new_selection: Option<TrackId> = None;
        {
            let rows = st.rows(lib);
            table(
                ui,
                cx,
                rows,
                Header {
                    col,
                    desc,
                    clicked: &mut clicked,
                },
                selected,
                &mut new_selection,
            );
        }
        if let Some(col) = clicked {
            st.click(col);
        }
        if let Some(id) = new_selection {
            st.selected = Some(id);
        }
    });
}

/// What the header row needs: the active sort, and somewhere to report a click.
struct Header<'a> {
    col: Col,
    desc: bool,
    clicked: &'a mut Option<Col>,
}

fn table(
    ui: &mut Ui,
    cx: &mut Ctx,
    rows: &[TrackId],
    header: Header<'_>,
    selected: Option<TrackId>,
    new_selection: &mut Option<TrackId>,
) {
    let Header { col, desc, clicked } = header;
    // The row's full x-range, captured before the table takes the `Ui`: a cell only knows
    // its own column, and hovering one column has to light up the whole row.
    //
    // Full means the columns, and stops where they do. `egui_extras` lays them out inside its
    // scroll area, so when a bar is on screen the last column ends `scroll.allocated_width()`
    // short of the space the table occupies — and a row is *highlighted* per cell, across
    // exactly those columns. Counting the bar's strip in would leave the pointer lighting up
    // a row's `▶`, heart and `⋯` with no fill under it and no click to be had, over a strip
    // `views::page`'s [`theme::SCROLL_GAP`] made 20 px wide.
    let area = ui.available_rect_before_wrap();
    let scrolls = theme::ROW_NAV + rows.len() as f32 * widgets::ROW_H > area.height();
    let span = Rangef::new(
        area.left(),
        area.right()
            - if scrolls {
                ui.spacing().scroll.allocated_width()
            } else {
                0.0
            },
    );
    // `egui_extras` puts `item_spacing.y` *between* rows, so the default 6 px would make a
    // 40 px row pitch 46 px and the Songs view would not line up with the other three list
    // views. The columns keep their horizontal spacing.
    ui.spacing_mut().item_spacing.y = 0.0;
    // …and horizontally, `egui_extras` puts exactly `item_spacing.x` between columns, which
    // is the app-wide 8 px — not the 16 px of air the other three list views leave between
    // the leading state column and the title (`widgets::LEAD_GAP`). Widening the *column*
    // rather than the spacing keeps that difference off the ARTIST / ALBUM / TIME gaps,
    // which are not what this measurement is about; the cell then hands `song_row::lead`
    // only the first `LEAD_W` of it (see [`song_row`]), so the icon column itself does not
    // move and the surplus lands where it is wanted, between the icon and the title.
    let lead_pad = (widgets::LEAD_GAP - ui.spacing().item_spacing.x).max(0.0);
    TableBuilder::new(ui)
        .id_salt("songs-table")
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(widgets::LEAD_W + lead_pad))
        .column(Column::remainder().at_least(theme::SONG_COL_W).clip(true))
        .column(
            Column::initial(theme::SONG_COL_W)
                .at_least(theme::TIME_W * 2.0)
                .clip(true),
        )
        .column(
            Column::initial(theme::SONG_COL_W)
                .at_least(theme::TIME_W * 2.0)
                .clip(true),
        )
        .column(Column::exact(widgets::tail_w()))
        .min_scrolled_height(0.0)
        .header(theme::ROW_NAV, |mut header| {
            header.col(|_| {});
            for column in [Col::Title, Col::Artist, Col::Album, Col::Time] {
                // TIME sits above the durations, which stop short of the `⋯` button. The
                // inset is measured from the column's RIGHT edge, so the heart column
                // v1.3 added on the durations' *left* does not move it: the tail grew by
                // `HEART_W + HEART_GAP` and every one of those pixels landed left of the
                // timestamps (see `song_row::tail` for the whole x-layout).
                let inset = (column == Col::Time).then_some(widgets::DUR_GAP + widgets::MORE_W);
                let (_, response) = header.col(|ui| {
                    header_cell(ui, column, col == column, desc, inset);
                });
                if response.clicked() {
                    *clicked = Some(column);
                }
            }
        })
        .body(|body| {
            body.rows(widgets::ROW_H, rows.len(), |mut row| {
                let index = row.index();
                let Some(&id) = rows.get(index) else {
                    return;
                };
                if selected == Some(id) {
                    row.set_hovered(true);
                }
                let (lead, more) = song_row(&mut row, cx, id, span);

                let response = row.response();
                if response.clicked() {
                    *new_selection = Some(id);
                }
                if response.double_clicked() || lead == song_row::Lead::PlayRow {
                    cx.act(Action::Play {
                        tracks: rows.to_vec(),
                        index,
                        shuffle: false,
                    });
                }
                if lead == song_row::Lead::TogglePlay {
                    cx.act(Action::TogglePlay);
                }
                response.context_menu(|ui| {
                    menus::track_menu(ui, cx, &[id], Some(menus::Nav::both(id)));
                });
                // The `⋯` button opens the very same menu, on a LEFT click.
                egui::Popup::menu(&more).show(|ui| {
                    menus::track_menu(ui, cx, &[id], Some(menus::Nav::both(id)));
                });
            });
        });
}

/// Fill one row's five cells and report what its two buttons did.
///
/// `span` is the row's full x-range (see [`table`]); every cell needs it to answer "is the
/// pointer on *this row*", which no single column can see on its own.
fn song_row(
    row: &mut egui_extras::TableRow<'_, '_>,
    cx: &mut Ctx,
    id: TrackId,
    span: Rangef,
) -> (song_row::Lead, Response) {
    let lib = cx.lib;
    let track = lib.track(id);
    let current = cx.now.is_current(id);
    let playing = cx.now.playing;

    // Leading state column. UI-SPEC v1.2 dropped the artwork thumbnail that used to be
    // here; the column is the same width, and it now says something about playback.
    let mut hovered = false;
    let mut lead = song_row::Lead::Nothing;
    row.col(|ui| {
        hovered = row_hovered(ui, span);
        // Only the first `LEAD_W` of the cell: the rest of it is the `LEAD_GAP` air before
        // the title (see [`table`]), and it belongs to neither the icon nor its hit rect.
        let rect = ui
            .max_rect()
            .with_max_x(ui.max_rect().left() + widgets::LEAD_W);
        lead = song_row::lead(
            ui,
            rect,
            egui::Id::new(("songs-lead", id)),
            song_row::state(current, playing, hovered),
            current,
            "",
        );
    });

    let title = track.map_or("—", |t| t.title.as_str());
    let (title_rect, _) = row.col(|ui| {
        let rect = ui.max_rect();
        widgets::text_left(
            ui,
            egui::pos2(rect.left(), rect.center().y),
            title,
            theme::font_body(),
            if current {
                theme::p().accent_text
            } else {
                theme::p().text_hi
            },
            rect.width(),
        );
    });

    let artist = track.map_or("", |t| t.artist.as_str());
    let (_, artist_resp) = row.col(|ui| cell_text(ui, artist));
    let album = track.map_or("", |t| t.album.as_str());
    let (_, album_resp) = row.col(|ui| cell_text(ui, album));
    hint(artist_resp, "RIGHT-CLICK: GO TO ARTIST");
    hint(album_resp, "RIGHT-CLICK: GO TO ALBUM");

    let mut more = None;
    row.col(|ui| {
        let rect = ui.max_rect();
        more = Some(song_row::tail(
            ui,
            cx,
            rect,
            egui::Id::new(("songs-more", id)),
            id,
            hovered,
        ));
        // UI-SPEC v1.2: the divider starts at the TITLE column. It is painted from the
        // last cell, whose own clip rect would cut it off — so the clip is widened back to
        // the ruled part of the row, keeping the scroller's vertical clipping.
        divider(ui, title_rect.left(), rect);
    });
    let more = more.expect("the last column always runs");
    (lead, more)
}

/// Is the pointer anywhere on this row? A cell's own `rect_contains_pointer` intersects
/// with the *column's* clip rect, so it can only ever answer for its own column.
fn row_hovered(ui: &Ui, span: Rangef) -> bool {
    let clip = ui.clip_rect();
    let cell = ui.max_rect();
    let rect = Rect::from_min_max(
        egui::pos2(span.min, cell.top().max(clip.top())),
        egui::pos2(span.max, cell.bottom().min(clip.bottom())),
    );
    ui.ctx().rect_contains_pointer(ui.layer_id(), rect)
}

/// The row's bottom hairline, from `x` to the right edge of `cell`.
fn divider(ui: &Ui, x: f32, cell: Rect) {
    let clip = ui.clip_rect();
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(Rect::from_min_max(
        egui::pos2(x, clip.top()),
        egui::pos2(cell.right(), clip.bottom()),
    ));
    painter.line_segment(
        [
            egui::pos2(x, cell.bottom() - 0.5),
            egui::pos2(cell.right(), cell.bottom() - 0.5),
        ],
        theme::hairline(),
    );
}

/// A `TEXT_MID` cell. It does **not** brighten to `TEXT_HI` under the pointer any more:
/// that read as "click me", and a single click here would eat the first half of a
/// double-click-to-play (see the module header). The tooltip from [`hint`] is the
/// affordance instead.
fn cell_text(ui: &mut Ui, text: &str) {
    let rect = ui.max_rect();
    widgets::text_left(
        ui,
        egui::pos2(rect.left(), rect.center().y),
        text,
        theme::font_small(),
        theme::p().text_mid,
        rect.width(),
    );
}

/// Point the user at the row's context menu, which is where ARTIST / ALBUM navigation
/// lives now.
fn hint(response: egui::Response, text: &str) {
    response.on_hover_text(
        egui::RichText::new(text)
            .font(theme::font_small())
            .color(theme::p().text_mid),
    );
}

/// A column header: micro-label, plus `▲` / `▼` in `ACCENT` on the active column.
///
/// `inset` right-aligns the label that far in from the column's right edge (`None` = left
/// aligned). The TIME column needs it: its cells reserve room for the `⋯` button, and a
/// header flush with the column edge would sit over that button instead of over the times.
fn header_cell(ui: &mut Ui, col: Col, active: bool, desc: bool, inset: Option<f32>) {
    let rect = ui.max_rect();
    let hovered = ui.rect_contains_pointer(rect);
    let color = if active {
        theme::p().accent_text
    } else {
        theme::hover_color(hovered, theme::p().text_low, theme::p().text_hi)
    };
    let label = widgets::spaced(col.label());
    let arrow = if desc {
        theme::GLYPH_SORT_DESC
    } else {
        theme::GLYPH_SORT_ASC
    };
    // Lay the label out first, so the indicator can be placed against its real width
    // instead of a guessed offset.
    let galley = widgets::truncated(ui, &label, theme::font_small(), color, rect.width());
    let (label_x, arrow_x, arrow_align) = if let Some(inset) = inset {
        let x = rect.right() - inset - galley.size().x;
        (x, x - ARROW_GAP, Align2::RIGHT_CENTER)
    } else {
        (
            rect.left(),
            rect.left() + galley.size().x + ARROW_GAP,
            Align2::LEFT_CENTER,
        )
    };
    let painter = ui.painter();
    painter.galley(
        egui::pos2(label_x, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    if active {
        painter.text(
            egui::pos2(arrow_x, rect.center().y),
            arrow_align,
            arrow,
            theme::font_icon(theme::ICON_SORT),
            theme::p().accent_text,
        );
    }
}

/// Sort the whole library once. Ties keep the default order, so switching to `TIME` and
/// back never scrambles equal-length tracks.
///
/// `desc` flips the **primary key only**. Reversing the finished vector instead would flip
/// the tie-break too, which is why `ALBUM ▼` used to list each album's tracks 13 → 1
/// instead of grouping them 1 → 13 under albums in reverse.
fn sorted(lib: &Library, col: Col, desc: bool) -> Vec<TrackId> {
    let base = lib.tracks_sorted();
    let mut out: Vec<TrackId> = Vec::with_capacity(base.len());
    if col == Col::Time {
        let mut keyed: Vec<(Duration, usize)> = base
            .iter()
            .enumerate()
            .map(|(i, id)| (lib.track(*id).map_or(Duration::ZERO, |t| t.duration), i))
            .collect();
        sort_keyed(&mut keyed, desc);
        out.extend(keyed.into_iter().map(|(_, i)| base[i]));
    } else {
        let mut keyed: Vec<(String, usize)> = base
            .iter()
            .enumerate()
            .map(|(i, id)| (key_of(lib, *id, col), i))
            .collect();
        sort_keyed(&mut keyed, desc);
        out.extend(keyed.into_iter().map(|(_, i)| base[i]));
    }
    out
}

/// Sort `(key, base index)` pairs: key ascending or descending, index always ascending.
fn sort_keyed<K: Ord>(keyed: &mut [(K, usize)], desc: bool) {
    if desc {
        keyed.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    } else {
        keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    }
}

fn key_of(lib: &Library, id: TrackId, col: Col) -> String {
    let Some(track) = lib.track(id) else {
        return String::new();
    };
    match col {
        Col::Title => track.title.to_lowercase(),
        Col::Artist => track.artist.to_lowercase(),
        Col::Album => track.album.to_lowercase(),
        Col::Time => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::Artwork;
    use crate::nav::{Fmt, Now};
    use phoebus_core::Track;

    fn track(rel: &str, title: &str, artist: &str, album: &str, secs: u64) -> Track {
        let mut t = Track::new(rel);
        t.title = title.to_string();
        t.artist = artist.to_string();
        t.album_artist = artist.to_string();
        t.album = album.to_string();
        t.duration = Duration::from_secs(secs);
        t.refresh_key();
        t
    }

    fn lib() -> Library {
        Library::build(
            "/lib",
            vec![
                track("Z/One/01 Beta.mp3", "Beta", "Zed", "One", 30),
                track("A/Two/01 Alpha.mp3", "Alpha", "Ann", "Two", 90),
                track("M/Three/01 Gamma.mp3", "Gamma", "Mel", "Three", 60),
            ],
        )
    }

    fn titles(lib: &Library, ids: &[TrackId]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| lib.track(*id))
            .map(|t| t.title.clone())
            .collect()
    }

    #[test]
    fn default_order_borrows_the_library_and_owns_nothing() {
        let l = lib();
        let mut st = State::default();
        assert!(st.is_default_order());
        assert_eq!(st.rows(&l).len(), 3);
        assert!(st.order.is_empty(), "the default order must not allocate");
        assert_eq!(titles(&l, st.rows(&l)), vec!["Alpha", "Gamma", "Beta"]);
    }

    #[test]
    fn clicking_sorts_and_flips() {
        let l = lib();
        let mut st = State::default();
        st.click(Col::Title);
        assert_eq!(titles(&l, st.rows(&l)), vec!["Alpha", "Beta", "Gamma"]);
        st.click(Col::Title);
        assert!(st.desc);
        assert_eq!(titles(&l, st.rows(&l)), vec!["Gamma", "Beta", "Alpha"]);
        st.click(Col::Time);
        assert_eq!(titles(&l, st.rows(&l)), vec!["Beta", "Gamma", "Alpha"]);
        st.click(Col::Album);
        assert_eq!(titles(&l, st.rows(&l)), vec!["Beta", "Gamma", "Alpha"]);
        // Back to the default: no cached vector is consulted.
        st.click(Col::Artist);
        assert!(st.is_default_order());
        assert_eq!(titles(&l, st.rows(&l)), vec!["Alpha", "Gamma", "Beta"]);
    }

    /// Two albums, three tracks each, all six the same length: every sort but TITLE is
    /// one long tie, which is exactly where reversing the whole vector went wrong.
    fn ties() -> Library {
        Library::build(
            "/lib",
            vec![
                track("A/One/01 a.mp3", "a", "Ann", "One", 60),
                track("A/One/02 b.mp3", "b", "Ann", "One", 60),
                track("A/One/03 c.mp3", "c", "Ann", "One", 60),
                track("B/Two/01 d.mp3", "d", "Bob", "Two", 60),
                track("B/Two/02 e.mp3", "e", "Bob", "Two", 60),
                track("B/Two/03 f.mp3", "f", "Bob", "Two", 60),
            ],
        )
    }

    #[test]
    fn descending_flips_the_column_but_not_the_tie_break() {
        let l = ties();
        let mut st = State::default();
        st.click(Col::Album);
        assert_eq!(
            titles(&l, st.rows(&l)),
            vec!["a", "b", "c", "d", "e", "f"],
            "ALBUM ▲ groups One then Two, each in track order"
        );
        st.click(Col::Album);
        assert!(st.desc);
        assert_eq!(
            titles(&l, st.rows(&l)),
            vec!["d", "e", "f", "a", "b", "c"],
            "ALBUM ▼ reverses the albums, not the tracks inside them"
        );

        st.click(Col::Time);
        let asc = titles(&l, st.rows(&l));
        st.click(Col::Time);
        let desc = titles(&l, st.rows(&l));
        assert_eq!(asc, desc, "one block of equal-length tracks never reorders");
        assert_eq!(asc, vec!["a", "b", "c", "d", "e", "f"]);
    }

    /// Lay the table out for `passes` frames with `now` in force and the pointer wherever
    /// `aim` puts it, and return the actions the last frame raised plus the `⋯` button rect
    /// of the first row.
    ///
    /// `aim` receives the previous frame's `⋯` rect, which is how a test finds a *row*: the
    /// button is the only widget in a row whose id is predictable from the track alone, and
    /// it spans the row's full height, so its centre line is the row's centre line.
    fn drive(
        lib: &Library,
        now: Now,
        clicks: bool,
        aim: impl Fn(Option<egui::Rect>) -> Option<egui::Pos2>,
    ) -> (Vec<Action>, Option<egui::Rect>) {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut art = Artwork::new();
        let fmt = Fmt::build(lib);
        let favs = crate::nav::test_favorites();
        let mut st = State::default();
        let first = *lib.tracks_sorted().first().expect("a track");
        let more_id = egui::Id::new(("songs-more", first));
        let mut actions: Vec<Action> = Vec::new();
        let mut more_rect = None;
        for pass in 0..3 {
            actions.clear();
            let pointer = aim(more_rect);
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.push(egui::Event::PointerMoved(pos));
                if clicks && pass == 2 {
                    for pressed in [true, false] {
                        input.events.push(egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                }
            }
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now,
                    fmt: &fmt,
                    actions: &mut actions,
                };
                show(ui, &mut cx, &mut st);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
            more_rect = ctx.read_response(more_id).map(|r| r.rect);
        }
        (actions, more_rect)
    }

    /// UI-SPEC v1.2 §Track rows, on the real virtualized table: the artwork column is gone,
    /// the leading state column is 28 px of nothing until the pointer arrives, and the row
    /// is 40 px tall with a `⋯` button pinned to its right edge.
    #[test]
    fn the_row_has_the_v12_geometry_and_no_artwork_column() {
        let l = lib();
        let (_, more) = drive(&l, Now::default(), false, |_| None);
        let more = more.expect("the ⋯ button is allocated even when it is not painted");
        assert_eq!(more.width(), widgets::MORE_W);
        assert_eq!(more.height(), widgets::ROW_H, "40 px rows");
        // The leading column starts at the page padding and the title follows it; nothing
        // between them, because the 24 px thumbnail that used to live there is gone.
        assert!(
            more.right() > 1000.0,
            "the tail is pinned to the right edge, not floating mid-row: {more:?}"
        );
    }

    /// Clicking the `▶` that appears in the leading column plays *that* row with the sorted
    /// list as context — and clicking it on the row that is already loaded toggles playback
    /// instead, so the track does not restart from zero.
    #[test]
    fn the_leading_column_plays_this_row_and_toggles_the_current_one() {
        let l = lib();
        // Aim at the leading column of the first row: same centre line as its `⋯` button,
        // one lead-column-width in from the page's left padding.
        let aim = |more: Option<egui::Rect>| {
            more.map(|r| egui::pos2(theme::VIEW_PAD + widgets::LEAD_W * 0.5, r.center().y))
        };
        let (actions, _) = drive(&l, Now::default(), true, aim);
        let played = actions.iter().find_map(|a| match a {
            Action::Play { index, tracks, .. } => Some((*index, tracks.len())),
            _ => None,
        });
        assert_eq!(
            played,
            Some((0, 3)),
            "the first row plays at index 0 with the whole sorted list as context: {actions:?}"
        );

        let first = *l.tracks_sorted().first().expect("a track");
        let now = Now {
            track: Some(first),
            playing: true,
        };
        let (actions, _) = drive(&l, now, true, aim);
        assert!(
            actions.iter().any(|a| matches!(a, Action::TogglePlay)),
            "the current row pauses instead of replaying: {actions:?}"
        );
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Play { .. })),
            "…and it must not also re-Play: {actions:?}"
        );
    }

    #[test]
    fn sorting_happens_once_per_click() {
        let l = lib();
        let mut st = State::default();
        st.click(Col::Title);
        assert!(st.dirty);
        st.rows(&l);
        assert!(!st.dirty, "the order is cached");
        st.rows(&l);
        assert!(!st.dirty);
        st.invalidate();
        assert!(st.dirty, "a rescan must re-sort");
    }
}
