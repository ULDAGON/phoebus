//! Album detail: back row, a 232 px cover with the metadata block, and the tracklist.

use egui::{Id, Rect, Sense, Ui, Vec2};
use phoebus_core::{AlbumKey, TrackId};

use crate::artwork;
use crate::nav::{self, Action, Ctx, View};
use crate::theme;
use crate::views;
use crate::widgets::{self, menus, song_row};

/// Draw the detail page for `key`.
pub fn show(ui: &mut Ui, cx: &mut Ctx, key: &AlbumKey) {
    let lib = cx.lib;
    let Some(album) = lib.album(key) else {
        views::page(ui, |ui| {
            views::heading(ui, "ALBUM");
            views::centered_note(ui, &["THIS ALBUM IS NO LONGER IN THE LIBRARY"]);
        });
        return;
    };

    views::page(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("album-detail")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                back_row(ui, cx);
                ui.add_space(theme::CARD_TEXT_GAP * 2.0);
                header(ui, cx, album);
                ui.add_space(theme::VIEW_PAD);
                tracklist(ui, cx, album);
            });
    });
}

/// `← ALBUMS`, the one control on this page that is an icon *and* a word.
///
/// The two are one [`widgets::icon_text`] galley with the arrow at [`theme::ICON_INLINE`]
/// rather than one `format!`-ed string at the label's own 11 px: the arrow has to be sized
/// against the capitals it stands next to, and a single string can only ever be one size.
fn back_row(ui: &mut Ui, cx: &mut Ctx) {
    let galley = widgets::icon_text(
        ui,
        theme::GLYPH_BACK,
        theme::ICON_INLINE,
        &widgets::spaced("ALBUMS"),
        theme::font_small(),
    );
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x, theme::HIT_MIN), Sense::click());
    let color = theme::hover_color(response.hovered(), theme::p().text_low, theme::p().text_hi);
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    if response.clicked() {
        cx.act(Action::Back);
    }
}

fn header(ui: &mut Ui, cx: &mut Ctx, album: &phoebus_core::Album) {
    ui.horizontal_top(|ui| {
        artwork::cover(ui, cx.art, Some(&album.key), theme::DETAIL_COVER);
        ui.add_space(theme::VIEW_PAD);
        ui.vertical(|ui| {
            let width = ui.available_width();
            ui.label(
                egui::RichText::new(&album.title)
                    .font(theme::font_heading())
                    .color(theme::p().text_hi),
            );
            ui.add_space(4.0);

            let galley = widgets::truncated(
                ui,
                &album.artist,
                theme::font_sub(),
                theme::p().text_mid,
                width,
            );
            let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
            ui.painter().galley(
                rect.min,
                galley,
                theme::hover_color(
                    response.hovered(),
                    theme::p().text_mid,
                    theme::p().accent_text,
                ),
            );
            if response.clicked() {
                cx.act(Action::GoArtist(album.artist.clone()));
                cx.act(Action::Go(View::Artists));
            }

            ui.add_space(6.0);
            let mut meta = String::new();
            if let Some(year) = album.year {
                meta.push_str(&year.to_string());
                meta.push_str(theme::SEP);
            }
            meta.push_str(&format!("{} SONGS", album.track_count()));
            meta.push_str(theme::SEP);
            meta.push_str(&nav::minutes(album.duration));
            ui.label(
                egui::RichText::new(meta)
                    .font(theme::font_small())
                    .color(theme::p().text_low),
            );

            ui.add_space(theme::VIEW_PAD);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, theme::GLYPH_PLAY, "PLAY").clicked() {
                    cx.act(Action::PlayCollection(album.track_ids.clone()));
                }
                if widgets::secondary_button(ui, theme::GLYPH_SHUFFLE, "SHUFFLE").clicked() {
                    cx.act(Action::Play {
                        tracks: album.track_ids.clone(),
                        index: 0,
                        shuffle: true,
                    });
                }
            });
        });
    });
}

fn tracklist(ui: &mut Ui, cx: &mut Ctx, album: &phoebus_core::Album) {
    let lib = cx.lib;
    let selection_id = Id::new(("album-selection", &album.key));
    let selected: Option<u64> = ui.data(|d| d.get_temp(selection_id));
    let mut new_selection: Option<u64> = None;

    ui.spacing_mut().item_spacing.y = 0.0;
    for (index, id) in album.track_ids.iter().enumerate() {
        let Some(track) = lib.track(*id) else {
            continue;
        };
        let (rect, response) = widgets::row(ui, widgets::ROW_H, Sense::click());
        let is_current = cx.now.is_current(*id);
        let is_selected = selected == Some(id.as_u64());
        // Geometric, not `response.hovered()`: the two buttons that sit on top of the row
        // take its hover away, and the row must not flicker back to its idle look the
        // instant the pointer reaches the `▶` it just offered.
        let hovering_row = ui.rect_contains_pointer(rect);
        widgets::row_background(ui, rect, hovering_row || is_selected, false);

        // Leading state column: number → `▶` → equalizer → `⏸` (UI-SPEC v1.2 §Track rows).
        let lead_rect = Rect::from_min_max(
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.left() + widgets::LEAD_W, rect.bottom()),
        );
        let number = track
            .track_no
            .map_or_else(|| "-".to_string(), |n| n.to_string());
        let hit = song_row::lead(
            ui,
            lead_rect,
            response.id.with("lead"),
            song_row::state(is_current, cx.now.playing, hovering_row),
            is_current,
            &number,
        );

        // Title (+ ` — artist` when the track artist differs from the album artist).
        let title_x = lead_rect.right() + widgets::LEAD_GAP;
        widgets::hairline_bottom_from(ui, rect, title_x);
        let tail_left = rect.right() - widgets::tail_w();
        let mut avail = (tail_left - title_x - theme::LCD_PAD).max(1.0);
        let title_color = if is_current {
            theme::p().accent_text
        } else {
            theme::p().text_hi
        };
        let title_g = widgets::truncated(ui, &track.title, theme::font_body(), title_color, avail);
        let title_pos = egui::pos2(title_x, rect.center().y - title_g.size().y * 0.5);
        avail -= title_g.size().x;
        ui.painter().galley(title_pos, title_g.clone(), title_color);
        if track.artist != album.artist && avail > theme::TRACK_NO_W {
            let suffix = format!(" — {}", track.artist);
            let g =
                widgets::truncated(ui, &suffix, theme::font_small(), theme::p().text_mid, avail);
            ui.painter().galley(
                egui::pos2(
                    title_x + title_g.size().x,
                    rect.center().y - g.size().y * 0.5,
                ),
                g,
                theme::p().text_mid,
            );
        }

        let more = song_row::tail(ui, cx, rect, response.id.with("more"), *id, hovering_row);

        let play_here = response.double_clicked() || hit == song_row::Lead::PlayRow;
        if play_here {
            cx.act(Action::Play {
                tracks: album.track_ids.clone(),
                index,
                shuffle: false,
            });
        }
        if hit == song_row::Lead::TogglePlay {
            cx.act(Action::TogglePlay);
        }
        if response.clicked() {
            new_selection = Some(id.as_u64());
        }
        // No `String` clone per row per frame: the menu closures only run while a menu is
        // open, so the artist is resolved in there.
        let track_ids: [TrackId; 1] = [*id];
        response.context_menu(|ui| {
            menus::track_menu(ui, cx, &track_ids, Some(menus::Nav::artist_only(*id)));
        });
        // The same menu, on a LEFT click of `⋯` (UI-SPEC v1.2 §Track rows).
        egui::Popup::menu(&more).show(|ui| {
            menus::track_menu(ui, cx, &track_ids, Some(menus::Nav::artist_only(*id)));
        });
    }

    if let Some(sel) = new_selection {
        ui.data_mut(|d| d.insert_temp(selection_id, sel));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::Artwork;
    use crate::nav::{Fmt, Now};
    use crate::widgets::equalizer;
    use phoebus_core::{Library, Track};
    use std::time::Duration;

    fn library() -> Library {
        let mut tracks = Vec::new();
        for (i, title) in ["Intro", "Native", "Decay"].iter().enumerate() {
            let mut t = Track::new(&format!("HOME/Odyssey/0{} {title}.m4a", i + 1));
            t.title = (*title).to_string();
            t.artist = "HOME".to_string();
            t.album_artist = "HOME".to_string();
            t.album = "Odyssey".to_string();
            t.track_no = Some(i as u32 + 1);
            t.duration = Duration::from_secs(200);
            t.refresh_key();
            tracks.push(t);
        }
        Library::build("/lib", tracks)
    }

    /// Lay the album page out three times with the given now-playing state and return the
    /// repaint the last frame asked for. Three passes because egui's own first frames
    /// (fonts, textures, sizing) request repaints of their own.
    fn repaint_delay(now: Now) -> Duration {
        let lib = library();
        let key = lib.albums().first().cloned().expect("one album");
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut art = Artwork::new();
        let fmt = Fmt::build(&lib);
        let favs = crate::nav::test_favorites();
        let mut actions = Vec::new();
        let mut delay = Duration::ZERO;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib: &lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now,
                    fmt: &fmt,
                    actions: &mut actions,
                };
                show(ui, &mut cx, &key);
            });
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

    /// UI-SPEC v1.2 §Track rows: the equalizer drives `request_repaint_after(≤ 100 ms)`
    /// **only** while one is actually on screen. This is the pacing contract, checked on the
    /// real widget tree rather than by reading the source: a tracklist with nothing playing,
    /// or with its current row paused (frozen bars), must not ask for a fast frame — that is
    /// what would burn a laptop battery on a view nobody is looking at.
    #[test]
    fn only_a_moving_equalizer_asks_for_fast_frames() {
        let lib = library();
        let first = *lib.tracks_sorted().first().expect("a track");

        let playing = repaint_delay(Now {
            track: Some(first),
            playing: true,
        });
        assert!(
            playing <= Duration::from_millis(100),
            "UI-SPEC v1.2 ceiling, got {playing:?}"
        );
        // egui subtracts one predicted frame from every `request_repaint_after` so it does
        // not overshoot (`Context::request_repaint_after`), so what comes back out is
        // `REPAINT - 1/60 s` ≈ 63 ms. Anything much shorter than that would mean something
        // *else* is driving the frame rate.
        assert!(
            playing >= equalizer::REPAINT.saturating_sub(Duration::from_millis(20)),
            "something other than the equalizer is pacing this view: {playing:?}"
        );

        let paused = repaint_delay(Now {
            track: Some(first),
            playing: false,
        });
        assert!(
            paused > Duration::from_secs(1),
            "frozen bars must not animate, got {paused:?}"
        );

        let idle = repaint_delay(Now::default());
        assert!(
            idle > Duration::from_secs(1),
            "nothing playing, nothing to repaint for, got {idle:?}"
        );
    }
}
