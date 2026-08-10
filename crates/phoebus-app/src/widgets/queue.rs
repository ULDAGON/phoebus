//! The Up Next drawer's body: header, rows, empty state.
//!
//! Rows mirror `PlayQueue::upcoming`, so a row index here is exactly the index
//! `jump_to_upcoming` / `remove_upcoming` expect.

use egui::{Align2, Rect, Sense, Ui, Vec2};
use phoebus_core::UpNext;

use crate::artwork;
use crate::nav::{Action, Ctx};
use crate::theme;
use crate::views;
use crate::widgets;

/// Draw the drawer's contents. `items` is `PlayQueue::upcoming(theme::QUEUE_MAX)`.
pub fn drawer(ui: &mut Ui, cx: &mut Ctx, items: &[UpNext]) {
    ui.add_space(theme::PANEL_PAD);
    // `CLEAR` empties the *manual* queue, so it only appears when there is one — offering
    // it against a pure context queue would be a button that does nothing.
    header(ui, cx, items.iter().any(|i| i.manual));
    ui.add_space(theme::CARD_TEXT_GAP);

    if items.is_empty() {
        views::centered_note(ui, &["QUEUE EMPTY"]);
        return;
    }

    ui.spacing_mut().item_spacing.y = 0.0;
    // The drawer is a side panel, not a `views::page`, so the gap between a row's right end
    // and the scrollbar is set here instead of inherited. The header above stays on the
    // panel's own edge: it is outside the scroller, and nothing in a row is right-aligned.
    ui.spacing_mut().scroll.bar_inner_margin = theme::SCROLL_GAP;
    egui::ScrollArea::vertical()
        .id_salt("queue-rows")
        .auto_shrink([false, false])
        .show_rows(ui, theme::ROW_QUEUE, items.len(), |ui, range| {
            for index in range {
                let Some(item) = items.get(index) else {
                    break;
                };
                row(ui, cx, index, *item);
            }
        });
}

fn header(ui: &mut Ui, cx: &mut Ctx, any: bool) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), theme::ROW_NAV),
        Sense::hover(),
    );
    widgets::text_left(
        ui,
        egui::pos2(rect.left(), rect.center().y),
        &widgets::spaced("UP NEXT"),
        theme::font_small(),
        theme::p().text_low,
        rect.width(),
    );
    if !any {
        return;
    }
    let label = widgets::spaced("CLEAR");
    let galley = widgets::truncated(
        ui,
        &label,
        theme::font_small(),
        theme::p().text_low,
        rect.width(),
    );
    let hit = Rect::from_min_max(
        egui::pos2(rect.right() - galley.size().x - 4.0, rect.top()),
        rect.max,
    );
    let response = ui.interact(hit, ui.id().with("queue-clear"), Sense::click());
    ui.painter().text(
        egui::pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        &label,
        theme::font_small(),
        theme::hover_color(response.hovered(), theme::p().text_low, theme::p().text_hi),
    );
    if response.clicked() {
        cx.act(Action::QueueClear);
    }
}

fn row(ui: &mut Ui, cx: &mut Ctx, index: usize, item: UpNext) {
    let (rect, response) = widgets::row(ui, theme::ROW_QUEUE, Sense::click());
    widgets::row_background(ui, rect, response.hovered(), false);

    let art_rect = Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - theme::QUEUE_ART * 0.5),
        Vec2::splat(theme::QUEUE_ART),
    );
    let lib = cx.lib;
    let track = lib.track(item.id);
    artwork::paint_cover(ui, cx.art, track.map(|t| &t.album_key), art_rect);

    let mut text_x = art_rect.right() + theme::CARD_TEXT_GAP;
    if item.manual {
        // Measured, not stepped over by a guessed constant. The old `◆` came out of the
        // mono face at ~6.6 px and a hard-coded 11 px step cleared it; an icon advances a
        // full em, so the same step would have run the title straight through the marker.
        let marker = widgets::truncated(
            ui,
            theme::GLYPH_MANUAL,
            theme::font_icon(theme::ICON_MARK),
            theme::p().accent_text,
            f32::INFINITY,
        );
        let size = marker.size();
        ui.painter().galley(
            egui::pos2(text_x, rect.center().y - size.y * 0.5),
            marker,
            theme::p().accent_text,
        );
        text_x += size.x + theme::CARD_TEXT_GAP;
    }
    let width = (rect.right() - text_x).max(1.0);
    let title_h = ui.text_style_height(&egui::TextStyle::Body);
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    let top = rect.center().y - (title_h + small_h) * 0.5;
    let (title, artist) = match track {
        Some(t) => (t.title.as_str(), t.artist.as_str()),
        None => ("—", ""),
    };
    widgets::text_left(
        ui,
        egui::pos2(text_x, top + title_h * 0.5),
        title,
        theme::font_body(),
        theme::p().text_hi,
        width,
    );
    widgets::text_left(
        ui,
        egui::pos2(text_x, top + title_h + small_h * 0.5),
        artist,
        theme::font_small(),
        theme::p().text_mid,
        width,
    );

    if response.clicked() {
        cx.act(Action::QueueJump(index));
    }
    response.context_menu(|ui| {
        if ui.button("Remove").clicked() {
            cx.act(Action::QueueRemove(index));
            ui.close();
        }
    });
}
