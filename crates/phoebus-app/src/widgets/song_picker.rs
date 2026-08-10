//! The add-songs picker: a centred modal listing the whole library, one `+` per row
//! (UI-SPEC v1.4 §Add songs).
//!
//! This is the **first and only modal in Phoebus**, and it is here under protest. Every
//! other question the app asks is answered in place — the sidebar's delete confirmation
//! rewrites the row it is asking about (`app::confirm_row`, "no modal, no dialog"), a rename
//! swaps the title for a field. Adding songs cannot be done that way: the answer is a
//! *browse* over a few thousand rows with its own filter, which is a second view, and a
//! second view has to go somewhere. A pop-out over the playlist is the smallest thing that
//! can hold one.
//!
//! What keeps it in the house style: no chrome of its own beyond the hairline and 2 px
//! corner every card already has, no shadow (the global style has none), no title bar, no
//! drag, no resize, no animation. It is a rectangle of `BG1` with the view dimmed behind it.
//!
//! The rows are deliberately **not** [`song_row::show`](super::song_row::show). That row is
//! the five list views' shared shape — leading play affordance, cover, title over artist,
//! album, heart, duration, `⋯` — and every one of those pieces is a way to do something to
//! a track. This surface does exactly one thing, so its row is `+` / `✓`, cover, title,
//! artist, length, and nothing is clickable but the `+`. The metrics are still the shared
//! ones from [`widgets`], so a picker row lines up with the playlist rows underneath it.

use std::collections::HashSet;

use egui::{Align2, Id, Rect, Sense, Ui, Vec2};
use phoebus_core::{Library, Playlist, TrackId};

use crate::artwork;
use crate::nav::{Action, Ctx};
use crate::theme;
use crate::widgets;

/// egui id of the modal's area. One picker, one id — there is never a second.
const MODAL_ID: &str = "add-songs";
/// Scroller id (the app has several `show_rows` scrollers and they must not share state).
const ROWS_ID: &str = "add-songs-rows";
/// Hint in the filter field.
const HINT: &str = "FILTER BY TITLE OR ARTIST";
/// Shown instead of the list when the filter matches nothing.
const NO_MATCH: &str = "NO SONGS MATCH";
/// Shown instead of the list when there is nothing to add at all.
const NO_LIBRARY: &str = "THE LIBRARY IS EMPTY";

/// Whether the picker is up, what is typed in it, and the two caches a row reads.
///
/// It lives in [`crate::views::ViewState`] rather than in egui's temp memory for the reason
/// the module doc there gives: the filtered order is a `Vec` of (potentially) every track in
/// the library, which is far too big to clone in and out of `ui.data()` once per frame. It
/// is deliberately **not** persisted — a modal that came back on the next launch would be a
/// bug, not a feature.
#[derive(Default)]
pub struct State {
    /// True while the modal is on screen.
    open: bool,
    /// The filter text, as typed.
    query: String,
    /// Set for one frame after [`State::open`] so the filter field takes the keyboard.
    focus: bool,
    /// Hits for `rows_key`, in `tracks_sorted` order.
    rows: Vec<TrackId>,
    /// The query `rows` was computed for. `None` = recompute.
    rows_key: Option<String>,
    /// Everything the playlist already has, for the `+` / `✓` decision.
    member: HashSet<TrackId>,
    /// `(playlist id, modified_at, entries.len())` `member` was computed for.
    ///
    /// The same key [`crate::views::playlist::Entries`] uses, and it has the same blind
    /// spot: `modified_at` has one-second resolution, so a change that keeps the entry count
    /// is invisible. Nothing this popup does keeps the count — it only appends — and
    /// [`State::invalidate`] is called after every playlist mutation anyway.
    member_key: Option<(u64, u64, usize)>,
}

impl State {
    /// Put the picker up, empty and focused.
    pub fn open(&mut self) {
        self.open = true;
        self.focus = true;
        self.query.clear();
        self.invalidate();
    }

    /// Take the picker down. Returns true if it was up — which is what makes it a step
    /// `Phoebus::escape` can stop on.
    pub fn close(&mut self) -> bool {
        self.focus = false;
        std::mem::replace(&mut self.open, false)
    }

    /// True while the modal is up. Read by the tests that drive the playlist page.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Drop both caches, keeping the modal up and the query as typed.
    ///
    /// Called after every playlist mutation (the `+` the user just clicked) and after every
    /// rescan (which changes what the ids mean).
    pub fn invalidate(&mut self) {
        self.rows_key = None;
        self.member_key = None;
    }

    /// Recompute whichever cache is stale. Both are keyed on their inputs, so a frame that
    /// changed nothing costs one string compare and one tuple compare.
    fn refresh(&mut self, lib: &Library, playlist: &Playlist) {
        if self.rows_key.as_deref() != Some(self.query.as_str()) {
            self.rows = phoebus_core::filter_tracks(lib, &self.query);
            self.rows_key = Some(self.query.clone());
        }
        let key = (playlist.id, playlist.modified_at, playlist.entries.len());
        if self.member_key != Some(key) {
            self.member = playlist.entry_ids().collect();
            self.member_key = Some(key);
        }
    }
}

/// Draw the picker over `playlist`, if it is open. A no-op otherwise.
///
/// Takes the `Context` and not a `Ui`, because the modal is its own foreground layer: the
/// surrounding `Ui` decides nothing about where it lands. Call position still matters for
/// one thing, so call it from the view **after** the page — it decides the order actions
/// come out in, and the picker's `+` must be applied after the page's own clicks.
pub fn show(ctx: &egui::Context, cx: &mut Ctx, st: &mut State, playlist: &Playlist) {
    if !st.open {
        return;
    }
    // Copied out of `cx` first (they are shared refs into the app, not borrows of `cx`), so
    // the closures below can still take `&mut Ctx` for `cx.art` and `cx.act`.
    let lib = cx.lib;
    st.refresh(lib, playlist);

    let bounds = ctx.content_rect();
    let width = (bounds.width() * theme::MODAL_W_FRAC).min(theme::MODAL_MAX_W);
    let height = bounds.height() * theme::MODAL_H_FRAC;
    let frame = egui::Frame::new()
        .fill(theme::p().bg1)
        .stroke(theme::hairline())
        .corner_radius(theme::corner())
        .inner_margin(egui::Margin::same(theme::VIEW_PAD as i8));

    let mut close = false;
    let clicked_outside = {
        let State {
            query,
            focus,
            rows,
            member,
            ..
        } = &mut *st;
        let rows: &[TrackId] = rows;
        let id = playlist.id;
        let name = playlist.name.as_str();

        let outcome = egui::Modal::new(Id::new(MODAL_ID))
            .backdrop_color(theme::p().scrim)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.set_height(height);
                close |= header(ui, name);
                ui.add_space(theme::CARD_TEXT_GAP);
                filter_field(ui, query, focus);
                ui.add_space(theme::CARD_TEXT_GAP);
                list(ui, cx, rows, member, id, lib.is_empty());
            });
        // NOT `ModalResponse::should_close`: its `Esc` arm consumes the key, and `Esc` in
        // this app is one ordered unwind owned by `Phoebus::escape` (rename, then this, then
        // the drawer, then search) — a modal that swallowed the key would take that ordering
        // apart. The backdrop click is the half egui can answer on its own.
        outcome.backdrop_response.clicked()
    };
    if close || clicked_outside {
        st.close();
    }
}

/// `ADD SONGS`, the playlist's own name under it, and the `✕`. Returns true if `✕` was hit.
fn header(ui: &mut Ui, name: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(widgets::spaced("ADD SONGS"))
                    .font(theme::font_sub())
                    .color(theme::p().text_hi),
            );
            // Verbatim, unspaced: it is the user's title, not one of the app's words.
            ui.label(
                egui::RichText::new(name)
                    .font(theme::font_small())
                    .color(theme::p().text_low),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            let icon =
                widgets::Icon::new(theme::ICON_INLINE, theme::p().text_low, theme::p().text_hi);
            close = widgets::icon_button(ui, theme::GLYPH_CLOSE, icon, "CLOSE (ESC)").clicked();
        });
    });
    close
}

/// The live filter. It reports nothing back: [`State::refresh`] is keyed on `query` itself,
/// so the keystroke that changed the text is already the thing that invalidates the rows.
fn filter_field(ui: &mut Ui, query: &mut String, focus: &mut bool) {
    // The sidebar's search field, verbatim: same magnifier at its own size beside the
    // letter-spaced hint, same margins. Two fields that filter should not look like two
    // different controls.
    let hint = widgets::icon_text(
        ui,
        theme::GLYPH_SEARCH,
        theme::ICON_INLINE,
        &widgets::spaced(HINT),
        theme::font_small(),
    );
    let response = ui.add(
        egui::TextEdit::singleline(query)
            .hint_text(egui::WidgetText::Galley(hint))
            .font(egui::TextStyle::Body)
            .text_color(theme::p().text_hi)
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(8, 6)),
    );
    if *focus {
        *focus = false;
        response.request_focus();
    }
}

/// The virtualized list, or the note that stands in for it.
///
/// `show_rows` for the same reason every other list in the app uses it: the picker's job is
/// to show a whole library, and a laid-out row per track would cost thousands of galleys and
/// cover lookups per frame to draw the fifteen that are on screen.
fn list(
    ui: &mut Ui,
    cx: &mut Ctx,
    rows: &[TrackId],
    member: &mut HashSet<TrackId>,
    playlist: u64,
    library_empty: bool,
) {
    if rows.is_empty() {
        note(ui, if library_empty { NO_LIBRARY } else { NO_MATCH });
        return;
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    // The modal is its own layer, not a `views::page`, so it sets the row-to-scrollbar gap
    // itself — the duration sits on the row's right edge here, with no `⋯` after it.
    ui.spacing_mut().scroll.bar_inner_margin = theme::SCROLL_GAP;
    egui::ScrollArea::vertical()
        .id_salt(ROWS_ID)
        .auto_shrink([false, false])
        .show_rows(ui, widgets::ROW_H, rows.len(), |ui, range| {
            for i in range {
                let Some(&id) = rows.get(i) else {
                    break;
                };
                if row(ui, cx, id, member.contains(&id)) {
                    cx.act(Action::AddToPlaylist(playlist, vec![id]));
                    // Optimistic, so the row flips to `✓` in the frame it was clicked. The
                    // action is applied at the end of this frame and invalidates the cache,
                    // so the next rebuild reads the truth back off the playlist — this only
                    // has to be right for the rest of *this* frame.
                    member.insert(id);
                }
            }
        });
}

/// One picker row: `+` / `✓`, cover, title, artist, length. True when `+` was clicked.
///
/// ```text
///  L        L+28      L+44                                    R-48   R
///  ├── +/✓ ──┤  gap   ├─ cover ─┤ ├── title ──┤ ├── artist ──┤ ├ time ┤
///     28 px    16 px     28 px       fills      ≤190 px, 26%    40 px
/// ```
fn row(ui: &mut Ui, cx: &mut Ctx, id: TrackId, member: bool) -> bool {
    // `Sense::hover`: the row itself does nothing. UI-SPEC v1.2's track row plays on a
    // double click and this one deliberately does not — the popup adds, and a surface that
    // sometimes started the music instead would be the worst kind of surprise.
    let (rect, _) = widgets::row(ui, widgets::ROW_H, Sense::hover());
    let hovered = ui.rect_contains_pointer(rect);
    widgets::row_background(ui, rect, hovered, false);

    let mark_rect = Rect::from_min_max(
        egui::pos2(rect.left(), rect.top()),
        egui::pos2(rect.left() + widgets::LEAD_W, rect.bottom()),
    );
    let clicked = mark(ui, mark_rect, plus_id(id), member);

    let art_rect = Rect::from_min_size(
        egui::pos2(
            mark_rect.right() + widgets::LEAD_GAP,
            rect.center().y - widgets::ROW_ART * 0.5,
        ),
        Vec2::splat(widgets::ROW_ART),
    );
    let lib = cx.lib;
    let track = lib.track(id);
    artwork::paint_cover(ui, cx.art, track.map(|t| &t.album_key), art_rect);

    let text_x = art_rect.right() + theme::LCD_PAD + 2.0;
    // UI-SPEC v1.2 §Track rows: the divider starts at the title, not at the row's edge.
    widgets::hairline_bottom_from(ui, rect, text_x);

    // The duration is the whole tail here — no heart and no `⋯` — so it sits on the row's
    // right edge rather than on `widgets::tail_w()`.
    let artist_w = (rect.width() * 0.26).min(theme::SONG_COL_W);
    let artist_x = rect.right() - theme::TIME_W - theme::LCD_PAD - artist_w;
    let title_w = (artist_x - theme::LCD_PAD - text_x).max(1.0);

    let (title, artist) = match track {
        Some(t) => (t.title.as_str(), t.artist.as_str()),
        None => ("—", ""),
    };
    widgets::text_left(
        ui,
        egui::pos2(text_x, rect.center().y),
        title,
        theme::font_body(),
        theme::p().text_hi,
        title_w,
    );
    if artist_w > theme::TRACK_NO_W {
        widgets::text_left(
            ui,
            egui::pos2(artist_x, rect.center().y),
            artist,
            theme::font_small(),
            theme::p().text_mid,
            artist_w,
        );
    }
    ui.painter().text(
        egui::pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        cx.fmt.dur(id),
        theme::font_small(),
        theme::p().text_low,
    );
    clicked
}

/// The id of one row's `+`, derived from the track rather than from the row it happens to
/// be on — the filter reorders nothing but it does change which rows exist, and a button
/// keyed on the row index would inherit the pressed state of whatever was there before.
fn plus_id(track: TrackId) -> Id {
    Id::new((MODAL_ID, "plus", track))
}

/// The leading column: an accent `+` button, or a quiet `✓` readout.
///
/// The `✓` state allocates **nothing** — no hit rect, no id. UI-SPEC v1.4 says the checkmark
/// is not a remove button, and the cheapest way to guarantee "clicking it does nothing" is
/// for there to be nothing there to click. It also makes a second `+` on the same song
/// impossible from this surface, which is what stands in for the dedupe `append_tracks`
/// deliberately does not do.
fn mark(ui: &mut Ui, rect: Rect, id: Id, member: bool) -> bool {
    let p = theme::p();
    if member {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            theme::GLYPH_CHECK,
            theme::font_icon(theme::ICON_LEAD),
            p.text_low,
        );
        return false;
    }
    let response = ui.interact(rect, id, Sense::click());
    // The accent, because this is the one action the surface exists for (UI-SPEC §tokens:
    // "yellow always means active / playing / primary action"). `accent_text` and not the
    // raw accent because it is a glyph and not a fill, and it dims on hover exactly like a
    // filled heart does — at full accent there is nowhere brighter to go.
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        theme::GLYPH_PLUS,
        theme::font_icon(theme::ICON_LEAD),
        theme::hover_color(response.hovered(), p.accent_text, p.accent_text_dim),
    );
    response.clicked()
}

/// A centred `TEXT_LOW` line where the list would be.
fn note(ui: &mut Ui, text: &str) {
    let rect = ui.available_rect_before_wrap();
    let galley = widgets::truncated(
        ui,
        &widgets::spaced(text),
        theme::font_body(),
        theme::p().text_low,
        rect.width(),
    );
    let pos = Align2::CENTER_CENTER.anchor_size(rect.center(), galley.size());
    ui.painter().galley(pos.min, galley, theme::p().text_low);
    ui.allocate_space(rect.size());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::Artwork;
    use crate::nav::{Fmt, Now};
    use phoebus_core::{Favorites, Track};

    fn make(rel: &str, title: &str, artist: &str) -> Track {
        let mut t = Track::new(rel);
        t.title = title.to_string();
        t.artist = artist.to_string();
        t.album_artist = artist.to_string();
        t.album = "Odyssey".to_string();
        t.duration = std::time::Duration::from_secs(120);
        t.refresh_key();
        t
    }

    fn library() -> Library {
        Library::build(
            "/lib",
            vec![
                make("HOME/Odyssey/01 Intro.m4a", "Intro", "HOME"),
                make("HOME/Odyssey/02 Resonance.m4a", "Resonance", "HOME"),
                make("Woodkid/S16/01 Goliath.m4a", "Goliath", "Woodkid"),
            ],
        )
    }

    fn playlist(entries: &[&str]) -> Playlist {
        Playlist {
            id: 1,
            name: "Late Night".into(),
            entries: entries.iter().map(|e| (*e).to_string()).collect(),
            created_at: 0,
            modified_at: 7,
        }
    }

    /// One headless egui context driven across several passes, so a widget registered on one
    /// pass can be found — and clicked — on the next. Everything the picker needs from the
    /// app is faked here and nothing of it touches disk.
    struct Harness {
        ctx: egui::Context,
        art: Artwork,
        fmt: Fmt,
        favs: Favorites,
        /// Everything the last [`Harness::pass`] raised.
        actions: Vec<Action>,
    }

    impl Harness {
        fn new(lib: &Library) -> Harness {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Harness {
                ctx,
                art: Artwork::new(),
                fmt: Fmt::build(lib),
                favs: crate::nav::test_favorites(),
                actions: Vec::new(),
            }
        }

        fn pass(&mut self, st: &mut State, lib: &Library, pl: &Playlist, events: Vec<egui::Event>) {
            self.actions.clear();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1280.0, 820.0),
                )),
                events,
                ..Default::default()
            };
            let (art, fmt, favs, actions) =
                (&mut self.art, &self.fmt, &self.favs, &mut self.actions);
            let mut out = self.ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib,
                    art,
                    playlists: std::slice::from_ref(pl),
                    favs,
                    now: Now::default(),
                    fmt,
                    actions,
                };
                show(ui.ctx(), &mut cx, st, pl);
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }

        /// Where one row's `+` ended up, or `None` if that row allocated no button at all.
        fn plus(&self, track: TrackId) -> Option<egui::Pos2> {
            self.ctx
                .read_response(plus_id(track))
                .map(|r| r.rect.center())
        }
    }

    fn click(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// A closed picker paints nothing at all — no backdrop, no layer, no blocked input, and
    /// above all no filter pass over the library on every frame of every playlist page.
    #[test]
    fn a_closed_picker_draws_nothing() {
        let lib = library();
        let pl = playlist(&[]);
        let mut st = State::default();
        let mut h = Harness::new(&lib);
        h.pass(&mut st, &lib, &pl, Vec::new());
        assert!(h.actions.is_empty());
        assert!(st.rows.is_empty(), "not even the filter ran");
        assert!(h.plus(lib.tracks_sorted()[0]).is_none());
    }

    /// The filter is recomputed only when the text changes, and the membership set only when
    /// the playlist does. Both walk the whole library, so a per-frame rebuild would be
    /// thousands of substring tests and hashes per 16 ms.
    #[test]
    fn both_caches_are_keyed_on_their_inputs() {
        let lib = library();
        let pl = playlist(&["HOME/Odyssey/01 Intro.m4a"]);
        let mut st = State::default();
        st.open();

        st.refresh(&lib, &pl);
        assert_eq!(st.rows.len(), 3, "an empty filter is the whole library");
        assert_eq!(st.rows, lib.tracks_sorted(), "…in the Songs view's order");
        assert_eq!(
            st.member,
            HashSet::from([TrackId::for_rel_path("HOME/Odyssey/01 Intro.m4a")])
        );

        let keys = (st.rows_key.clone(), st.member_key);
        st.refresh(&lib, &pl);
        assert_eq!((st.rows_key.clone(), st.member_key), keys, "no work done");

        st.query = "wood".to_string();
        st.refresh(&lib, &pl);
        assert_eq!(st.rows.len(), 1, "matched on the artist");
        assert_eq!(st.member.len(), 1, "…without touching the membership set");

        let mut grown = pl.clone();
        grown.entries.push("Woodkid/S16/01 Goliath.m4a".into());
        st.refresh(&lib, &grown);
        assert_eq!(st.member.len(), 2, "a new entry is picked up by the key");

        st.invalidate();
        assert!(st.rows_key.is_none() && st.member_key.is_none());
        assert!(st.open, "…and the modal stays up");
        assert_eq!(st.query, "wood", "…with what was typed still typed");
    }

    /// UI-SPEC v1.4 §Add songs, the two clauses that make the popup usable: clicking `+`
    /// raises the add for **that** track, and the popup STAYS OPEN so the next song is one
    /// click away.
    #[test]
    fn clicking_plus_adds_that_song_and_leaves_the_popup_up() {
        let lib = library();
        let pl = playlist(&[]);
        let mut st = State::default();
        st.open();
        let mut h = Harness::new(&lib);
        // Two settling passes: the modal's area is placed on the first and its rows only sit
        // where the pointer will be told to click from the second on.
        for _ in 0..3 {
            h.pass(&mut st, &lib, &pl, Vec::new());
        }
        let first = lib.tracks_sorted()[0];
        let at = h.plus(first).expect("the first row's `+`");

        h.pass(&mut st, &lib, &pl, click(at));
        assert_eq!(
            h.actions
                .iter()
                .filter_map(|a| match a {
                    Action::AddToPlaylist(id, tracks) if *id == pl.id => Some(tracks.clone()),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>(),
            vec![first],
            "exactly the clicked row, exactly once: {:?}",
            h.actions
        );
        assert!(st.open, "adding a song must not dismiss the popup");
        assert!(
            st.member.contains(&first),
            "the row has to flip to `✓` in the same frame"
        );

        // …and now that it is a member the button is gone, so a second click on the same
        // spot cannot add it twice (`append_tracks` has no dedupe of its own). Two passes:
        // `read_response` answers from the *previous* frame, so one pass would still find
        // the button the click itself was delivered to.
        for _ in 0..2 {
            h.pass(&mut st, &lib, &pl, Vec::new());
        }
        assert!(h.plus(first).is_none());
        h.pass(&mut st, &lib, &pl, click(at));
        assert!(
            h.actions.is_empty(),
            "a `✓` is not a button: {:?}",
            h.actions
        );
    }

    /// A song already on the list shows its checkmark from the very first frame — the popup
    /// never offers `+` for it and then takes it back.
    #[test]
    fn a_song_already_on_the_list_has_no_button_from_the_start() {
        let lib = library();
        let ids = lib.tracks_sorted().to_vec();
        let pl = playlist(&["HOME/Odyssey/01 Intro.m4a"]);
        let mut st = State::default();
        st.open();
        let mut h = Harness::new(&lib);
        for pass in 0..3 {
            h.pass(&mut st, &lib, &pl, Vec::new());
            assert!(
                h.plus(ids[0]).is_none(),
                "pass {pass}: the member row must allocate nothing"
            );
        }
        assert!(
            h.plus(ids[1]).is_some() && h.plus(ids[2]).is_some(),
            "every other row still offers `+`"
        );
    }

    /// The filter narrows the list live, and a query that matches nothing leaves the popup
    /// standing with its note rather than collapsing.
    #[test]
    fn the_filter_narrows_the_rows_and_survives_a_miss() {
        let lib = library();
        let pl = playlist(&[]);
        let ids = lib.tracks_sorted().to_vec();
        let mut st = State::default();
        st.open();
        let mut h = Harness::new(&lib);
        for _ in 0..2 {
            h.pass(&mut st, &lib, &pl, Vec::new());
        }

        st.query = "goliath".to_string();
        st.rows_key = None;
        for _ in 0..2 {
            h.pass(&mut st, &lib, &pl, Vec::new());
        }
        let goliath = ids
            .iter()
            .copied()
            .find(|id| lib.track(*id).is_some_and(|t| t.title == "Goliath"))
            .expect("Goliath");
        assert_eq!(st.rows, vec![goliath]);
        assert!(h.plus(goliath).is_some(), "the one hit is still addable");

        st.query = "zzzz".to_string();
        st.rows_key = None;
        h.pass(&mut st, &lib, &pl, Vec::new());
        assert!(st.rows.is_empty());
        assert!(st.open, "a miss is not a reason to close");
    }

    /// The whole library, virtualized: a 4 000-track picker must lay out a screenful of rows
    /// and not 4 000 of them.
    #[test]
    fn a_large_library_only_lays_out_the_visible_rows() {
        let tracks: Vec<Track> = (0..4000)
            .map(|i| make(&format!("A{i:04}/Alb/{i:04} Song.m4a"), "Song", "Artist"))
            .collect();
        let lib = Library::build("/lib", tracks);
        let pl = playlist(&[]);
        let mut st = State::default();
        st.open();
        let mut h = Harness::new(&lib);
        for _ in 0..3 {
            h.pass(&mut st, &lib, &pl, Vec::new());
        }
        assert_eq!(st.rows.len(), 4000, "all of them are in the list…");
        let drawn = lib
            .tracks_sorted()
            .iter()
            .filter(|id| h.plus(**id).is_some())
            .count();
        assert!(
            drawn > 0 && drawn < 60,
            "…but only a screenful is laid out: {drawn} rows"
        );
    }

    /// Clicking the dimmed area outside the popup dismisses it — the one of the three
    /// dismissals egui answers for, and the one that could plausibly fire by accident from
    /// inside the popup. So both halves are driven: a click on the backdrop closes, a click
    /// on a row does not.
    #[test]
    fn a_click_outside_closes_and_a_click_inside_does_not() {
        let lib = library();
        let pl = playlist(&[]);
        let mut st = State::default();
        st.open();
        let mut h = Harness::new(&lib);
        for _ in 0..3 {
            h.pass(&mut st, &lib, &pl, Vec::new());
        }

        // Inside first: the `+` of a row is as "in the popup" as it gets.
        let at = h.plus(lib.tracks_sorted()[0]).expect("the first row's `+`");
        h.pass(&mut st, &lib, &pl, click(at));
        assert!(st.open, "a row click must not reach the backdrop under it");

        // The top-left corner of a 1280 × 820 screen is nowhere near a centred 640-wide box.
        h.pass(&mut st, &lib, &pl, click(egui::pos2(6.0, 6.0)));
        assert!(!st.open, "clicking outside must dismiss the popup");
    }

    /// `Esc` is `Phoebus::escape`'s to handle, and [`State::close`] is the step it stops on.
    #[test]
    fn close_reports_whether_there_was_anything_to_close() {
        let mut st = State::default();
        assert!(!st.close(), "nothing was open");
        st.open();
        assert!(st.open && st.focus);
        assert!(st.close());
        assert!(!st.open);
        assert!(!st.close());
    }
}
