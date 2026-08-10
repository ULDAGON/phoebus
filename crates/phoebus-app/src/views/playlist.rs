//! Playlist detail: a mosaic cover built from the playlist's own albums, an inline-
//! renameable title, and a reorderable track list.
//!
//! The one subtlety here is **entry indices**. A playlist stores paths, and the same song
//! may appear twice; `PlaylistStore::remove_at` / `move_entry` therefore address entries,
//! not tracks. Every row carries the index of its entry so that removing the second copy
//! of a song removes the second copy.

use egui::{Key, Rect, Sense, Ui, Vec2};
use phoebus_core::{AlbumKey, Library, Playlist, TrackId};

use crate::artwork::{self, Artwork};
use crate::nav::{self, Action, Ctx};
use crate::theme;
use crate::views;
use crate::widgets::{self, menus, song_picker, song_row};

/// Longest mosaic: four distinct album covers in a 2 × 2 grid.
const MOSAIC_TILES: usize = 4;

/// Why `PLAY` / `SHUFFLE` are dead on an empty playlist.
const EMPTY_TIP: &str = "NOTHING TO PLAY YET";

/// An inline rename in progress.
pub struct Rename {
    /// Playlist being renamed.
    pub id: u64,
    /// The edited text.
    pub text: String,
    /// Set for one frame so the field grabs the keyboard.
    pub focus: bool,
}

/// Resolved rows for one playlist, plus everything the header shows — all recomputed only
/// when the playlist changes.
///
/// The header used to sum durations and pick mosaic covers on every frame, two O(entries)
/// passes with a `HashMap` lookup each; on a 5 000-song playlist that is ~10 000 lookups
/// per frame before a single row is drawn. Both fall out of the pass that resolves the
/// rows, so they are computed there and read back for free.
#[derive(Default)]
pub struct Entries {
    key: Option<(u64, u64, usize)>,
    rows: Vec<(usize, TrackId)>,
    total: std::time::Duration,
    mosaic: Vec<AlbumKey>,
}

impl Entries {
    /// Forget the cache. `modified_at` only has one-second resolution, so a reorder that
    /// keeps the entry count cannot be detected from the playlist alone — the app calls
    /// this after every mutation instead.
    fn invalidate(&mut self) {
        self.key = None;
    }

    /// Resolve the playlist against the library if anything moved since the last frame.
    fn refresh(&mut self, playlist: &Playlist, lib: &Library) {
        let key = (playlist.id, playlist.modified_at, playlist.entries.len());
        if self.key == Some(key) {
            return;
        }
        self.rows.clear();
        self.mosaic.clear();
        self.total = std::time::Duration::ZERO;
        for (index, rel) in playlist.entries.iter().enumerate() {
            let id = TrackId::for_rel_path(rel);
            let Some(track) = lib.track(id) else {
                continue;
            };
            self.rows.push((index, id));
            self.total += track.duration;
            if self.mosaic.len() < MOSAIC_TILES && !self.mosaic.contains(&track.album_key) {
                self.mosaic.push(track.album_key.clone());
            }
        }
        self.key = Some(key);
    }

    /// `(entry index, track id)` for every entry whose file is currently in the library.
    fn rows(&self) -> &[(usize, TrackId)] {
        &self.rows
    }

    /// Total playing time of the resolved rows.
    fn total(&self) -> std::time::Duration {
        self.total
    }

    /// Up to four distinct album covers, in playlist order.
    fn mosaic(&self) -> &[AlbumKey] {
        &self.mosaic
    }
}

/// Playlist-detail state.
#[derive(Default)]
pub struct State {
    /// The rename in flight, if any.
    pub rename: Option<Rename>,
    /// The `ADD SONGS` picker: open flag, filter text and its caches.
    pub picker: song_picker::State,
    entries: Entries,
    selected: Option<usize>,
}

impl State {
    /// Begin renaming `id`, pre-filled with its current name.
    pub fn start_rename(&mut self, id: u64, name: &str) {
        self.rename = Some(Rename {
            id,
            text: name.to_string(),
            focus: true,
        });
    }

    /// Abandon a rename (`Esc`).
    pub fn cancel_rename(&mut self) -> bool {
        self.rename.take().is_some()
    }

    /// Dismiss the add-songs picker (`Esc`, or leaving the page). True if it was up.
    pub fn close_picker(&mut self) -> bool {
        self.picker.close()
    }

    /// Drop the resolved-row cache after a playlist mutation or a rescan.
    ///
    /// The picker's caches go with it — its membership set is exactly what a mutation
    /// changes — but the picker itself stays **open**: the mutation it is reacting to is
    /// usually the `+` the user just clicked, and a popup that closed itself after one song
    /// would defeat the whole point of it (UI-SPEC v1.4 §Add songs).
    pub fn invalidate(&mut self) {
        self.entries.invalidate();
        self.picker.invalidate();
        self.selected = None;
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State, id: u64) {
    // Copy the shared slices out of `cx` first, so they survive the `&mut cx` below.
    let (lib, playlists) = (cx.lib, cx.playlists);
    let Some(playlist) = playlists.iter().find(|p| p.id == id) else {
        // Nothing to add songs to any more. The picker cannot be *drawn* without a playlist,
        // so leaving the flag set would leave `Esc` with an invisible step to eat.
        st.close_picker();
        views::page(ui, |ui| {
            views::heading(ui, "PLAYLIST");
            views::centered_note(ui, &["THIS PLAYLIST IS GONE"]);
        });
        return;
    };
    let State {
        rename,
        picker,
        entries,
        selected,
    } = st;
    entries.refresh(playlist, lib);
    let rows = entries.rows();
    let name = playlist.name.as_str();
    let mut open_picker = false;

    views::page(ui, |ui| {
        header(
            ui,
            cx,
            rename,
            Head {
                id,
                name,
                rows,
                total: entries.total(),
                mosaic: entries.mosaic(),
            },
            &mut open_picker,
        );
        ui.add_space(theme::VIEW_PAD);
        if rows.is_empty() {
            // Names the `+ ADD SONGS` button sitting directly above it (UI-SPEC v1.4
            // §Add songs); right-click still works, and still gets a mention.
            views::centered_note(ui, &["EMPTY — ADD SONGS ABOVE, OR RIGHT-CLICK ANY SONG"]);
            return;
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut selection: Option<usize> = None;
        let current = *selected;
        egui::ScrollArea::vertical()
            .id_salt("playlist-rows")
            .auto_shrink([false, false])
            .show_rows(ui, widgets::ROW_H, rows.len(), |ui, range| {
                for i in range {
                    let Some(&(entry, track)) = rows.get(i) else {
                        break;
                    };
                    let row = song_row::show(ui, cx, track, current == Some(entry));
                    if row.response.clicked() {
                        selection = Some(entry);
                    }
                    if row.response.double_clicked() || row.lead == song_row::Lead::PlayRow {
                        cx.act(Action::Play {
                            tracks: rows.iter().map(|(_, t)| *t).collect(),
                            index: i,
                            shuffle: false,
                        });
                    }
                    if row.lead == song_row::Lead::TogglePlay {
                        cx.act(Action::TogglePlay);
                    }
                    let neighbours = (
                        i.checked_sub(1).and_then(|p| rows.get(p)).map(|r| r.0),
                        rows.get(i + 1).map(|r| r.0),
                    );
                    row.response.context_menu(|ui| {
                        menus::track_menu(ui, cx, &[track], Some(menus::Nav::both(track)));
                        ui.separator();
                        row_menu(ui, cx, id, entry, neighbours);
                    });
                    // The `⋯` button, same menu, left click (UI-SPEC v1.2 §Track rows).
                    egui::Popup::menu(&row.more).show(|ui| {
                        menus::track_menu(ui, cx, &[track], Some(menus::Nav::both(track)));
                        ui.separator();
                        row_menu(ui, cx, id, entry, neighbours);
                    });
                }
            });
        if let Some(entry) = selection {
            *selected = Some(entry);
        }
    });

    // Outside `views::page`, because the picker is its own foreground layer and owes the
    // page's 24 px margin nothing. After it, so the `+` clicks it raises are applied after
    // the page's own — `Phoebus::apply_actions` drains in order.
    if open_picker {
        picker.open();
    }
    song_picker::show(ui.ctx(), cx, picker, playlist);
}

/// `Remove from Playlist`, `Move Up`, `Move Down`.
fn row_menu(
    ui: &mut Ui,
    cx: &mut Ctx,
    playlist: u64,
    entry: usize,
    neighbours: (Option<usize>, Option<usize>),
) {
    if ui.button("Remove from Playlist").clicked() {
        cx.act(Action::RemoveFromPlaylist(playlist, entry));
        ui.close();
    }
    let (above, below) = neighbours;
    if ui
        .add_enabled(above.is_some(), egui::Button::new("Move Up"))
        .clicked()
        && let Some(to) = above
    {
        cx.act(Action::MovePlaylistEntry(playlist, entry, to));
        ui.close();
    }
    if ui
        .add_enabled(below.is_some(), egui::Button::new("Move Down"))
        .clicked()
        && let Some(to) = below
    {
        cx.act(Action::MovePlaylistEntry(playlist, entry, to));
        ui.close();
    }
}

/// Everything the header draws, all of it read straight out of [`Entries`].
struct Head<'a> {
    id: u64,
    name: &'a str,
    rows: &'a [(usize, TrackId)],
    total: std::time::Duration,
    mosaic: &'a [AlbumKey],
}

fn header(
    ui: &mut Ui,
    cx: &mut Ctx,
    rename: &mut Option<Rename>,
    head: Head<'_>,
    open_picker: &mut bool,
) {
    let Head {
        id,
        name,
        rows,
        total,
        mosaic: keys,
    } = head;
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(theme::DETAIL_COVER), Sense::hover());
        mosaic(ui, cx.art, keys, rect);
        ui.add_space(theme::VIEW_PAD);
        ui.vertical(|ui| {
            title(ui, cx, rename, id, name);
            ui.add_space(6.0);

            views::subheading(
                ui,
                &format!("{} SONGS{}{}", rows.len(), theme::SEP, nav::minutes(total)),
            );

            ui.add_space(theme::VIEW_PAD);
            ui.horizontal(|ui| {
                // An empty playlist is the first thing `+ NEW PLAYLIST` shows, and the
                // accent is reserved for "active / primary action" (UI-SPEC §tokens) —
                // so with nothing to play both buttons go quiet and inert rather than
                // painting a live yellow button that swallows the click.
                if rows.is_empty() {
                    widgets::disabled_button(ui, theme::GLYPH_PLAY, "PLAY", EMPTY_TIP);
                    widgets::disabled_button(ui, theme::GLYPH_SHUFFLE, "SHUFFLE", EMPTY_TIP);
                    return;
                }
                if widgets::primary_button(ui, theme::GLYPH_PLAY, "PLAY").clicked() {
                    cx.act(Action::PlayCollection(
                        rows.iter().map(|(_, t)| *t).collect(),
                    ));
                }
                if widgets::secondary_button(ui, theme::GLYPH_SHUFFLE, "SHUFFLE").clicked() {
                    cx.act(Action::Play {
                        tracks: rows.iter().map(|(_, t)| *t).collect(),
                        index: 0,
                        shuffle: true,
                    });
                }
            });
            // A row of its own UNDER the transport pair (UI-SPEC v1.4 §Add songs), because
            // it is the only button here that does not start the music — and because the
            // empty playlist, where it matters most, is exactly the state in which the two
            // above it are dead.
            ui.add_space(theme::CARD_TEXT_GAP);
            ui.horizontal(|ui| {
                *open_picker =
                    widgets::secondary_button(ui, theme::GLYPH_PLUS, "ADD SONGS").clicked();
            });
        });
    });
}

/// The title line: a Heading, or a `TextEdit` in its place while renaming.
fn title(ui: &mut Ui, cx: &mut Ctx, rename: &mut Option<Rename>, id: u64, name: &str) {
    let editing = rename.as_ref().is_some_and(|r| r.id == id);
    if !editing {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(name)
                    .font(theme::font_heading())
                    .color(theme::p().text_hi),
            );
            ui.add_space(theme::CARD_TEXT_GAP);
            // UI-SPEC's `✎` affordance, at last: it used to spell out the word `RENAME`
            // because that codepoint was tofu in the bundled fonts. The tooltip keeps
            // saying it — and keeps naming the F2 shortcut, which a bare pencil cannot.
            let pencil =
                widgets::Icon::new(theme::ICON_INLINE, theme::p().text_low, theme::p().text_hi);
            if widgets::icon_button(ui, theme::GLYPH_RENAME, pencil, "RENAME (F2)").clicked() {
                cx.act(Action::StartRename(id));
            }
        });
        return;
    }

    let Some(state) = rename.as_mut() else {
        return;
    };
    let response = ui.add(
        egui::TextEdit::singleline(&mut state.text)
            .font(egui::TextStyle::Heading)
            .text_color(theme::p().text_hi)
            .desired_width(ui.available_width().min(theme::LCD_MAX_W))
            .margin(egui::Margin::symmetric(6, 4)),
    );
    if state.focus {
        state.focus = false;
        response.request_focus();
    }
    // egui's `TextEdit` surrenders focus on BOTH Enter and Esc, so `lost_focus` alone
    // cannot tell "commit" from "cancel". Esc is left to `Phoebus::escape`, which drops
    // the rename; all this has to do is not commit on the way out.
    let escaped = ui.input(|i| i.key_pressed(Key::Escape));
    let entered = ui.input(|i| i.key_pressed(Key::Enter));
    if (response.lost_focus() || entered) && !escaped {
        let text = state.text.clone();
        *rename = None;
        cx.act(Action::RenamePlaylist(id, text));
    }
}

/// Paint the playlist cover: one cover, a 2 × 2 mosaic, or the `♪` placeholder.
fn mosaic(ui: &Ui, art: &mut Artwork, keys: &[AlbumKey], rect: Rect) {
    match keys.len() {
        0 => artwork::paint_cover(ui, art, None, rect),
        1 => artwork::paint_cover(ui, art, keys.first(), rect),
        n => {
            let half = rect.width() * 0.5;
            for tile in 0..MOSAIC_TILES {
                let origin = egui::pos2(
                    rect.left() + (tile % 2) as f32 * half,
                    rect.top() + (tile / 2) as f32 * half,
                );
                artwork::paint_cover(
                    ui,
                    art,
                    keys.get(tile % n),
                    Rect::from_min_size(origin, Vec2::splat(half)),
                );
            }
            let painter = ui.painter_at(rect);
            painter.line_segment(
                [
                    egui::pos2(rect.center().x, rect.top()),
                    egui::pos2(rect.center().x, rect.bottom()),
                ],
                theme::hairline(),
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left(), rect.center().y),
                    egui::pos2(rect.right(), rect.center().y),
                ],
                theme::hairline(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoebus_core::Track;

    fn lib() -> Library {
        let mut tracks = vec![
            Track::new("HOME/Odyssey/01 Intro.m4a"),
            Track::new("HOME/Odyssey/02 Resonance.m4a"),
            Track::new("Woodkid/S16/01 Goliath.m4a"),
        ];
        for (i, t) in tracks.iter_mut().enumerate() {
            t.duration = std::time::Duration::from_secs(60 * (i as u64 + 1));
        }
        Library::build("/lib", tracks)
    }

    fn playlist(entries: &[&str]) -> Playlist {
        Playlist {
            id: 1,
            name: "Mix".into(),
            entries: entries.iter().map(|e| (*e).to_string()).collect(),
            created_at: 0,
            modified_at: 7,
        }
    }

    #[test]
    fn rows_keep_entry_indices_and_skip_missing_files() {
        let l = lib();
        let p = playlist(&[
            "HOME/Odyssey/01 Intro.m4a",
            "Ghost/Gone/99 Missing.m4a",
            "Woodkid/S16/01 Goliath.m4a",
            "HOME/Odyssey/01 Intro.m4a",
        ]);
        let mut entries = Entries::default();
        entries.refresh(&p, &l);
        let rows = entries.rows().to_vec();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[1].0, 2, "the missing entry keeps its index reserved");
        assert_eq!(rows[2].0, 3, "a duplicate is its own row");
        assert_eq!(rows[0].1, rows[2].1, "…of the same track");
    }

    #[test]
    fn rows_are_cached_until_the_playlist_changes() {
        let l = lib();
        let p = playlist(&["HOME/Odyssey/01 Intro.m4a"]);
        let mut entries = Entries::default();
        entries.refresh(&p, &l);
        assert_eq!(entries.rows().len(), 1);
        let key = entries.key;
        entries.refresh(&p, &l);
        assert_eq!(entries.key, key, "no work on an unchanged playlist");

        let mut changed = p.clone();
        changed.entries.push("Woodkid/S16/01 Goliath.m4a".into());
        changed.modified_at = 8;
        entries.refresh(&changed, &l);
        assert_eq!(entries.rows().len(), 2);
    }

    /// The header reads both of these; neither may be recomputed per frame.
    #[test]
    fn the_header_totals_come_out_of_the_same_pass_as_the_rows() {
        let l = lib();
        let p = playlist(&[
            "HOME/Odyssey/01 Intro.m4a",
            "HOME/Odyssey/02 Resonance.m4a",
            "Woodkid/S16/01 Goliath.m4a",
            "Ghost/Gone/99 Missing.m4a",
        ]);
        let mut entries = Entries::default();
        entries.refresh(&p, &l);
        assert_eq!(
            entries.total(),
            std::time::Duration::from_secs(60 + 120 + 180),
            "unresolved entries contribute nothing"
        );
        assert_eq!(entries.mosaic().len(), 2, "distinct albums only");
        assert!(entries.mosaic().len() <= MOSAIC_TILES);

        entries.invalidate();
        entries.refresh(&p, &l);
        assert_eq!(entries.rows().len(), 3, "recomputing is idempotent");
        assert_eq!(entries.mosaic().len(), 2);
    }
}
