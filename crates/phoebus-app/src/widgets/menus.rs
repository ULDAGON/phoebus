//! The right-click menus. One definition each for "a bunch of tracks" and "an album", so
//! the album grid, the album detail page, the song table and the `⋯` row button all offer
//! the same verbs in the same order — and, through [`styled`], in the same type.

use egui::{TextStyle, Ui};
use phoebus_core::{AlbumKey, TrackId};

use crate::nav::{self, Action, Ctx, View};
use crate::theme;

/// Put one menu body into UI-SPEC v1.2's menu style: 11.5 px items, 8 × 5 px item padding.
///
/// Every popup this module opens goes through here, including each submenu — a submenu is a
/// *new* popup built from the global style, so styling the parent does not reach it. The
/// remaining piece, the 10 px popup margin, cannot be set from inside: egui builds the
/// popup's frame from the style before it hands the body a `Ui`, so that one lives on the
/// global style ([`theme::MENU_MARGIN`], installed by [`theme::apply`]).
///
/// Call sites outside this module (a bespoke menu on a sidebar row) wrap their own body in
/// it; nothing paints a menu item without it.
pub fn styled<R>(ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> R {
    let style = ui.style_mut();
    style
        .text_styles
        .insert(TextStyle::Button, theme::font_menu());
    style.spacing.button_padding = theme::MENU_ITEM_PAD;
    // egui lays menu items out with the ambient item spacing; the padding above is what the
    // spec measures, so the gap between items stays at zero and the rows abut.
    style.spacing.item_spacing.y = 0.0;
    body(ui)
}

/// The `Go to …` half of a track menu.
///
/// It carries the track rather than pre-resolved names on purpose: an egui context-menu
/// closure only runs while the menu is open (egui `popup.rs`), so a row that is merely
/// *drawn* pays nothing — no `String` clone, no artist lookup — for a menu the user is
/// almost never opening.
#[derive(Clone, Copy)]
pub struct Nav {
    /// Track whose artist and album the menu navigates to.
    pub track: TrackId,
    /// Offer `Go to Album`. Off on the album detail page, which already is that page.
    pub album: bool,
}

impl Nav {
    /// Both destinations — the Songs table, the playlist rows, the search results.
    pub fn both(track: TrackId) -> Nav {
        Nav { track, album: true }
    }

    /// Artist only, for a row that already sits on its album's page.
    pub fn artist_only(track: TrackId) -> Nav {
        Nav {
            track,
            album: false,
        }
    }
}

/// `Play`, `Shuffle`, `Favorite`, `Play Next`, `Play Later`, `Add to Playlist ▸` — the
/// album menu.
pub fn album_menu(ui: &mut Ui, cx: &mut Ctx, key: &AlbumKey, tracks: &[TrackId]) {
    styled(ui, |ui| {
        if ui.button("Play").clicked() {
            cx.act(Action::PlayCollection(tracks.to_vec()));
            ui.close();
        }
        if ui.button("Shuffle").clicked() {
            cx.act(Action::Play {
                tracks: tracks.to_vec(),
                index: 0,
                shuffle: true,
            });
            ui.close();
        }
        ui.separator();
        if ui.button(fav_label(cx.favs.is_album(key))).clicked() {
            cx.act(Action::ToggleFavAlbum(key.clone()));
            ui.close();
        }
        ui.separator();
        queue_items(ui, cx, tracks);
        add_to_playlist(ui, cx, tracks);
    });
}

/// What a favourite toggle calls itself, given what it would do.
///
/// One verb, two spellings, and never a checkbox: the heart on the row already says which
/// state the thing is in, so the menu item says what pressing it *does*.
fn fav_label(hearted: bool) -> &'static str {
    if hearted { "Unfavorite" } else { "Favorite" }
}

/// `Favorite`, `Play Next`, `Play Later`, `Add to Playlist ▸`, `Go to Artist`,
/// `Go to Album` — the track-row menu.
///
/// The two `Go to …` items are the *only* way to navigate from a Songs-table row: single
/// click on the ARTIST / ALBUM cells was removed so that double-click-to-play works
/// everywhere on the row (see `views::songs`).
pub fn track_menu(ui: &mut Ui, cx: &mut Ctx, tracks: &[TrackId], nav: Option<Nav>) {
    styled(ui, |ui| {
        favorite_item(ui, cx, tracks);
        queue_items(ui, cx, tracks);
        add_to_playlist(ui, cx, tracks);
        let Some(nav) = nav else {
            return;
        };
        let artist = nav::artist_target(cx.lib, nav.track);
        let album = nav
            .album
            .then(|| cx.lib.track(nav.track).map(|t| t.album_key.clone()))
            .flatten();
        if artist.is_none() && album.is_none() {
            return;
        }
        ui.separator();
        if let Some(artist) = artist
            && ui.button("Go to Artist").clicked()
        {
            cx.act(Action::GoArtist(artist));
            cx.act(Action::Go(View::Artists));
            ui.close();
        }
        if let Some(key) = album
            && ui.button("Go to Album").clicked()
        {
            cx.act(Action::Go(View::Album(key)));
            ui.close();
        }
    });
}

/// `Favorite` / `Unfavorite` at the head of a track menu (UI-SPEC v1.3 §Favorites).
///
/// Nothing at all for an empty selection — there is no such thing as hearting no songs, and
/// a menu that offered it would be a dead row. `all` rather than `any` decides the verb, so
/// a mixed selection reads `Favorite` and hearting it makes the whole thing hearted.
fn favorite_item(ui: &mut Ui, cx: &mut Ctx, tracks: &[TrackId]) {
    if tracks.is_empty() {
        return;
    }
    let hearted = tracks.iter().all(|id| cx.favs.is_track(*id));
    if ui.button(fav_label(hearted)).clicked() {
        for id in tracks {
            if cx.favs.is_track(*id) != !hearted {
                cx.act(Action::ToggleFavTrack(*id));
            }
        }
        ui.close();
    }
    ui.separator();
}

fn queue_items(ui: &mut Ui, cx: &mut Ctx, tracks: &[TrackId]) {
    if ui.button("Play Next").clicked() {
        cx.act(Action::PlayNext(tracks.to_vec()));
        ui.close();
    }
    if ui.button("Play Later").clicked() {
        cx.act(Action::PlayLater(tracks.to_vec()));
        ui.close();
    }
}

/// The `Add to Playlist ▸` submenu: every existing playlist, then `New Playlist…`.
pub fn add_to_playlist(ui: &mut Ui, cx: &mut Ctx, tracks: &[TrackId]) {
    ui.menu_button(format!("Add to Playlist  {}", theme::GLYPH_SUBMENU), |ui| {
        // A submenu is its own popup, built from the global style: the parent menu's `Ui`
        // never reaches it, so it has to be styled again here.
        styled(ui, |ui| {
            let existing: Vec<(u64, String)> = cx
                .playlists
                .iter()
                .map(|p| (p.id, p.name.clone()))
                .collect();
            for (id, name) in existing {
                if ui.button(name).clicked() {
                    cx.act(Action::AddToPlaylist(id, tracks.to_vec()));
                    ui.close();
                }
            }
            if !cx.playlists.is_empty() {
                ui.separator();
            }
            if ui.button("New Playlist…").clicked() {
                cx.act(Action::NewPlaylistWith(tracks.to_vec()));
                ui.close();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UI-SPEC v1.2 §Menus, measured off the wrapper every menu in the app goes through.
    ///
    /// A right-click cannot be synthesised headlessly (API-FACTS §3.7: context menus are
    /// compile-only), so the evidence is the style the body would be laid out with —
    /// which is the whole of what `styled` contributes.
    #[test]
    fn every_menu_body_is_smaller_type_with_more_padding() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(400.0, 300.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let outside = ui.style().clone();
            assert_eq!(
                outside.spacing.button_padding,
                egui::vec2(10.0, 5.0),
                "the ordinary button padding, for contrast"
            );

            styled(ui, |ui| {
                let style = ui.style();
                assert_eq!(
                    style.text_styles.get(&TextStyle::Button).map(|f| f.size),
                    Some(theme::SIZE_MENU),
                    "menu items are one step below Body"
                );
                const { assert!(theme::SIZE_MENU < theme::SIZE_BODY) };
                assert_eq!(style.spacing.button_padding, theme::MENU_ITEM_PAD);
                assert_eq!(style.spacing.button_padding, egui::vec2(8.0, 5.0));
                assert_eq!(
                    style.spacing.item_spacing.y, 0.0,
                    "the item padding is the gap; a second one would double it"
                );
                // The popup's own margin cannot be set from in here — it is already
                // painted — so it is asserted where it lives.
                assert_eq!(
                    style.spacing.menu_margin,
                    egui::Margin::same(theme::MENU_MARGIN)
                );
                assert_eq!(theme::MENU_MARGIN, 10);
            });

            // …and the wrapper's reach ends with its body: the surrounding UI is untouched
            // by anything a menu did to it.
            assert_eq!(
                ui.style().spacing.button_padding,
                theme::MENU_ITEM_PAD,
                "styled() deliberately mutates the Ui it is given, not a clone"
            );
        });
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        out.textures_delta.clear();
    }

    /// The same rules, but measured off a real popup instead of off the style.
    ///
    /// `Popup::menu` forced open is exactly what `Response::context_menu` builds — same
    /// kind, same layout, same `menu_style` — minus the right-click egui will not let a
    /// headless test synthesise. So the geometry this produces *is* the geometry the user
    /// gets: the frame's inner margin and the height of one item.
    #[test]
    fn a_real_menu_popup_comes_out_at_the_spec_geometry() {
        use std::cell::Cell;

        let ctx = egui::Context::default();
        theme::install(&ctx);
        let library = phoebus_core::Library::build("/music", Vec::new());
        let mut art = crate::artwork::Artwork::new();
        let fmt = crate::nav::Fmt::default();
        let favs = crate::nav::test_favorites();
        let mut actions = Vec::new();
        let inner = Cell::new(egui::Rect::NOTHING);

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(600.0, 400.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let anchor = ui.button("anchor");
            let mut cx = crate::nav::Ctx {
                lib: &library,
                art: &mut art,
                playlists: &[],
                favs: &favs,
                now: crate::nav::Now {
                    track: None,
                    playing: false,
                },
                fmt: &fmt,
                actions: &mut actions,
            };
            let popup = egui::Popup::menu(&anchor)
                .open(true)
                .show(|ui| {
                    // Three items and no separator: `Play Next`, `Play Later`,
                    // `Add to Playlist ▸`.
                    track_menu(ui, &mut cx, &[], None);
                    inner.set(ui.min_rect());
                })
                .expect("forced open");

            let frame = popup.response.rect;
            let body = inner.get();
            // The popup's rect is the frame's *outside*, so the hairline egui strokes
            // around it sits between the two measurements.
            let margin = f32::from(theme::MENU_MARGIN) + theme::HAIRLINE_W;
            for (side, got) in [
                ("left", body.left() - frame.left()),
                ("top", body.top() - frame.top()),
                ("right", frame.right() - body.right()),
                ("bottom", frame.bottom() - body.bottom()),
            ] {
                assert!(
                    (got - margin).abs() < 0.5,
                    "{side} margin is {got}, wanted {margin}"
                );
            }

            // The three items abut (the wrapper zeroes the vertical item spacing), so the
            // body divides evenly into them, and each one is tall enough to hold an 11.5 px
            // line inside 5 px of padding — and, as always, to be hit.
            let line = ui.ctx().fonts_mut(|f| f.row_height(&theme::font_menu()));
            let item = body.height() / 3.0;
            assert!(
                item >= line + 2.0 * theme::MENU_ITEM_PAD.y,
                "an item is {item} tall, too short for a {line} line in {:?} of padding",
                theme::MENU_ITEM_PAD
            );
            assert!(item >= theme::HIT_MIN, "an item is only {item} tall");
            assert!(
                item <= line + 4.0 * theme::MENU_ITEM_PAD.y,
                "an item is {item} tall — the items are not abutting"
            );
        });
        out.textures_delta.clear();
    }
}
