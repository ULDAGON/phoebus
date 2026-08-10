//! Artists: a 260 px list on the left, the selected artist's albums on the right, and a
//! draggable divider between them whose width outlives the run (UI-SPEC v1.4 §Panel
//! widths). The divider is hand-rolled — this split is two `UiBuilder` scopes, not two
//! panels — so it copies `egui::Panel`'s resize handle beat for beat.

use egui::{CursorIcon, Id, Rangef, Rect, Sense, Ui, UiBuilder};

use crate::nav::{Action, Ctx};
use crate::theme;
use crate::views::{self, ViewState};
use crate::widgets::{self, album_card};

/// Id source for the split's drag handle. A fixed name rather than one derived from the
/// surrounding `Ui`: the handle is read a frame before it is registered, so its id has to
/// hash to the same number whatever else the view laid out first.
const SPLIT_ID: &str = "artist-split";

/// Which artist the list has selected, and how wide the user dragged the split.
pub struct State {
    /// Index into `Library::artists()`, or `None` for "nothing selected" — where a
    /// `Go to Artist` for a name this library has no page for lands. Clamped on every
    /// draw, so a rescan that shrinks the library can never strand it out of range.
    pub selected: Option<usize>,
    /// Width of the left-hand list in points, as the user dragged it — *not* as it was
    /// last drawn. A window too narrow to honour it narrows the split for that frame only
    /// (see [`show`]), so widening the window brings the user's width back.
    ///
    /// Seeded from `state.json` when the app starts and persisted through
    /// [`Action::SetArtistListW`](crate::nav::Action::SetArtistListW) when it changes.
    pub list_w: f32,
}

impl Default for State {
    fn default() -> State {
        // Arriving from the sidebar opens on the first artist, as UI-SPEC's screenshot of
        // the split view shows. Only a failed `Go to Artist` clears the selection.
        State {
            selected: Some(0),
            list_w: theme::ARTIST_LIST_W.default,
        }
    }
}

/// One frame's geometry for the split: the rect either side of the divider, and where the
/// divider itself is painted.
///
/// The ceiling and the "is there room for the albums?" guard live together because they
/// have to agree on what "one card" means. [`Split::ceiling`] leaves the album side
/// *exactly* [`theme::CARD_W`], so [`Split::shows_albums`] has to count exactly one card as
/// enough: when it asked for more than a card, the far-right end of the drag — the position
/// a drag past the end clamps to — was the one position where the albums were not drawn at
/// all, and it persisted.
pub(crate) struct Split {
    list: Rect,
    detail: Rect,
    divider_x: f32,
}

impl Split {
    /// The widest the list may be on a page `full_w` wide: whatever leaves the album side
    /// one whole card, kept inside the range the divider may be dragged over. On a page too
    /// narrow for even that, the floor wins and the album side drops out exactly as it
    /// always has.
    ///
    /// Recomputed every frame and deliberately *not* written back into `list_w`: shrinking
    /// the window must not forget the width the user chose.
    ///
    /// (`clamp` cannot panic here: core's `every_panel_width_range_contains_its_default`
    /// pins `min < max`, and a `Rect`'s width is never NaN.)
    pub(crate) fn ceiling(full_w: f32) -> f32 {
        (full_w - theme::VIEW_PAD - theme::CARD_W)
            .clamp(theme::ARTIST_LIST_W.min, theme::ARTIST_LIST_W.max)
    }

    /// Split `full` with the list at `list_w`, honouring floor and ceiling.
    pub(crate) fn of(full: Rect, list_w: f32) -> Split {
        let split =
            full.left() + list_w.clamp(theme::ARTIST_LIST_W.min, Split::ceiling(full.width()));
        Split {
            list: Rect::from_min_max(full.min, egui::pos2(split, full.bottom())),
            detail: Rect::from_min_max(egui::pos2(split + theme::VIEW_PAD, full.top()), full.max),
            divider_x: split + theme::VIEW_PAD * 0.5,
        }
    }

    /// Is there room beside the list for the album grid? One whole card is enough — and one
    /// whole card is precisely what the ceiling leaves.
    pub(crate) fn shows_albums(&self) -> bool {
        self.detail.width() >= theme::CARD_W
    }
}

/// Resolve a `Go to Artist` name to a row of `Library::artists()`.
///
/// The list is grouped by album artist and sorted by a lowercased key, so the match is
/// case-insensitive — the same rule `Library::artist` uses. A name with no page at all
/// answers `None`, which shows the view with nothing selected instead of quietly leaving
/// whatever was selected before (artist `[0]` on a fresh start).
fn resolve(lib: &phoebus_core::Library, name: &str) -> Option<usize> {
    let key = name.trim().to_lowercase();
    lib.artists().iter().position(|a| a.sort_key == key)
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut ViewState) {
    let lib = cx.lib;
    if lib.artist_count() == 0 {
        views::page(ui, |ui| {
            views::heading(ui, "ARTISTS");
            views::centered_note(ui, &["NO ARTISTS YET"]);
        });
        return;
    }

    // `Go to Artist` hands the name over exactly once.
    if let Some(name) = st.pending_artist.take() {
        st.artists.selected = resolve(lib, &name);
        if st.artists.selected.is_none() {
            log::debug!("artists: no page for {name:?}; showing the list with no selection");
        }
    }
    let selected = st.artists.selected.map(|i| i.min(lib.artist_count() - 1));
    st.artists.selected = selected;

    views::page(ui, |ui| {
        let full = ui.available_rect_before_wrap();

        // Resolve the drag before laying anything out, the way `egui::Panel` does with its
        // own resize handle: reading last frame's response here is what keeps the divider
        // under the pointer instead of one frame behind it.
        if let Some(drag) = ui.read_response(Id::new(SPLIT_ID))
            && (drag.dragged() || drag.drag_stopped())
            && let Some(pointer) = drag.interact_pointer_pos()
        {
            let wanted = pointer.x - theme::VIEW_PAD * 0.5 - full.left();
            st.artists.list_w =
                wanted.clamp(theme::ARTIST_LIST_W.min, Split::ceiling(full.width()));
            cx.act(Action::SetArtistListW(st.artists.list_w));
        }

        let geom = Split::of(full, st.artists.list_w);
        let divider_x = geom.divider_x;

        let mut clicked: Option<usize> = None;
        ui.scope_builder(UiBuilder::new().max_rect(geom.list), |ui| {
            list(ui, cx, selected, &mut clicked);
        });
        if geom.shows_albums() {
            ui.scope_builder(UiBuilder::new().max_rect(geom.detail), |ui| {
                detail(ui, cx, selected);
            });
        }
        if let Some(index) = clicked {
            st.artists.selected = Some(index);
        }

        // The handle goes on last, on top of both scroll areas, so neither can eat the
        // drag — the same order, the same `resize_grab_radius_side` hit area and the same
        // three-state stroke egui gives a resizable panel's edge, so the two dividers in
        // this window behave identically (UI-SPEC v1.4 §Panel widths).
        let grab = ui.style().interaction.resize_grab_radius_side;
        let handle =
            egui::Rect::from_x_y_ranges(Rangef::point(divider_x).expand(grab), full.y_range());
        let response = ui.interact(handle, Id::new(SPLIT_ID), Sense::click_and_drag());
        if response.hovered() || response.dragged() {
            // Pinned at an end, egui's panel handle stops promising both directions and
            // points at the only one left (`Panel::cursor_icon`). The list is the left
            // side, so growing it is east and shrinking it is west.
            let width = geom.list.width();
            ui.set_cursor_icon(if width <= theme::ARTIST_LIST_W.min {
                CursorIcon::ResizeEast
            } else if width < Split::ceiling(full.width()) {
                CursorIcon::ResizeHorizontal
            } else {
                CursorIcon::ResizeWest
            });
        }
        let stroke = {
            let widgets = &ui.style().visuals.widgets;
            if response.dragged() {
                widgets.active.fg_stroke
            } else if response.hovered() {
                widgets.hovered.fg_stroke
            } else {
                theme::hairline()
            }
        };
        ui.painter().vline(divider_x, full.y_range(), stroke);
    });
}

fn list(ui: &mut Ui, cx: &mut Ctx, selected: Option<usize>, clicked: &mut Option<usize>) {
    let lib = cx.lib;
    widgets::micro(ui, "ARTISTS");
    ui.add_space(theme::CARD_TEXT_GAP);
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("artist-list")
        .auto_shrink([false, false])
        .show_rows(ui, theme::ROW_ARTIST, lib.artist_count(), |ui, range| {
            for index in range {
                let Some(artist) = lib.artists().get(index) else {
                    break;
                };
                if row(
                    ui,
                    &artist.name,
                    artist.album_keys.len(),
                    selected == Some(index),
                ) {
                    *clicked = Some(index);
                }
            }
        });
}

/// One list row: name over an album count, `ACCENT` text and a 2 px left bar when selected.
fn row(ui: &mut Ui, name: &str, albums: usize, selected: bool) -> bool {
    let (rect, response) = widgets::row(ui, theme::ROW_ARTIST, Sense::click());
    widgets::row_background(ui, rect, response.hovered() && !selected, false);
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + 5.0),
                egui::pos2(rect.left() + theme::ACTIVE_BAR_W, rect.bottom() - 5.0),
            ),
            egui::CornerRadius::ZERO,
            theme::p().accent_text,
        );
    }
    let color = if selected {
        theme::p().accent_text
    } else if response.hovered() {
        theme::p().text_hi
    } else {
        theme::p().text_mid
    };
    let x = rect.left() + theme::ACTIVE_BAR_W + theme::CARD_TEXT_GAP;
    let width = (rect.right() - x - 4.0).max(1.0);
    let title_h = ui.text_style_height(&egui::TextStyle::Body);
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    let top = rect.center().y - (title_h + small_h) * 0.5;
    widgets::text_left(
        ui,
        egui::pos2(x, top + title_h * 0.5),
        name,
        theme::font_body(),
        color,
        width,
    );
    widgets::text_left(
        ui,
        egui::pos2(x, top + title_h + small_h * 0.5),
        &count_label(albums, "ALBUM"),
        theme::font_small(),
        theme::p().text_low,
        width,
    );
    response.clicked()
}

fn detail(ui: &mut Ui, cx: &mut Ctx, selected: Option<usize>) {
    let lib = cx.lib;
    let Some(artist) = selected.and_then(|i| lib.artists().get(i)) else {
        views::centered_note(ui, &["NO ARTIST SELECTED"]);
        return;
    };
    views::heading(ui, &artist.name);
    let meta = format!(
        "{}{}{}",
        count_label(artist.album_keys.len(), "ALBUM"),
        theme::SEP,
        count_label(artist.track_count, "SONG"),
    );
    views::subheading(ui, &meta);
    ui.add_space(theme::VIEW_PAD * 0.7);
    album_card::grid(ui, cx, &artist.album_keys, "artist-albums");
}

/// `1 ALBUM` / `3 ALBUMS` — the only place this view formats a string, and it does so once
/// per visible row.
fn count_label(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}S")
    }
}
