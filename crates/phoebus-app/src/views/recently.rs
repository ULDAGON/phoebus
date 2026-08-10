//! Recently Added: the album grid ordered by `added_at`, newest first. No sort controls —
//! the order *is* the point of the view.

use egui::Ui;

use crate::nav::Ctx;
use crate::views;
use crate::widgets::album_card;

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx) {
    views::page(ui, |ui| {
        views::heading(ui, "RECENTLY ADDED");
        if cx.lib.album_count() == 0 {
            views::centered_note(ui, &["NO ALBUMS YET"]);
            return;
        }
        // Copy the shared reference out of `cx` so the slice does not borrow `cx` itself.
        let lib = cx.lib;
        album_card::grid(ui, cx, lib.recently_added(), "recently-grid");
    });
}
