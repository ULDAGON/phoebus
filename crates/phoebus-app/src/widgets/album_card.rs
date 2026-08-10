//! The album card and the responsive grid it lives in — shared by Recently Added, Albums,
//! the Artists view (C2) and Search (C2).
//!
//! The grid is fluid: the column count is however many [`theme::CARD_W`]-wide cards fit
//! the available width, and the cards then GROW to share that width exactly — no dead
//! right edge. Widening the window inflates every card until one more minimum-width
//! column fits, at which point the count bumps and every card snaps back toward the
//! minimum. The grid virtualizes through `ScrollArea::show_rows`, so a 10 000-album
//! library still only lays out what is visible.

use egui::{Align2, Rect, Sense, StrokeKind, TextStyle, Ui, Vec2};
use phoebus_core::AlbumKey;

use crate::artwork;
use crate::nav::{Action, Ctx, View};
use crate::theme;
use crate::widgets::{self, menus};

/// Draw a responsive grid of album cards.
///
/// `id_salt` keeps the scroll offsets of different views apart.
pub fn grid(ui: &mut Ui, cx: &mut Ctx, keys: &[AlbumKey], id_salt: &str) {
    if keys.is_empty() {
        return;
    }
    let m = metrics(ui);
    let rows = keys.len().div_ceil(m.columns);

    // `show_rows` reads `item_spacing` from *this* ui before it ever runs the closure, and
    // reserves `row_height + spacing.y` per row. `grid_row` supplies its own gutter with
    // `add_space`, so the vertical spacing has to be zeroed here — setting it inside the
    // closure reserved 6 px per row that nothing occupied, which is what made the
    // scrollbar and the scroll range drift apart on a long grid.
    ui.spacing_mut().item_spacing = Vec2::new(theme::GRID_GUTTER, 0.0);
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .show_rows(ui, m.card_h + theme::GRID_GUTTER, rows, |ui, range| {
            for r in range {
                grid_row(ui, cx, keys, r, m);
            }
        });
}

/// The same grid without a scroll area of its own — for the Search view, where all three
/// sections share one outer scroller. Lays every row out eagerly, so only ever call it
/// with a capped list (search sections top out at
/// [`phoebus_core::SECTION_CAP`](phoebus_core::SECTION_CAP) hits).
pub fn grid_inline(ui: &mut Ui, cx: &mut Ctx, keys: &[AlbumKey]) {
    if keys.is_empty() {
        return;
    }
    let m = metrics(ui);
    let rows = keys.len().div_ceil(m.columns);
    ui.spacing_mut().item_spacing = Vec2::new(theme::GRID_GUTTER, 0.0);
    for r in 0..rows {
        grid_row(ui, cx, keys, r, m);
    }
}

/// One frame's grid geometry, shared by every row so a whole pass agrees with itself.
#[derive(Clone, Copy)]
struct Metrics {
    columns: usize,
    /// The stretched card width: minimum [`theme::CARD_W`], at most one gutter-plus-card
    /// short of fitting another column.
    card_w: f32,
    card_h: f32,
}

/// Fit as many minimum-width cards as the width allows, then stretch them to share it.
fn metrics(ui: &Ui) -> Metrics {
    let available = ui.available_width();
    let columns =
        (((available + theme::GRID_GUTTER) / (theme::CARD_W + theme::GRID_GUTTER)).floor()
            as usize)
            .max(1);
    let gutters = (columns as f32 - 1.0) * theme::GRID_GUTTER;
    // On a window narrower than one minimum card, shrink rather than overflow.
    let card_w = ((available - gutters) / columns as f32)
        .min(available)
        .max(48.0);
    Metrics {
        columns,
        card_w,
        card_h: card_height(ui, card_w),
    }
}

fn grid_row(ui: &mut Ui, cx: &mut Ctx, keys: &[AlbumKey], r: usize, m: Metrics) {
    ui.horizontal(|ui| {
        for c in 0..m.columns {
            let Some(key) = keys.get(r * m.columns + c) else {
                break;
            };
            card(ui, cx, key, m.card_w, m.card_h);
        }
    });
    ui.add_space(theme::GRID_GUTTER);
}

/// Height of a card at `width`: square cover + gap + title line + artist line.
pub fn card_height(ui: &Ui, width: f32) -> f32 {
    width
        + theme::CARD_TEXT_GAP
        + ui.text_style_height(&TextStyle::Body)
        + 2.0
        + ui.text_style_height(&TextStyle::Small)
}

/// One album card: cover, title, artist, hover treatment, `▶` badge, favourite heart and
/// context menu.
fn card(ui: &mut Ui, cx: &mut Ctx, key: &AlbumKey, width: f32, height: f32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let cover_rect = Rect::from_min_size(rect.min, Vec2::splat(width));

    // The badge is registered after the card, so it wins the pointer where they overlap.
    let badge_rect = Rect::from_min_size(
        egui::pos2(
            cover_rect.left() + theme::CARD_TEXT_GAP,
            cover_rect.bottom() - theme::CARD_TEXT_GAP - theme::PLAY_BADGE,
        ),
        Vec2::splat(theme::PLAY_BADGE),
    );
    // Top-right, inset by the same gap that floats the play badge off the bottom-left
    // (UI-SPEC v1.3 §Favorites: "inset like the play badge's").
    let heart_rect = Rect::from_min_size(
        egui::pos2(
            cover_rect.right() - theme::CARD_TEXT_GAP - widgets::HEART_W,
            cover_rect.top() + theme::CARD_TEXT_GAP,
        ),
        Vec2::splat(widgets::HEART_W),
    );
    let hovering = ui.rect_contains_pointer(cover_rect);
    let hearted = cx.favs.is_album(key);
    // Both overlays are registered AFTER the card, so they win the pointer where they
    // overlap it — that is what keeps a click on either from also opening the album. The
    // heart is live whenever it is *visible*, which for a hearted album is always; the
    // play badge only ever exists on hover.
    let badge = if hovering {
        Some(ui.interact(badge_rect, response.id.with("play"), Sense::click()))
    } else {
        None
    };
    let heart = (hovering || hearted)
        .then(|| ui.interact(heart_rect, response.id.with("heart"), Sense::click()));

    artwork::paint_cover(ui, cx.art, Some(key), cover_rect);

    // Copy the shared library reference out of `cx` so these borrows outlive `&mut cx`.
    let lib = cx.lib;
    let album = lib.album(key);
    let title = album.map_or(key.album.as_str(), |a| a.title.as_str());
    let artist = album.map_or(key.artist.as_str(), |a| a.artist.as_str());
    let tracks: &[phoebus_core::TrackId] = album.map_or(&[], |a| a.track_ids.as_slice());

    if hovering {
        let painter = ui.painter_at(cover_rect);
        painter.rect_stroke(
            cover_rect,
            theme::corner(),
            theme::accent_line(),
            StrokeKind::Inside,
        );
        let badge_hovered = badge.as_ref().is_some_and(egui::Response::hovered);
        painter.rect_filled(
            badge_rect,
            theme::corner(),
            if badge_hovered {
                theme::p().accent_dim
            } else {
                theme::p().accent
            },
        );
        painter.text(
            badge_rect.center(),
            Align2::CENTER_CENTER,
            theme::GLYPH_PLAY,
            theme::font_icon(theme::ICON_SMALL),
            theme::p().on_accent,
        );
    }
    // The heart, painted last so it sits over the hover outline. An unhearted one gets a
    // scrim under it: it is a hairline outline in a text colour, and covers are
    // photographs — without a pad it disappears into a bright corner (UI-SPEC v1.3).
    if let Some(heart) = &heart {
        let painter = ui.painter_at(cover_rect);
        let hot = heart.hovered();
        let p = theme::p();
        let (font, color) = if hearted {
            (
                theme::font_icon_fill(theme::ICON_HEART),
                theme::hover_color(hot, p.accent_text, p.accent_text_dim),
            )
        } else {
            painter.rect_filled(heart_rect, theme::corner(), p.scrim);
            (
                theme::font_icon(theme::ICON_HEART),
                theme::hover_color(hot, p.text_hi, p.accent_text),
            )
        };
        painter.text(
            heart_rect.center(),
            Align2::CENTER_CENTER,
            theme::GLYPH_HEART,
            font,
            color,
        );
    }

    let mut y = cover_rect.bottom() + theme::CARD_TEXT_GAP;
    let title_h = ui.text_style_height(&TextStyle::Body);
    widgets::text_left(
        ui,
        egui::pos2(rect.left(), y + title_h * 0.5),
        title,
        theme::font_body(),
        theme::p().text_hi,
        width,
    );
    y += title_h + 2.0;
    let small_h = ui.text_style_height(&TextStyle::Small);
    widgets::text_left(
        ui,
        egui::pos2(rect.left(), y + small_h * 0.5),
        artist,
        theme::font_small(),
        theme::p().text_mid,
        width,
    );

    if heart.is_some_and(|h| h.clicked()) {
        cx.act(Action::ToggleFavAlbum(key.clone()));
    } else if badge.is_some_and(|b| b.clicked()) {
        cx.act(Action::PlayCollection(tracks.to_vec()));
    } else if response.clicked() {
        cx.act(Action::Go(View::Album(key.clone())));
    }

    response.context_menu(|ui| menus::album_menu(ui, cx, key, tracks));
}
