//! Playlist detail: a mosaic cover built from the playlist's own albums, an inline-
//! renameable title, and a reorderable track list.
//!
//! The one subtlety here is **entry indices**. A playlist stores paths, and the same song
//! may appear twice; `PlaylistStore::remove_at` / `move_entry` therefore address entries,
//! not tracks. Every row carries the index of its entry so that removing the second copy
//! of a song removes the second copy.
//!
//! ## Reordering
//!
//! A row can be dragged to a new position ([`Drag`]). Three facts shape the implementation:
//!
//! * **Rows are not entries.** Entries whose file is currently missing are skipped by
//!   [`Entries::refresh`] but keep their place in the JSON, so a gap between two *rows* has
//!   to be turned back into a gap between two *entries* before anything is moved
//!   ([`entry_gap`]). Missing entries stay exactly where they are, which is the only answer
//!   that survives the file coming back.
//! * **The queue is not touched.** Playing a playlist snapshots its tracks into the
//!   [`PlayQueue`](phoebus_core::PlayQueue) at press time (`Action::Play` /
//!   `Action::PlayCollection`), so what is playing is a copy and a later edit of the list
//!   cannot desynchronise it. A reorder therefore leaves an in-flight queue alone — exactly
//!   as `Remove from Playlist`, `Move Up` and `Move Down` already do. Dragging the row that
//!   is currently playing is legal and changes nothing about the music.
//! * **A drag that goes nowhere is not a write.** Dropping a row onto its own position, or
//!   outside the list entirely, raises no action at all — see
//!   [`move_target`](phoebus_core::playlists::move_target).

use egui::{Key, PointerButton, Pos2, Rect, Sense, Ui, Vec2};
use phoebus_core::playlists::move_target;
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

/// The foot-of-the-list control, and the empty playlist's one affordance.
const ADD_SONGS: &str = "ADD SONGS";

/// What an empty playlist says. It names the button underneath it first, because that is
/// the one thing on screen; right-clicking a song elsewhere still works and still gets a
/// mention (UI-SPEC v1.4 §Add songs, which said `ABOVE` while the button was in the header).
const EMPTY_NOTE: &str = "EMPTY — ADD SONGS BELOW, OR RIGHT-CLICK ANY SONG";

/// Thickness of the insertion line a drag paints between two rows. Two pixels rather than
/// the app's 1 px `BORDER` hairline on purpose: it is a transient caret, not a divider, and
/// it has to be unmistakable against the hairline it is sitting next to.
const DROP_LINE_H: f32 = 2.0;

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

/// A row that has been picked up and is on its way to a new position.
///
/// Held here rather than in egui's drag-and-drop payload because the source row is
/// virtualized: scroll far enough while dragging and the row that started the gesture is no
/// longer being drawn, so there is no `Response` left to ask. The drag outlives its row.
#[derive(Clone, Copy)]
struct Drag {
    /// **Entry** index of the row that was picked up (see the module doc).
    from: usize,
    /// The gap it would land in, as an index into the resolved row list (`0..=rows.len()`).
    ///
    /// `None` while the pointer is outside the list. A drop there is abandoned rather than
    /// clamped to an end: a drag that left the list is a cancel, not a request to move the
    /// song to the top.
    gap: Option<usize>,
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
    drag: Option<Drag>,
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
        // Row indices are what a drag is expressed in, and the mutation just renumbered
        // them — including, usually, the drop that raised it.
        self.drag = None;
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
        drag,
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
        );
        ui.add_space(theme::VIEW_PAD);
        if rows.is_empty() {
            open_picker |= empty(ui);
            return;
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut selection: Option<usize> = None;
        let current = *selected;
        let picked = drag.map(|d| d.from);
        // Where the pointer is *this* frame, drag or no drag. `pointer_interact_pos` rather
        // than `hover_pos` because it survives the frame the button is released on, which is
        // the frame the drop has to be resolved in.
        let pointer = ui.ctx().pointer_interact_pos();
        // The first visible row, which is all the geometry a uniform-height list needs to
        // turn a pointer position into a gap index.
        let mut anchor: Option<(usize, Rect)> = None;
        let mut started: Option<usize> = None;

        // One extra "row" past the end: the `+ ADD SONGS` foot. Inside the scroller and on
        // the list's own 40 px rhythm, so it scrolls with the songs and reads as the last
        // line of the list rather than as a fourth button in the header.
        let out = egui::ScrollArea::vertical()
            .id_salt("playlist-rows")
            .auto_shrink([false, false])
            .show_rows(ui, widgets::ROW_H, rows.len() + 1, |ui, range| {
                for i in range {
                    let Some(&(entry, track)) = rows.get(i) else {
                        open_picker |= add_songs_row(ui);
                        break;
                    };
                    let row = song_row::draggable(
                        ui,
                        cx,
                        track,
                        current == Some(entry) || picked == Some(entry),
                    );
                    if anchor.is_none() {
                        anchor = Some((i, row.response.rect));
                    }
                    // Primary button only: a right-press must stay a context menu, and a
                    // middle-press must not silently reorder anything.
                    if row.response.drag_started_by(PointerButton::Primary) {
                        started = Some(entry);
                    }
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

        if let Some(from) = started {
            *drag = Some(Drag { from, gap: None });
        }
        let gap = pointer
            .filter(|p| out.inner_rect.contains(*p))
            .zip(anchor)
            .map(|(p, (first, rect))| gap_at(p, first, rect, rows.len()));
        if let Some(state) = drag.as_mut() {
            state.gap = gap;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(gap) = gap
                && let Some((first, rect)) = anchor
            {
                drop_line(ui, out.inner_rect, gap_y(gap, first, rect), rect.x_range());
            }
        }
        // Released anywhere — the pointer state is global, so a release over the sidebar or
        // off the window still ends the gesture here rather than leaving a drag running to
        // reorder on the next unrelated click.
        //
        // The PRIMARY button, though, and only it: it is the button the drag was picked up
        // with, so it is the button that says where the row lands. `any_released` let a
        // stray right- or middle-click part-way through a drag commit the drop at whatever
        // gap the pointer happened to be over, which is the same reorder-by-accident the
        // press side is guarded against above.
        if ui.input(|i| i.pointer.button_released(PointerButton::Primary))
            && let Some(state) = drag.take()
            && let Some(gap) = state.gap
            && let Some(to) = drop_move(rows, playlist.entries.len(), state.from, gap)
        {
            cx.act(Action::MovePlaylistEntry(id, state.from, to));
        }
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

/// Which gap the pointer is naming, derived from the first visible row alone — every row is
/// exactly [`widgets::ROW_H`] tall, so one rect fixes the whole column and the rows that
/// were virtualized away never have to be measured.
///
/// The rounding is what makes it a *gap* rather than a row: the top half of a row means
/// "above it", the bottom half "below it". Clamped to `0..=rows`, so both the `+ ADD SONGS`
/// foot row and the empty space under a short list mean "at the end".
fn gap_at(pointer: Pos2, first: usize, rect: Rect, rows: usize) -> usize {
    let offset = (pointer.y - rect.top()) / widgets::ROW_H;
    (first as f32 + offset).round().clamp(0.0, rows as f32) as usize
}

/// The y of a gap, in the geometry [`gap_at`] reads. Its exact inverse.
fn gap_y(gap: usize, first: usize, rect: Rect) -> f32 {
    rect.top() + (gap as f32 - first as f32) * widgets::ROW_H
}

/// Turn a gap between two *rows* into the gap between two *entries* it means.
///
/// Gap `n` is "immediately above row `n`", hence immediately above that row's entry; past
/// the last row it is past the last entry. Entries whose file is missing are never named by
/// a gap and so never move — they keep their place in the JSON, which is what lets them
/// reappear where they belong when the file does.
fn entry_gap(rows: &[(usize, TrackId)], gap: usize, entries: usize) -> usize {
    rows.get(gap).map_or(entries, |&(entry, _)| entry)
}

/// Turn a completed drop into the [`Action::MovePlaylistEntry`] it asks for, or `None` when
/// it asks for nothing.
///
/// Two no-op tests, and they are not the same test twice. The first is in ROW space: the two
/// gaps either side of the dragged row are the order the user is already looking at, and a
/// playlist with an unresolvable entry between them would otherwise turn "put it back where
/// it was" into a real — and completely invisible — entry move. The second is
/// [`move_target`]'s, in ENTRY space, which is what keeps `playlists.json` and `modified_at`
/// untouched by a drag that went nowhere.
fn drop_move(rows: &[(usize, TrackId)], entries: usize, from: usize, gap: usize) -> Option<usize> {
    let row = rows.iter().position(|&(entry, _)| entry == from)?;
    if gap == row || gap == row + 1 {
        return None;
    }
    move_target(from, entry_gap(rows, gap, entries))
}

/// The insertion caret: an accent line across the list at `y`, clipped to the scroll
/// viewport so a gap that has scrolled just out of sight cannot paint over the header.
fn drop_line(ui: &Ui, clip: Rect, y: f32, x: egui::Rangef) {
    let line = Rect::from_min_max(
        egui::pos2(x.min, (y - DROP_LINE_H * 0.5).round()),
        egui::pos2(x.max, (y + DROP_LINE_H * 0.5).round()),
    );
    ui.painter().with_clip_rect(clip).rect_filled(
        line,
        egui::CornerRadius::ZERO,
        theme::p().accent,
    );
}

/// `+ ADD SONGS` painted with its top-left at `pos` and hit-tested on its own box. Returns
/// true when it was clicked.
///
/// Accent TEXT and nothing else: no fill, no hairline, no frame. The accent already means
/// "primary action" (UI-SPEC §Design tokens), and a frame here would put a second button
/// shape in front of the filled `PLAY` in the header.
///
/// The target is the text's box rather than the whole row it sits on, for two reasons: the
/// rest of that row is the reorder gesture's drop zone, and a full-width target would make
/// an aimless click at the far right of an empty page open a modal.
fn add_songs_at(ui: &mut Ui, pos: Pos2, galley: std::sync::Arc<egui::Galley>, salt: &str) -> bool {
    let size = Vec2::new(galley.size().x, galley.size().y.max(theme::HIT_MIN));
    let hit = Rect::from_min_size(
        egui::pos2(pos.x, pos.y + (galley.size().y - size.y) * 0.5),
        size,
    );
    let response = ui
        .interact(hit, ui.id().with(salt), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let color = theme::hover_color(
        response.hovered(),
        theme::p().accent_text,
        theme::p().accent_text_dim,
    );
    ui.painter().galley(pos, galley, color);
    response.clicked()
}

/// The label both placements share: the words and nothing else.
///
/// No leading `+` either. The control used to be `+ ADD SONGS` on a `SECONDARY` button, and
/// text-only means the glyph goes with the frame — a lone icon floating at the foot of a
/// list of songs reads as a control that is still trying to be a button. `Color32::PLACEHOLDER`
/// is what lets [`add_songs_at`] recolour the same galley on hover.
fn add_songs_galley(ui: &Ui) -> std::sync::Arc<egui::Galley> {
    widgets::icon_text(ui, "", 0.0, ADD_SONGS, theme::font_body())
}

/// The foot of the song list: one more 40 px row, aligned to the title column above it.
///
/// UI-SPEC v1.4 §Add songs put this in the header as a SECONDARY button. It now sits after
/// the last song, inside the same scroller and on the same rhythm, so it reads as the last
/// line of the list — "…and one more" — rather than as a third transport control. It opens
/// exactly the same picker.
fn add_songs_row(ui: &mut Ui) -> bool {
    let (rect, _) = widgets::row(ui, widgets::ROW_H, Sense::hover());
    let galley = add_songs_galley(ui);
    let pos = egui::pos2(
        song_row::title_x(rect),
        rect.center().y - galley.size().y * 0.5,
    );
    add_songs_at(ui, pos, galley, "add-songs-foot")
}

/// The empty playlist: the note, and the same `+ ADD SONGS` centred under it.
///
/// The header's `PLAY` / `SHUFFLE` are the disabled pair in this state, so this is the only
/// live control on the page — which is the whole reason the affordance has to survive the
/// list having no last row to sit after.
fn empty(ui: &mut Ui) -> bool {
    let rect = ui.available_rect_before_wrap();
    let note = widgets::truncated(
        ui,
        &widgets::spaced(EMPTY_NOTE),
        theme::font_body(),
        theme::p().text_low,
        (rect.width() - 2.0 * theme::VIEW_PAD).max(1.0),
    );
    let link = add_songs_galley(ui);
    let total = note.size().y + theme::VIEW_PAD + link.size().y.max(theme::HIT_MIN);
    let top = rect.center().y - total * 0.5;
    let note_h = note.size().y;
    ui.painter().galley(
        egui::pos2(rect.center().x - note.size().x * 0.5, top),
        note,
        theme::p().text_low,
    );
    let pos = egui::pos2(
        rect.center().x - link.size().x * 0.5,
        top + note_h + theme::VIEW_PAD,
    );
    let clicked = add_songs_at(ui, pos, link, "add-songs-empty");
    ui.allocate_space(rect.size());
    clicked
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

fn header(ui: &mut Ui, cx: &mut Ctx, rename: &mut Option<Rename>, head: Head<'_>) {
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
            // `+ ADD SONGS` used to be a SECONDARY button on a row of its own here
            // (UI-SPEC v1.4 §Add songs). It is now the foot of the song list — see
            // [`add_songs_row`] — so the header is back to the two transport buttons the
            // album page has, and the control sits where the songs it adds will land.
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

    /// Where the song list starts in the test window: the page's own margin, the header's
    /// cover, the gap egui's vertical layout leaves after it, and the space the view adds
    /// under the header.
    ///
    /// Derived from the style rather than written down, because every pointer test below
    /// aims at a row through it and a silently stale offset would move them all onto the
    /// wrong rows without failing anything for the right reason.
    fn list_top() -> f32 {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        theme::VIEW_PAD
            + theme::DETAIL_COVER
            + ctx.style_of(egui::Theme::Dark).spacing.item_spacing.y
            + theme::VIEW_PAD
    }

    /// Centre of row `n`, in window coordinates.
    fn row_y(n: usize) -> f32 {
        list_top() + widgets::ROW_H * (n as f32 + 0.5)
    }

    /// x to aim at: past the leading column and the cover, well short of the heart, the
    /// duration and the `⋯` — the part of a row that is only the row.
    const ROW_X: f32 = 400.0;

    /// Everything one [`drive`] run reported.
    struct Run {
        /// Actions the view raised, over every frame.
        actions: Vec<Action>,
        /// The view state the run left behind.
        state: State,
        /// Shapes painted on the LAST frame — the only way to see the insertion caret, which
        /// exists solely while a mouse button is held down and so cannot be photographed.
        shapes: Vec<egui::epaint::ClippedShape>,
    }

    /// Drive the real view with a synthetic pointer, one frame per step, and report
    /// everything it did.
    ///
    /// Each step is `(y, button)` where `button` is `Some(true)` to press, `Some(false)` to
    /// release and `None` to only move. A warm-up frame goes in front: egui can only deliver
    /// a press to a widget it registered on an earlier frame.
    fn drive(steps: &[(f32, Option<bool>)]) -> Run {
        drive_over(
            &[
                "HOME/Odyssey/01 Intro.m4a",
                "HOME/Odyssey/02 Resonance.m4a",
                "Woodkid/S16/01 Goliath.m4a",
            ],
            ROW_X,
            steps,
        )
    }

    /// [`drive`], over a playlist of the caller's choosing, aiming at `x`. Every step is on
    /// the primary button, which is the only one the reorder gesture is spelled with.
    fn drive_over(entries: &[&str], x: f32, steps: &[(f32, Option<bool>)]) -> Run {
        let steps: Vec<Step> = steps
            .iter()
            .map(|&(y, press)| (y, press.map(|down| (PointerButton::Primary, down))))
            .collect();
        drive_buttons(entries, x, &steps)
    }

    /// One frame of a [`drive_buttons`] run: a pointer y, and optionally a button to press
    /// (`true`) or release (`false`) there.
    type Step = (f32, Option<(PointerButton, bool)>);

    /// [`drive_over`] with the button spelled out, for the gestures that mix buttons — a
    /// stray right-click part-way through a left-button drag.
    fn drive_buttons(entries: &[&str], x: f32, steps: &[Step]) -> Run {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let lib = lib();
        let pls = vec![playlist(entries)];
        let fmt = crate::nav::Fmt::build(&lib);
        let favs = crate::nav::test_favorites();
        let mut art = Artwork::new();
        let mut st = State::default();
        let mut actions: Vec<Action> = Vec::new();
        let mut shapes = Vec::new();

        let mut frames: Vec<Step> = vec![(row_y(0), None)];
        frames.extend_from_slice(steps);
        for (y, button) in frames {
            let pos = egui::pos2(x, y);
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(pos));
            if let Some((button, pressed)) = button {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib: &lib,
                    art: &mut art,
                    playlists: &pls,
                    favs: &favs,
                    now: crate::nav::Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                show(ui, &mut cx, &mut st, 1);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
            shapes = out.shapes;
        }
        Run {
            actions,
            state: st,
            shapes,
        }
    }

    /// The control the header used to carry: a click on the foot of the list opens the
    /// picker, and only there — the rest of that row is the reorder gesture's drop zone.
    #[test]
    fn the_add_songs_foot_row_opens_the_picker() {
        // Three songs, so the foot row is the fourth row down, and the label starts at the
        // title column the rows above it share.
        let entries = &[
            "HOME/Odyssey/01 Intro.m4a",
            "HOME/Odyssey/02 Resonance.m4a",
            "Woodkid/S16/01 Goliath.m4a",
        ];
        let on_label = theme::VIEW_PAD
            + widgets::LEAD_W
            + widgets::LEAD_GAP
            + widgets::ROW_ART
            + theme::LCD_PAD
            + 2.0
            + 8.0;
        let press = |x: f32, y: f32| drive_over(entries, x, &[(y, Some(true)), (y, Some(false))]);

        let Run {
            actions, state: st, ..
        } = press(on_label, row_y(3));
        assert!(st.picker.is_open(), "the foot row did not open the picker");
        assert!(actions.is_empty(), "…and it plays nothing: {actions:?}");

        // The far right of the same row is not a target: an aimless click out there must not
        // put a modal on screen.
        let st = press(1000.0, row_y(3)).state;
        assert!(!st.picker.is_open(), "the whole row is a button");
        // Nor is a song row.
        let st = press(on_label, row_y(1)).state;
        assert!(!st.picker.is_open());
    }

    /// The empty playlist's copy of the same control, centred under its note.
    ///
    /// Found by pressing down the middle of the page rather than by asserting a coordinate:
    /// the note and the link are centred together, so exactly where the link lands follows
    /// from the body font's line height, which is not this test's business. What is its
    /// business is that the affordance is reachable at all — an empty playlist with no way
    /// to add songs is the one broken state this whole control exists to prevent.
    #[test]
    fn the_empty_playlist_offers_the_same_control() {
        let found = (list_top() as i32..790).step_by(4).find(|y| {
            let y = *y as f32;
            drive_over(&[], 640.0, &[(y, Some(true)), (y, Some(false))])
                .state
                .picker
                .is_open()
        });
        assert!(
            found.is_some(),
            "nothing down the middle of the empty page opened the picker"
        );
    }

    /// The reorder gesture on the real widget tree: press the first row, drag past the last,
    /// let go. Nothing here re-derives the arithmetic — it presses a synthetic mouse button
    /// and reads what the view asked the app to do.
    #[test]
    fn dragging_a_row_past_the_last_one_moves_it_there() {
        let Run {
            actions, state: st, ..
        } = drive(&[
            (row_y(0), Some(true)),
            // Well past egui's click/drag threshold, into the bottom half of row 2.
            (row_y(2) + widgets::ROW_H * 0.3, None),
            (row_y(2) + widgets::ROW_H * 0.3, Some(false)),
        ]);
        assert!(
            matches!(actions.as_slice(), [Action::MovePlaylistEntry(1, 0, 2)]),
            "the drag raised {actions:?}, not a move of entry 0 to index 2"
        );
        assert!(st.drag.is_none(), "the drag must not outlive the release");
    }

    /// …and upwards, which is the direction that shares its gap index with its move index.
    #[test]
    fn dragging_a_row_to_the_top_moves_it_there() {
        let actions = drive(&[
            (row_y(2), Some(true)),
            (list_top() + 2.0, None),
            (list_top() + 2.0, Some(false)),
        ])
        .actions;
        assert!(
            matches!(actions.as_slice(), [Action::MovePlaylistEntry(1, 2, 0)]),
            "the drag raised {actions:?}, not a move of entry 2 to the top"
        );
    }

    /// A drag is picked up with the primary button, so only the primary button may put it
    /// down. A right- or middle-click part-way through used to end the gesture at the
    /// waypoint the pointer happened to be over — a reorder the user never asked for, and
    /// one that outlived the still-held drag it stole.
    #[test]
    fn a_stray_click_mid_drag_does_not_drop_the_row_where_it_is() {
        const ENTRIES: [&str; 3] = [
            "HOME/Odyssey/01 Intro.m4a",
            "HOME/Odyssey/02 Resonance.m4a",
            "Woodkid/S16/01 Goliath.m4a",
        ];
        // The waypoint: over row 1, which is the wrong answer (`MovePlaylistEntry(1, 0, 1)`).
        let waypoint = row_y(1);
        // The destination: past the last row, which is what the completed drag means.
        let target = row_y(2) + widgets::ROW_H * 0.3;
        for stray in [PointerButton::Secondary, PointerButton::Middle] {
            let Run {
                actions, state: st, ..
            } = drive_buttons(
                &ENTRIES,
                ROW_X,
                &[
                    (row_y(0), Some((PointerButton::Primary, true))),
                    (waypoint, None),
                    (waypoint, Some((stray, true))),
                    (waypoint, Some((stray, false))),
                    (target, None),
                    (target, Some((PointerButton::Primary, false))),
                ],
            );
            assert!(
                matches!(actions.as_slice(), [Action::MovePlaylistEntry(1, 0, 2)]),
                "a {stray:?} click mid-drag raised {actions:?}, not the one move the \
                 completed drag asks for"
            );
            assert!(st.drag.is_none(), "the drag must not outlive the release");
        }
    }

    /// The insertion caret, in the paint list.
    ///
    /// This is the one piece of the feature that cannot be photographed — it only exists
    /// while a mouse button is held down, and `--shot` cannot hold one — so it is checked
    /// where it is actually produced: an accent-filled rectangle exactly [`DROP_LINE_H`]
    /// tall, at the gap the drop would use, painted only while a drag is in flight.
    #[test]
    fn a_drag_paints_an_accent_line_at_the_gap_it_would_drop_into() {
        // Held over the top half of row 2, so the caret belongs on the seam above it.
        let held = row_y(2) - widgets::ROW_H * 0.3;
        let mid = drive(&[(row_y(0), Some(true)), (held, None)]);
        assert!(
            mid.state.drag.is_some(),
            "the drag was not in flight on the frame under test"
        );
        let caret = accent_lines(&mid.shapes);
        assert_eq!(
            caret.len(),
            1,
            "expected exactly one insertion caret, found {caret:?}"
        );
        assert!(
            (caret[0] - (list_top() + widgets::ROW_H * 2.0)).abs() < 1.0,
            "the caret is at y = {}, not on the seam above row 2",
            caret[0]
        );

        // And with nothing being dragged there is no caret at all: a line that lingered
        // would read as a divider in the accent, which the design reserves for the music.
        let idle = drive(&[(row_y(1), None)]);
        assert!(accent_lines(&idle.shapes).is_empty());
    }

    /// The y of every accent-filled rectangle exactly [`DROP_LINE_H`] tall.
    fn accent_lines(shapes: &[egui::epaint::ClippedShape]) -> Vec<f32> {
        shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect)
                    if rect.fill == theme::p().accent
                        && (rect.rect.height() - DROP_LINE_H).abs() < 0.01 =>
                {
                    Some(rect.rect.center().y)
                }
                _ => None,
            })
            .collect()
    }

    /// The threshold, from the other side: a press and release in the same place is a CLICK,
    /// and a click selects. If drag detection ate it, the row would be unselectable and the
    /// double-click that plays it would never assemble.
    #[test]
    fn a_click_is_not_swallowed_by_drag_detection() {
        let Run {
            actions, state: st, ..
        } = drive(&[(row_y(1), Some(true)), (row_y(1), Some(false))]);
        assert!(
            actions.is_empty(),
            "a single click must select and nothing else, but raised {actions:?}"
        );
        assert_eq!(st.selected, Some(1), "the click did not select its row");
        assert!(st.drag.is_none());
    }

    /// A drag released outside the list is a cancel, not a move to the nearest end.
    #[test]
    fn a_drop_outside_the_list_reorders_nothing() {
        let Run {
            actions, state: st, ..
        } = drive(&[
            (row_y(0), Some(true)),
            // Up into the header, which is not the drop zone.
            (list_top() - 8.0, None),
            (list_top() - 8.0, Some(false)),
        ]);
        assert!(
            actions.is_empty(),
            "a drop over the header raised {actions:?}"
        );
        assert!(st.drag.is_none(), "…and still ended the gesture");
    }

    /// Dropping a row back where it came from writes nothing at all — no action, so no
    /// `move_entry`, so no `playlists.json` and no `modified_at`.
    #[test]
    fn dropping_a_row_onto_itself_raises_nothing() {
        for target in [
            row_y(1) - widgets::ROW_H * 0.3,
            row_y(1) + widgets::ROW_H * 0.3,
        ] {
            let actions = drive(&[
                (row_y(1), Some(true)),
                (row_y(1) + 40.0, None),
                (target, None),
                (target, Some(false)),
            ])
            .actions;
            assert!(
                actions.is_empty(),
                "dropping onto itself raised {actions:?}"
            );
        }
    }

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

    /// The pointer→gap rule, which is the whole feel of the gesture: the top half of a row
    /// means "above it", the bottom half "below it", and everything past the last row means
    /// "at the end".
    #[test]
    fn the_pointer_names_the_gap_it_is_nearest_to() {
        let h = widgets::ROW_H;
        // Three visible rows starting at row 2 of a 6-row list, first one at y = 100.
        let first = Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, h));
        let gap = |y: f32| gap_at(egui::pos2(10.0, y), 2, first, 6);
        assert_eq!(gap(100.0), 2, "the very top of row 2 is the gap above it");
        assert_eq!(gap(100.0 + h * 0.4), 2);
        assert_eq!(gap(100.0 + h * 0.6), 3, "past the middle: the gap below");
        assert_eq!(gap(100.0 + h), 3, "the seam between rows 2 and 3");
        assert_eq!(gap(100.0 + h * 3.5), 6, "past the last visible row");
        // Both ends clamp: above the list is the top gap, far below is the end gap, and
        // neither may run off the row list (the `+ ADD SONGS` foot row lives past it).
        assert_eq!(gap(-9000.0), 0);
        assert_eq!(gap(9000.0), 6);
    }

    /// `gap_y` is `gap_at`'s inverse — the caret must be painted exactly where the drop is
    /// computed, or the line lies about where the song is going.
    #[test]
    fn the_caret_is_painted_where_the_drop_is_computed() {
        let h = widgets::ROW_H;
        let first = Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, h));
        for gap in 2..=6 {
            let y = gap_y(gap, 2, first);
            assert_eq!(gap_at(egui::pos2(10.0, y), 2, first, 6), gap, "gap {gap}");
        }
    }

    /// Row gaps are not entry gaps, and a playlist whose files have partly gone missing is
    /// the case that tells them apart. Nothing here may disturb the missing entries.
    #[test]
    fn a_row_gap_becomes_the_entry_gap_it_means() {
        // entries: [0 A, 1 MISSING, 2 B, 3 MISSING] → rows: A at 0, B at 2.
        let rows = [(0usize, TrackId(1)), (2usize, TrackId(2))];
        assert_eq!(entry_gap(&rows, 0, 4), 0, "above A is above entry 0");
        assert_eq!(
            entry_gap(&rows, 1, 4),
            2,
            "between A and B is above entry 2"
        );
        assert_eq!(entry_gap(&rows, 2, 4), 4, "past B is past every entry");
        assert!(
            entry_gap(&[], 0, 3) == 3,
            "no rows: the only gap is the end"
        );
    }

    /// The two no-op cases the gesture has, and the one that only a partly-missing playlist
    /// can produce: with a missing entry sitting between two rows, dropping a row into the
    /// gap "below itself" names a different ENTRY index but the very same visible order —
    /// and must still write nothing.
    #[test]
    fn a_drop_that_changes_nothing_visible_raises_nothing() {
        let rows = [(0usize, TrackId(1)), (2usize, TrackId(2))];
        // Row 0 (entry 0), dropped either side of itself.
        assert_eq!(drop_move(&rows, 4, 0, 0), None, "above itself");
        assert_eq!(
            drop_move(&rows, 4, 0, 1),
            None,
            "below itself — entry gap 2, which is NOT entry 1, and would have moved it"
        );
        // Row 1 (entry 2), dropped either side of itself.
        assert_eq!(drop_move(&rows, 4, 2, 1), None);
        assert_eq!(drop_move(&rows, 4, 2, 2), None);
        // A single-row playlist has two gaps and both are its own.
        let one = [(0usize, TrackId(1))];
        assert_eq!(drop_move(&one, 1, 0, 0), None);
        assert_eq!(drop_move(&one, 1, 0, 1), None);
        // A drag whose row is no longer in the list at all (a rescan lost the file).
        assert_eq!(drop_move(&rows, 4, 1, 0), None);
        // …and the moves that are real still come through, in ENTRY indices.
        assert_eq!(drop_move(&rows, 4, 0, 2), Some(3), "A to the end");
        assert_eq!(drop_move(&rows, 4, 2, 0), Some(0), "B to the top");
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
