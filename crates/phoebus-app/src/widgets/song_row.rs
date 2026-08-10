//! The track row, and the three pieces every list view builds one out of.
//!
//! UI-SPEC v1.2 §Track rows makes the list views — album detail, playlist, Songs, search,
//! and Favorites since v1.3 — one family: a 40 px row, a [`lead`] state column that answers
//! "is this the track, and is it moving?", a divider that starts at the title instead of at
//! the row's left edge, and a [`tail`] carrying the favourite [`heart`], the duration and a
//! `⋯` button that opens the row's menu on a *left* click.
//!
//! [`tail`] is why the heart needed no work in four of the five views: the whole right end
//! of a row is defined once, here, and every view already delegated to it.
//!
//! The pieces are exported one at a time rather than as a single `show()` because the five
//! views disagree about everything *between* those two ends: the album page has track
//! numbers and no artwork, Songs is a virtualized `egui_extras` table with sortable columns,
//! playlist and search rows carry a cover and stack title over artist. [`show`] is only the
//! last of those.

use egui::{Align2, Id, Rect, Response, Sense, Ui, Vec2};
use phoebus_core::TrackId;

use crate::artwork;
use crate::nav::{Action, Ctx};
use crate::theme;
use crate::widgets::{self, equalizer};

/// What the leading state column of a row is showing (UI-SPEC v1.2 §Track rows).
///
/// The four spec'd states are five variants because "playing" and "paused" share the `▶`
/// and the equalizer between them: a paused current row shows frozen bars, and hovering it
/// offers `▶` to resume — the same glyph an idle row offers to start.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Affordance {
    /// Not the current track and not under the pointer: the track number, or nothing.
    Idle,
    /// `▶` — click to play this row, or to resume the paused current track.
    Play,
    /// `⏸` — click to pause the current track.
    Pause,
    /// The current track's equalizer: moving while it plays, frozen while it is paused.
    Bars {
        /// False for the paused pose.
        animated: bool,
    },
}

impl Affordance {
    /// True for the two states that are buttons. The other two must not sense clicks at
    /// all, or the invisible target would eat the double-click that plays the row.
    pub fn is_button(self) -> bool {
        matches!(self, Affordance::Play | Affordance::Pause)
    }
}

/// The whole leading-column state machine, as a pure function of the three facts a row
/// knows about itself. Every view calls exactly this; nothing else decides what to draw.
pub fn state(current: bool, playing: bool, hovered: bool) -> Affordance {
    match (current, playing, hovered) {
        // Someone else's row: the number, until the pointer arrives.
        (false, _, false) => Affordance::Idle,
        (false, _, true) => Affordance::Play,
        // The current row, running.
        (true, true, false) => Affordance::Bars { animated: true },
        (true, true, true) => Affordance::Pause,
        // The current row, paused: the bars stop, and the pointer offers to resume.
        (true, false, false) => Affordance::Bars { animated: false },
        (true, false, true) => Affordance::Play,
    }
}

/// What a click on the leading column asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lead {
    /// Nothing was clicked.
    Nothing,
    /// Start this row (`Action::Play` with the view's list as context).
    PlayRow,
    /// Pause or resume what is already loaded (`Action::TogglePlay`).
    TogglePlay,
}

/// Draw the leading state column into `rect` and report what was clicked.
///
/// `number` is what [`Affordance::Idle`] paints — the track number on an album page, and
/// the empty string everywhere else (UI-SPEC v1.2 removed the Songs view's thumbnail and
/// put nothing in its place). Everything is right-aligned on `rect`'s right edge so the
/// number, the glyphs and the bars all share one optical column and nothing jumps sideways
/// as the state changes.
pub fn lead(ui: &mut Ui, rect: Rect, id: Id, aff: Affordance, current: bool, number: &str) -> Lead {
    let clicked = aff.is_button() && ui.interact(rect, id, Sense::click()).clicked();
    let color = if current {
        theme::p().accent_text
    } else {
        theme::p().text_hi
    };
    match aff {
        Affordance::Idle => {
            if !number.is_empty() {
                ui.painter().text(
                    egui::pos2(rect.right(), rect.center().y),
                    Align2::RIGHT_CENTER,
                    number,
                    theme::font_small(),
                    theme::p().text_low,
                );
            }
        }
        Affordance::Play | Affordance::Pause => {
            let glyph = if aff == Affordance::Play {
                theme::GLYPH_PLAY
            } else {
                theme::GLYPH_PAUSE
            };
            // CENTER_CENTER on the equalizer's own center, not RIGHT_CENTER on the
            // column edge: a glyph's layout box carries side bearing the painted bars
            // don't have, so right-edge alignment left the two visibly out of column.
            // The construction survived the move to Phosphor unchanged; only the size did
            // not, because the ink of an icon fills its em far more completely than `▶`
            // did (see [`theme::ICON_LEAD`]).
            ui.painter().text(
                egui::pos2(rect.right() - equalizer::WIDTH * 0.5, rect.center().y),
                Align2::CENTER_CENTER,
                glyph,
                theme::font_icon(theme::ICON_LEAD),
                color,
            );
        }
        Affordance::Bars { animated } => {
            let time = animated.then(|| ui.input(|i| i.time));
            let box_rect = Rect::from_min_max(
                egui::pos2(rect.right() - equalizer::WIDTH, rect.top()),
                egui::pos2(rect.right(), rect.bottom()),
            );
            equalizer::paint(ui, box_rect, theme::p().accent, time);
        }
    }
    match (clicked, current) {
        (false, _) => Lead::Nothing,
        (true, true) => Lead::TogglePlay,
        (true, false) => Lead::PlayRow,
    }
}

/// The right end of a row: the favourite heart, the duration, and the `⋯` button.
///
/// Laid out from the row's right edge, so every list view's tail is the same column
/// whatever sits to the left of it (UI-SPEC v1.3 §Favorites — "so every heart sits in one
/// right-aligned column"). With `R` = `row.right()` and the widths from [`widgets`]:
///
/// ```text
///  R-104        R-80  R-72          R-32       R-24    R
///    ├── HEART ──┤ gap ├── duration ─┤   gap    ├─ ⋯ ──┤
///        24 px     8      40 px, right-aligned    24 px
/// ```
///
/// Returns the `⋯` button's response — hang the row's menu on it with
/// `egui::Popup::menu(&more)`, which is the same popup `Response::context_menu` opens, so
/// the left-click menu and the right-click menu are one widget with one look. The heart is
/// not returned: it raises its own [`Action::ToggleFavTrack`], which is the only thing any
/// caller ever did with it.
///
/// `hovered` is the row's own hover, tested geometrically (`Ui::rect_contains_pointer`)
/// rather than through a `Response`: both buttons sit *on top* of the row and take its
/// hover away, so a row that asked its own response would blink them off the moment the
/// pointer reached one.
pub fn tail(
    ui: &mut Ui,
    cx: &mut Ctx,
    row: Rect,
    id: Id,
    track: TrackId,
    hovered: bool,
) -> Response {
    let more_rect = Rect::from_min_max(
        egui::pos2(row.right() - widgets::MORE_W, row.top()),
        egui::pos2(row.right(), row.bottom()),
    );
    let time_right = more_rect.left() - widgets::DUR_GAP;
    ui.painter().text(
        egui::pos2(time_right, row.center().y),
        Align2::RIGHT_CENTER,
        cx.fmt.dur(track),
        theme::font_small(),
        theme::p().text_low,
    );
    let heart_right = time_right - theme::TIME_W - widgets::HEART_GAP;
    let heart_rect = Rect::from_min_max(
        egui::pos2(heart_right - widgets::HEART_W, row.top()),
        egui::pos2(heart_right, row.bottom()),
    );
    if heart(
        ui,
        heart_rect,
        id.with("heart"),
        cx.favs.is_track(track),
        hovered,
    ) {
        cx.act(Action::ToggleFavTrack(track));
    }
    more(ui, more_rect, id, hovered)
}

/// What the heart column is showing (UI-SPEC v1.3 §Favorites), as a pure function of the
/// three facts the column knows about itself.
///
/// The whole state machine of a heart, in the shape [`state`] gave the leading column: the
/// paint code below reads this and nothing else decides what a heart looks like.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fav {
    /// Not hearted, and the pointer is not on the row: nothing at all.
    Blank,
    /// Not hearted, row hovered: an outline heart, offering to become one.
    Outline {
        /// The pointer is on the heart itself, not merely on the row.
        hot: bool,
    },
    /// Hearted: a filled heart, painted whether or not anything is hovered.
    Filled {
        /// The pointer is on the heart itself.
        hot: bool,
    },
}

/// The heart's state from `hearted`, the row's hover and the heart's own hover.
///
/// `hearted` wins outright: a favourite is visible on an untouched row, which is the point
/// of it. `row` only decides whether an *unhearted* heart is offered at all.
pub fn fav_state(hearted: bool, row: bool, hot: bool) -> Fav {
    match (hearted, row || hot) {
        (true, _) => Fav::Filled { hot },
        (false, true) => Fav::Outline { hot },
        (false, false) => Fav::Blank,
    }
}

/// The favourite heart: a 24 px hit rect that is ALWAYS allocated and only sometimes
/// painted, on exactly the terms [`more`] documents — a target that came and went with the
/// row's hover would drop the click that is already on its way to it.
///
/// Returns true when it was clicked. The click cannot reach the row underneath (egui gives
/// the pointer to the last widget registered over a point), so hearting a song never plays
/// it — which is the one thing UI-SPEC v1.3 says about this control twice.
///
/// The two weights come from two different faces: the outline is Phosphor Regular, reached
/// through the text families like every other icon, and the fill is Phosphor Fill, reached
/// through a family of its own because the two share a codepoint (see
/// [`theme::font_icon_fill`]).
pub fn heart(ui: &mut Ui, rect: Rect, id: Id, hearted: bool, row_hovered: bool) -> bool {
    let response = ui.interact(rect, id, Sense::click());
    let p = theme::p();
    let (font, color) = match fav_state(hearted, row_hovered, response.hovered()) {
        Fav::Blank => return false,
        Fav::Outline { hot } => (
            theme::font_icon(theme::ICON_HEART),
            theme::hover_color(hot, p.text_mid, p.text_hi),
        ),
        Fav::Filled { hot } => (
            theme::font_icon_fill(theme::ICON_HEART),
            theme::hover_color(hot, p.accent_text, p.accent_text_dim),
        ),
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        theme::GLYPH_HEART,
        font,
        color,
    );
    response.clicked()
}

/// The `⋯` button: a 24 px hit rect that is only *painted* while its row is hovered or its
/// menu is open, but is always allocated — a button that stops existing when the pointer
/// leaves the row would close its own menu the moment the pointer moved onto it.
///
/// The dots are painted rather than typed. `⋯` (U+22EF) does render in the bundled fonts,
/// but three 3 px squares are immune to font substitution, sit exactly on the pixel grid at
/// any DPI, and match the square knobs and hairlines the rest of the design is made of.
pub fn more(ui: &mut Ui, rect: Rect, id: Id, hovered: bool) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    if hovered || open {
        let color = if response.hovered() || open {
            theme::p().text_hi
        } else {
            theme::p().text_low
        };
        let painter = ui.painter();
        let centre = rect.center();
        let half = widgets::DOT * 0.5;
        for step in -1..=1 {
            let x = (centre.x + step as f32 * widgets::DOT_STEP).round();
            let y = centre.y.round();
            painter.rect_filled(
                Rect::from_min_max(
                    egui::pos2(x - half, y - half),
                    egui::pos2(x + half, y + half),
                ),
                egui::CornerRadius::ZERO,
                color,
            );
        }
    }
    response
}

/// Everything one [`show`] row reported this frame.
pub struct Row {
    /// The row itself: click selects, double-click plays.
    pub response: Response,
    /// The `⋯` button, for `egui::Popup::menu`.
    pub more: Response,
    /// What the leading state column was asked to do.
    pub lead: Lead,
}

/// x where a [`show`] row's title starts, measured from the row's left edge: the state
/// column, the gap, the cover, and the cover's own gap.
///
/// Exported because the playlist's `+ ADD SONGS` foot row has to line up with the titles
/// above it, and re-deriving the sum at that call site is how two columns drift apart.
pub fn title_x(row: Rect) -> f32 {
    row.left() + widgets::LEAD_W + widgets::LEAD_GAP + widgets::ROW_ART + theme::LCD_PAD + 2.0
}

/// Draw one playlist / search song row: state column, cover, title over artist, album,
/// duration, `⋯`.
///
/// The album-detail tracklist is deliberately *not* this row — it shows track numbers and
/// no artwork, because on an album page the cover is already three inches tall.
pub fn show(ui: &mut Ui, cx: &mut Ctx, id: TrackId, selected: bool) -> Row {
    row_with(ui, cx, id, selected, Sense::click())
}

/// The same row, but it can also be picked up and dragged to a new position.
///
/// The extra sense is `Sense::click_and_drag()` rather than a hand-rolled distance
/// threshold, because that is precisely what egui's own threshold is: with both senses set
/// it postpones the click-or-drag verdict until the pointer has moved further than
/// `InputOptions::max_click_dist`, been held longer than `max_click_duration`, or left the
/// row. Until then the gesture is still a click, so click-to-select, double-click-to-play
/// and the right-click menu all keep landing as themselves and only a real drag ever
/// reports [`Response::drag_started`].
///
/// Only the playlist asks for this, because only the playlist has an order of its own to
/// change. Every other list view is derived — sorted, filtered, chronological — and a drop
/// there would have nowhere to be saved.
pub fn draggable(ui: &mut Ui, cx: &mut Ctx, id: TrackId, selected: bool) -> Row {
    row_with(ui, cx, id, selected, Sense::click_and_drag())
}

fn row_with(ui: &mut Ui, cx: &mut Ctx, id: TrackId, selected: bool, sense: Sense) -> Row {
    let (rect, response) = widgets::row(ui, widgets::ROW_H, sense);
    let current = cx.now.is_current(id);
    let hovered = ui.rect_contains_pointer(rect);
    widgets::row_background(ui, rect, hovered || selected, false);

    let lead_rect = Rect::from_min_max(
        egui::pos2(rect.left(), rect.top()),
        egui::pos2(rect.left() + widgets::LEAD_W, rect.bottom()),
    );
    let hit = lead(
        ui,
        lead_rect,
        response.id.with("lead"),
        state(current, cx.now.playing, hovered),
        current,
        "",
    );

    let art_rect = Rect::from_min_size(
        egui::pos2(
            lead_rect.right() + widgets::LEAD_GAP,
            rect.center().y - widgets::ROW_ART * 0.5,
        ),
        Vec2::splat(widgets::ROW_ART),
    );
    let lib = cx.lib;
    let track = lib.track(id);
    let key = track.map(|t| &t.album_key);
    artwork::paint_cover(ui, cx.art, key, art_rect);

    let text_x = title_x(rect);
    // UI-SPEC v1.2: the divider starts at the title, not at the state column.
    widgets::hairline_bottom_from(ui, rect, text_x);

    let tail_left = rect.right() - widgets::tail_w();
    let album_w = (rect.width() * 0.26).min(theme::SONG_COL_W);
    let album_x = tail_left - theme::LCD_PAD - album_w;
    let main_w = (album_x - theme::LCD_PAD - text_x).max(1.0);

    let title_h = ui.text_style_height(&egui::TextStyle::Body);
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    let top = rect.center().y - (title_h + small_h + 1.0) * 0.5;

    let (title, artist, album) = match track {
        Some(t) => (t.title.as_str(), t.artist.as_str(), t.album.as_str()),
        None => ("—", "", ""),
    };
    let title_color = if current {
        theme::p().accent_text
    } else {
        theme::p().text_hi
    };
    widgets::text_left(
        ui,
        egui::pos2(text_x, top + title_h * 0.5),
        title,
        theme::font_body(),
        title_color,
        main_w,
    );
    widgets::text_left(
        ui,
        egui::pos2(text_x, top + title_h + 1.0 + small_h * 0.5),
        artist,
        theme::font_small(),
        theme::p().text_mid,
        main_w,
    );
    if album_w > theme::TRACK_NO_W {
        widgets::text_left(
            ui,
            egui::pos2(album_x, rect.center().y),
            album,
            theme::font_small(),
            theme::p().text_mid,
            album_w,
        );
    }
    let more = tail(ui, cx, rect, response.id.with("more"), id, hovered);
    Row {
        response,
        more,
        lead: hit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UI-SPEC v1.2 §Track rows, as a truth table. This is the whole contract of the
    /// leading column, and it is the one part of a row that can be checked without a
    /// window — so it is checked here rather than by squinting at a screenshot.
    #[test]
    fn the_leading_column_follows_the_spec() {
        // Not the current track: the number, and `▶` under the pointer.
        assert_eq!(state(false, false, false), Affordance::Idle);
        assert_eq!(state(false, true, false), Affordance::Idle);
        assert_eq!(state(false, false, true), Affordance::Play);
        assert_eq!(state(false, true, true), Affordance::Play);
        // The current track, playing: bars, and `⏸` under the pointer.
        assert_eq!(
            state(true, true, false),
            Affordance::Bars { animated: true }
        );
        assert_eq!(state(true, true, true), Affordance::Pause);
        // The current track, paused: frozen bars, and `▶` to resume.
        assert_eq!(
            state(true, false, false),
            Affordance::Bars { animated: false }
        );
        assert_eq!(state(true, false, true), Affordance::Play);
    }

    /// Only the two glyphs are clickable. An always-on hit rect in the number column would
    /// swallow the first half of every double-click-to-play over that column.
    #[test]
    fn only_the_glyph_states_are_buttons() {
        assert!(Affordance::Play.is_button());
        assert!(Affordance::Pause.is_button());
        assert!(!Affordance::Idle.is_button());
        assert!(!Affordance::Bars { animated: true }.is_button());
        assert!(!Affordance::Bars { animated: false }.is_button());
    }

    /// Every state paints without panicking, and none of the four reports a click when
    /// nothing was clicked.
    #[test]
    fn all_five_states_paint_and_report_nothing_when_untouched() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(widgets::LEAD_W, 40.0));
        let mut out = ctx.run_ui(screen(400.0, 200.0), |ui| {
            for (aff, current) in [
                (Affordance::Idle, false),
                (Affordance::Play, false),
                (Affordance::Pause, true),
                (Affordance::Bars { animated: true }, true),
                (Affordance::Bars { animated: false }, true),
            ] {
                let hit = lead(ui, rect, Id::new(("lead", current, aff)), aff, current, "7");
                assert_eq!(hit, Lead::Nothing);
            }
        });
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        out.textures_delta.clear();
    }

    /// A click on the current row's `▶`/`⏸` must resume or pause — never re-`Play`, which
    /// would restart the track it is already on from zero. Driven with a synthetic pointer,
    /// so this is the real `Sense::click()` path and not a re-derivation of it.
    #[test]
    fn the_current_row_toggles_and_every_other_row_plays() {
        for (aff, current, want) in [
            (Affordance::Play, false, Lead::PlayRow),
            (Affordance::Play, true, Lead::TogglePlay),
            (Affordance::Pause, true, Lead::TogglePlay),
        ] {
            assert_eq!(click_lead(aff, current), want, "{aff:?} current={current}");
        }
        // The two non-button states cannot report anything, whatever the pointer does.
        for aff in [Affordance::Idle, Affordance::Bars { animated: true }] {
            assert_eq!(click_lead(aff, false), Lead::Nothing, "{aff:?}");
        }
    }

    /// Press and release the primary button over the leading column, and return what it
    /// reported. Two passes: the first registers the widget with egui, the second delivers
    /// the click to it.
    fn click_lead(aff: Affordance, current: bool) -> Lead {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(widgets::LEAD_W, 40.0));
        let centre = rect.center();
        let mut hit = Lead::Nothing;
        for pass in 0..2 {
            let mut input = screen(400.0, 200.0);
            input.events = if pass == 0 {
                vec![egui::Event::PointerMoved(centre)]
            } else {
                vec![
                    egui::Event::PointerMoved(centre),
                    egui::Event::PointerButton {
                        pos: centre,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos: centre,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            let mut out = ctx.run_ui(input, |ui| {
                hit = lead(ui, rect, Id::new("lead"), aff, current, "7");
            });
            out.textures_delta.clear();
        }
        hit
    }

    fn screen(w: f32, h: f32) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h))),
            ..Default::default()
        }
    }

    /// The `⋯` button is allocated whether or not it is painted: a hit rect that came and
    /// went with the row's hover would take its own menu down with it.
    ///
    /// The heart is measured in the same pass, because it is the same trap and because this
    /// is where the tail's x-layout is pinned (UI-SPEC v1.3 §Favorites: the heart column
    /// sits immediately LEFT of the duration, in one right-aligned column for every view).
    #[test]
    fn the_tail_allocates_both_buttons_even_when_they_are_invisible() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let lib = library();
        let id = *lib.tracks_sorted().first().expect("a track");
        let fmt = crate::nav::Fmt::build(&lib);
        let favs = crate::nav::test_favorites();
        let mut art = crate::artwork::Artwork::new();
        let mut actions = Vec::new();
        let row = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, widgets::ROW_H));
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            },
            |ui| {
                let mut cx = Ctx {
                    lib: &lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now: crate::nav::Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                let cold = tail(ui, &mut cx, row, Id::new("cold"), id, false);
                let hot = tail(ui, &mut cx, row, Id::new("hot"), id, true);
                assert_eq!(cold.rect, hot.rect, "visibility must not move the target");
                assert_eq!(cold.rect.width(), widgets::MORE_W);
                assert!(cold.rect.height() >= theme::HIT_MIN);
                assert!(cold.sense.senses_click());

                // The heart, read back off the context by the id `tail` derives.
                let heart = |base: &str| {
                    ui.ctx()
                        .read_response(Id::new(base).with("heart"))
                        .expect("the heart is allocated whether or not it is painted")
                        .rect
                };
                let (cold_heart, hot_heart) = (heart("cold"), heart("hot"));
                assert_eq!(
                    cold_heart, hot_heart,
                    "an invisible heart still has a target"
                );
                assert_eq!(cold_heart.width(), widgets::HEART_W);
                assert!(cold_heart.height() >= theme::HIT_MIN);

                // …and where it is: 24 px of heart, 8 px of air, the 40 px duration column,
                // 8 px of air, 24 px of `⋯`, all measured from the row's right edge.
                assert_eq!(row.right() - cold.rect.right(), 0.0);
                assert_eq!(
                    cold_heart.right(),
                    cold.rect.left() - widgets::DUR_GAP - theme::TIME_W - widgets::HEART_GAP,
                    "the heart is not immediately left of the duration column"
                );
                assert_eq!(
                    row.right() - cold_heart.left(),
                    widgets::tail_w(),
                    "the tail's reserved width and its contents disagree"
                );
            },
        );
        out.textures_delta.clear();
    }

    /// UI-SPEC v1.3 §Favorites, as a truth table — the whole contract of the heart column.
    #[test]
    fn the_heart_column_follows_the_spec() {
        // Not hearted: invisible until the row is hovered, then an outline that brightens
        // when the pointer is on the heart itself.
        assert_eq!(fav_state(false, false, false), Fav::Blank);
        assert_eq!(fav_state(false, true, false), Fav::Outline { hot: false });
        assert_eq!(fav_state(false, true, true), Fav::Outline { hot: true });
        // Hearted: filled, whatever the pointer is doing — "always visible".
        assert_eq!(fav_state(true, false, false), Fav::Filled { hot: false });
        assert_eq!(fav_state(true, true, false), Fav::Filled { hot: false });
        assert_eq!(fav_state(true, true, true), Fav::Filled { hot: true });
        // Hovering the heart implies hovering the row — but the row's own hover is taken
        // away by the buttons on top of it, so the two are tested independently and the
        // heart's own hover has to be enough on its own.
        assert_eq!(fav_state(false, false, true), Fav::Outline { hot: true });
        assert_eq!(fav_state(true, false, true), Fav::Filled { hot: true });
    }

    /// Clicking the heart raises exactly one toggle and NOTHING else — above all not a
    /// `Play`, which is what a click that fell through to the row would produce.
    #[test]
    fn clicking_the_heart_toggles_and_never_plays() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let lib = library();
        let id = *lib.tracks_sorted().first().expect("a track");
        let fmt = crate::nav::Fmt::build(&lib);
        let favs = crate::nav::test_favorites();
        let mut art = crate::artwork::Artwork::new();
        let mut actions: Vec<crate::nav::Action> = Vec::new();
        let mut clicked = None;
        for pass in 0..2 {
            actions.clear();
            // The heart's centre, in the coordinates the row below is laid out in.
            let centre = egui::pos2(
                400.0 - widgets::tail_w() + widgets::HEART_W * 0.5,
                widgets::ROW_H * 0.5,
            );
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(400.0, 200.0),
                )),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(centre));
            if pass == 1 {
                for pressed in [true, false] {
                    input.events.push(egui::Event::PointerButton {
                        pos: centre,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = Ctx {
                    lib: &lib,
                    art: &mut art,
                    playlists: &[],
                    favs: &favs,
                    now: crate::nav::Now::default(),
                    fmt: &fmt,
                    actions: &mut actions,
                };
                let row = show(ui, &mut cx, id, false);
                clicked = Some((row.response.clicked(), row.response.double_clicked()));
            });
            out.textures_delta.clear();
        }
        assert_eq!(
            actions.len(),
            1,
            "one click on the heart raised {actions:?}"
        );
        assert!(
            matches!(actions[0], crate::nav::Action::ToggleFavTrack(t) if t == id),
            "{:?} is not the favourite toggle",
            actions[0]
        );
        assert_eq!(
            clicked,
            Some((false, false)),
            "the row itself must not see the click — the heart is on top of it"
        );
    }

    fn library() -> phoebus_core::Library {
        let mut track = phoebus_core::Track::new("HOME/Odyssey/01 Intro.m4a");
        track.title = "Intro".to_string();
        track.artist = "HOME".to_string();
        track.album_artist = "HOME".to_string();
        track.album = "Odyssey".to_string();
        track.duration = std::time::Duration::from_secs(201);
        track.refresh_key();
        phoebus_core::Library::build("/lib", vec![track])
    }
}
