//! The 64 px player bar: transport, shuffle/repeat, the centre "LCD", volume and the queue
//! toggle.
//!
//! It sits at the BOTTOM of the window and spans its full width (UI-SPEC §Layout), so it is
//! centred on the window rather than on the content area. Every rect below is derived from
//! the panel's own rect, never from the window's — the bar does not care where it is
//! mounted.
//!
//! The controls do NOT pin to the window edges (UI-SPEC v1.2 §Player bar): transport +
//! shuffle/repeat and volume + queue sit immediately on EACH SIDE of the LCD, one
//! `LCD_PAD * 2` gap away, and that whole ensemble is centred in the bar. The edges stay
//! empty.
//!
//! It is laid out from three measured rects rather than from egui layouts, because the
//! ensemble is centred as a unit: both side groups are fixed-width, so their widths have to
//! be known *before* anything is drawn. [`transport_w`] and [`right_group_w`] therefore
//! mirror exactly what [`transport`] and [`right_group`] allocate — change one, change the
//! other. The LCD is the only elastic member, so it is what a narrow window eats into.

use std::sync::Arc;
use std::time::Duration;

use egui::{Align, Align2, Galley, Layout, Rect, Sense, StrokeKind, UiBuilder, Vec2};
use phoebus_core::Repeat;

use crate::artwork;
use crate::nav::{self, Action, Ctx, View};
use crate::theme;
use crate::widgets;

/// Everything the bar needs from the controller.
#[derive(Clone, Debug)]
pub struct BarState {
    /// True while audio is running.
    pub playing: bool,
    /// Position to draw (the scrub target while dragging).
    pub pos: Duration,
    /// Duration of the loaded track.
    pub duration: Duration,
    /// Whether the loaded track can be seeked at all. A file the decoder refuses to seek
    /// gets a dead scrubber rather than one that silently does nothing.
    pub seekable: bool,
    /// Why playback stopped, shown in the idle LCD (error storms, dead audio device).
    pub error: Option<String>,
    /// UI volume, 0.0..=1.0.
    pub volume: f32,
    /// Shuffle toggle.
    pub shuffle: bool,
    /// Repeat mode.
    pub repeat: Repeat,
    /// Whether the Up Next drawer is open (C2 fills it in).
    pub queue_open: bool,
}

/// Draw the whole bar into the panel's `ui`.
pub fn show(ui: &mut egui::Ui, cx: &mut Ctx, state: &BarState) {
    let full = ui.max_rect();
    let gap = theme::LCD_PAD * 2.0;
    let left_w = transport_w();
    let right_w = right_group_w(vol_icon(ui).size().x);

    // Everything but the LCD is fixed-width, so a narrow window shrinks the LCD and nothing
    // else. Below two artwork squares it is dropped entirely (it can no longer hold art +
    // text) and the two groups close up around a single gap.
    let lcd_w = (full.width() - left_w - right_w - 2.0 * gap).clamp(0.0, theme::LCD_MAX_W);
    let drawn = lcd_w > theme::LCD_ART * 2.0;
    let span = if drawn { lcd_w + 2.0 * gap } else { gap };
    let x = full.center().x - (left_w + span + right_w) * 0.5;

    let left_rect =
        Rect::from_min_size(egui::pos2(x, full.top()), Vec2::new(left_w, full.height()));
    let right_rect = Rect::from_min_size(
        egui::pos2(left_rect.right() + span, full.top()),
        Vec2::new(right_w, full.height()),
    );
    let lcd_rect = Rect::from_center_size(
        egui::pos2(left_rect.right() + gap + lcd_w * 0.5, full.center().y),
        Vec2::new(lcd_w, theme::LCD_H),
    );

    ui.scope_builder(
        UiBuilder::new()
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| transport(ui, cx, state),
    );
    if drawn {
        ui.scope_builder(UiBuilder::new().max_rect(lcd_rect), |ui| {
            lcd(ui, cx, state, lcd_rect, full);
        });
    }
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(right_rect)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| right_group(ui, cx, state),
    );
}

/// Side of an auto-sized icon button — the same arithmetic as [`widgets::Icon::new`].
fn icon_hit(size: f32) -> f32 {
    (size + theme::ICON_PAD).max(theme::HIT_MIN)
}

/// Width of `⏮ ▶ ⏭ · shuffle repeat` — exactly what [`transport`] allocates: three
/// [`theme::TRANSPORT_HIT`] squares each followed by a gap, the extra [`theme::LCD_PAD`]
/// before the toggles, then the two auto-sized toggles with one gap between them.
fn transport_w() -> f32 {
    3.0 * (theme::TRANSPORT_HIT + theme::TRANSPORT_GAP)
        + theme::LCD_PAD
        + 2.0 * icon_hit(theme::ICON_SMALL)
        + theme::TRANSPORT_GAP
}

/// Width of `🔊 ─── ≡` — exactly what [`right_group`] allocates, given the measured width
/// of the speaker icon.
fn right_group_w(icon_w: f32) -> f32 {
    icon_w
        + theme::VOLUME_LABEL_GAP
        + theme::VOLUME_W
        + theme::LCD_PAD
        + icon_hit(theme::ICON_SMALL)
}

/// The speaker icon that stands where the `VOL` micro-label used to (UI-SPEC §Player bar
/// allows either). Laid out twice per frame — once to measure the ensemble, once to paint
/// it — which costs nothing: egui caches galleys, so the second call is a lookup.
///
/// It stays a plain painted galley rather than becoming an [`widgets::icon_button`]: it is
/// a *label* for the bar next to it, not a control, and the mute-on-click a speaker button
/// would imply does not exist in this app.
fn vol_icon(ui: &egui::Ui) -> Arc<Galley> {
    widgets::truncated(
        ui,
        theme::GLYPH_VOLUME,
        theme::font_icon(theme::ICON_INLINE),
        theme::p().text_low,
        f32::INFINITY,
    )
}

/// `⏮ ▶ ⏭` then the two toggles.
///
/// The three transport buttons are one row of IDENTICAL [`theme::TRANSPORT_HIT`] squares
/// with equal [`theme::TRANSPORT_GAP`] gaps and one glyph size between them, so the play
/// button cannot grow the row when it swaps `▶` for `⏸` (UI-SPEC §Player bar). The layout
/// is `Align::Center`, which vertically centres all three in the bar.
fn transport(ui: &mut egui::Ui, cx: &mut Ctx, state: &BarState) {
    ui.spacing_mut().item_spacing.x = theme::TRANSPORT_GAP;
    let step = widgets::Icon::new(
        theme::ICON_TRANSPORT,
        theme::p().text_hi,
        theme::p().accent_text,
    )
    .sized(theme::TRANSPORT_HIT);
    if widgets::icon_button(ui, theme::GLYPH_PREV, step, "PREVIOUS").clicked() {
        cx.act(Action::Prev);
    }
    let (glyph, tip) = if state.playing {
        (theme::GLYPH_PAUSE, "PAUSE")
    } else {
        (theme::GLYPH_PLAY, "PLAY")
    };
    if widgets::icon_button(ui, glyph, step, tip).clicked() {
        cx.act(Action::TogglePlay);
    }
    if widgets::icon_button(ui, theme::GLYPH_NEXT, step, "NEXT").clicked() {
        cx.act(Action::Next);
    }

    ui.add_space(theme::LCD_PAD);

    let (idle, hover) = toggle_colors(state.shuffle);
    let small = widgets::Icon::new(theme::ICON_SMALL, idle, hover);
    if widgets::icon_button(ui, theme::GLYPH_SHUFFLE, small, "SHUFFLE").clicked() {
        cx.act(Action::ToggleShuffle);
    }

    let repeat_on = state.repeat != Repeat::Off;
    let (idle, hover) = toggle_colors(repeat_on);
    // One glyph per state, never a glyph plus a superscript: `repeat-once` draws the `1`
    // inside the loop, so the button is the same width in all three modes.
    let glyph = if state.repeat == Repeat::One {
        theme::GLYPH_REPEAT_ONE
    } else {
        theme::GLYPH_REPEAT
    };
    let tip = match state.repeat {
        Repeat::Off => "REPEAT: OFF",
        Repeat::All => "REPEAT: ALL",
        Repeat::One => "REPEAT: ONE",
    };
    let small = widgets::Icon::new(theme::ICON_SMALL, idle, hover);
    if widgets::icon_button(ui, glyph, small, tip).clicked() {
        cx.act(Action::CycleRepeat);
    }
}

fn right_group(ui: &mut egui::Ui, cx: &mut Ctx, state: &BarState) {
    ui.spacing_mut().item_spacing.x = theme::LCD_PAD;
    let (idle, hover) = toggle_colors(state.queue_open);
    let icon = widgets::Icon::new(theme::ICON_SMALL, idle, hover);
    if widgets::icon_button(ui, theme::GLYPH_QUEUE, icon, "QUEUE").clicked() {
        cx.act(Action::ToggleQueue);
    }
    volume(ui, cx, state.volume);
}

/// `🔊 ──────●───` — a speaker icon and a [`theme::VOLUME_W`] bar. No endcap glyphs: the
/// always-visible knob is the readout (UI-SPEC §Player bar).
fn volume(ui: &mut egui::Ui, cx: &mut Ctx, volume: f32) {
    let label = vol_icon(ui);
    let width = label.size().x + theme::VOLUME_LABEL_GAP + theme::VOLUME_W;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, theme::BAR_HIT_H), Sense::hover());
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - label.size().y * 0.5),
        label,
        theme::p().text_low,
    );
    // The bar's strip is the full `BAR_HIT_H` (≥ HIT_MIN) tall and all of it grabs; only
    // the 3 px track and the 6 px knob inside it are painted. The icon is outside the
    // strip, so a click on it does not jump the volume.
    let bar_rect = Rect::from_min_max(
        egui::pos2(rect.right() - theme::VOLUME_W, rect.top()),
        rect.max,
    );
    let out = widgets::bar_at(
        ui,
        bar_rect,
        bar_rect,
        ui.id().with("volume"),
        widgets::BarValue {
            fraction: volume,
            enabled: true,
        },
        widgets::BarStyle::volume(),
    );
    if let Some(v) = out.live.or(out.commit) {
        cx.act(Action::Volume(v));
    }
    out.response.on_hover_text(
        egui::RichText::new(format!("VOLUME {}%", (volume * 100.0).round()))
            .font(theme::font_small())
            .color(theme::p().text_mid),
    );
}

fn lcd(ui: &mut egui::Ui, cx: &mut Ctx, state: &BarState, rect: Rect, bar: Rect) {
    ui.painter().rect(
        rect,
        theme::corner(),
        theme::p().bg2,
        theme::hairline(),
        StrokeKind::Inside,
    );
    let Some(id) = cx.now.track else {
        match &state.error {
            Some(message) => {
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.center().y - 8.0),
                    Align2::CENTER_CENTER,
                    widgets::spaced("PLAYBACK STOPPED"),
                    theme::font_small(),
                    theme::p().text_low,
                );
                let mut detail = message.clone();
                if detail.len() > 72 {
                    detail.truncate(69);
                    detail.push('…');
                }
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.center().y + 8.0),
                    Align2::CENTER_CENTER,
                    detail,
                    theme::font_small(),
                    theme::p().text_low,
                );
            }
            None => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    widgets::spaced("NOT PLAYING"),
                    theme::font_small(),
                    theme::p().text_low,
                );
            }
        }
        return;
    };

    let art_rect = Rect::from_min_size(
        egui::pos2(
            rect.left() + (theme::LCD_H - theme::LCD_ART) * 0.5,
            rect.top() + (theme::LCD_H - theme::LCD_ART) * 0.5,
        ),
        Vec2::splat(theme::LCD_ART),
    );
    let album_key = cx.lib.track(id).map(|t| t.album_key.clone());
    artwork::paint_cover(ui, cx.art, album_key.as_ref(), art_rect);

    let text_left = art_rect.right() + theme::LCD_PAD;
    let text_right = rect.right() - theme::LCD_PAD;
    let width = (text_right - text_left).max(1.0);
    let (title, artist, album) = match cx.lib.track(id) {
        Some(t) => (t.title.as_str(), t.artist.as_str(), t.album.as_str()),
        None => ("—", "", ""),
    };

    let title_h = ui.text_style_height(&egui::TextStyle::Body);
    let small_h = ui.text_style_height(&egui::TextStyle::Small);
    let mut y = art_rect.top() + 1.0;
    widgets::text_left(
        ui,
        egui::pos2(text_left, y + title_h * 0.5),
        title,
        theme::font_body(),
        theme::p().text_hi,
        width,
    );
    y += title_h + 1.0;

    let subtitle_pos = egui::pos2(text_left, y + small_h * 0.5);
    let subtitle_bottom = y + small_h;
    y += small_h + 2.0;

    // The painted seek row is what is left of the 48 px artwork square — about 15 px, well
    // under UI-SPEC's 24 px hit-target floor. So the grab area is a taller strip: down
    // through the player bar's own padding below the LCD frame, and up to (never into) the
    // subtitle's baseline box. Registering it *before* the subtitle also means the
    // artist / album links win the pointer wherever the two ever touch.
    //
    // `bar` is the player bar's own rect, so the strip ends at the bar's bottom edge and
    // never leaks into whatever the bar is mounted next to — which, since the bar moved to
    // the bottom of the window, is the window edge rather than the content area. Measured
    // at 1280 x 820: bar [757, 820], LCD [762.5, 814.5], painted seek row 15.5 px, grab
    // strip [795, 820] = 25 px — over the 24 px floor and inside the bar at both ends.
    let seek_rect = Rect::from_min_max(
        egui::pos2(text_left, y),
        egui::pos2(text_right, art_rect.bottom()),
    );
    let hit_rect = Rect::from_min_max(
        egui::pos2(text_left, subtitle_bottom),
        egui::pos2(text_right, bar.bottom()),
    );
    seek(ui, cx, state, seek_rect, hit_rect);

    subtitle(
        ui,
        cx,
        Sub {
            pos: subtitle_pos,
            track: id,
            artist,
            album,
            album_key: album_key.as_ref(),
            width,
        },
    );
}

/// What the LCD's second line needs.
struct Sub<'a> {
    pos: egui::Pos2,
    /// The loaded track — the artist link resolves through it, not through the tag text.
    track: phoebus_core::TrackId,
    artist: &'a str,
    album: &'a str,
    album_key: Option<&'a phoebus_core::AlbumKey>,
    width: f32,
}

/// `artist · album`, both clickable.
fn subtitle(ui: &mut egui::Ui, cx: &mut Ctx, sub: Sub<'_>) {
    let Sub {
        pos,
        track,
        artist,
        album,
        album_key,
        width,
    } = sub;
    let font = theme::font_small();
    let artist_g = widgets::truncated(ui, artist, font.clone(), theme::p().text_mid, width * 0.6);
    let sep_g = widgets::truncated(ui, theme::SEP, font.clone(), theme::p().text_low, width);
    let rest = (width - artist_g.size().x - sep_g.size().x).max(1.0);
    let album_g = widgets::truncated(ui, album, font, theme::p().text_mid, rest);

    let top = pos.y - artist_g.size().y * 0.5;
    let artist_rect = Rect::from_min_size(egui::pos2(pos.x, top), artist_g.size());
    let sep_x = artist_rect.right();
    let album_rect = Rect::from_min_size(egui::pos2(sep_x + sep_g.size().x, top), album_g.size());

    let artist_resp = ui.interact(artist_rect, ui.id().with("lcd-artist"), Sense::click());
    let album_resp = ui.interact(album_rect, ui.id().with("lcd-album"), Sense::click());

    let painter = ui.painter();
    painter.galley(
        artist_rect.min,
        artist_g,
        theme::hover_color(
            artist_resp.hovered(),
            theme::p().text_mid,
            theme::p().text_hi,
        ),
    );
    painter.galley(egui::pos2(sep_x, top), sep_g, theme::p().text_low);
    painter.galley(
        album_rect.min,
        album_g,
        theme::hover_color(
            album_resp.hovered(),
            theme::p().text_mid,
            theme::p().text_hi,
        ),
    );

    // Resolve on click, not per frame: the tag under the pointer is often not an artist
    // page (a remix credit, a guest), so the target is the album artist of this track.
    if artist_resp.clicked()
        && let Some(name) = nav::artist_target(cx.lib, track)
    {
        cx.act(Action::GoArtist(name));
        cx.act(Action::Go(View::Artists));
    }
    if album_resp.clicked()
        && let Some(key) = album_key
    {
        cx.act(Action::Go(View::Album(key.clone())));
    }
}

/// The LCD's two readouts: elapsed, and remaining as a negative.
///
/// Both go through [`nav::mmss`], so both roll into `H:MM:SS` past the hour (UI-SPEC v1.2
/// §Durations): a two-hour DJ set one minute in reads `1:05` / `-1:58:55`, never `-118:55`.
fn times(pos: Duration, duration: Duration) -> (String, String) {
    (
        nav::mmss(pos),
        format!("-{}", nav::mmss(duration.saturating_sub(pos))),
    )
}

/// `0:00 ───────── -3:09`, dragged to seek. `hit` is the taller grab strip (see [`lcd`]).
///
/// The track's ends are set from the *measured* timestamps rather than a fixed column, so
/// an hour-long file's wider `1:01:05` / `-58:55` pushes the track in instead of running
/// under it.
fn seek(ui: &mut egui::Ui, cx: &mut Ctx, state: &BarState, rect: Rect, hit: Rect) {
    let total = state.duration.as_secs_f32();
    let fraction = if total > 0.0 {
        (state.pos.as_secs_f32() / total).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (elapsed, remaining) = times(state.pos, state.duration);
    let color = theme::p().text_low;
    let elapsed = widgets::truncated(ui, &elapsed, theme::font_small(), color, f32::INFINITY);
    let remaining = widgets::truncated(ui, &remaining, theme::font_small(), color, f32::INFINITY);
    let (elapsed_w, remaining_w) = (elapsed.size().x, remaining.size().x);
    let top = |g: &Arc<Galley>| rect.center().y - g.size().y * 0.5;
    let painter = ui.painter_at(rect);
    painter.galley(egui::pos2(rect.left(), top(&elapsed)), elapsed, color);
    painter.galley(
        egui::pos2(rect.right() - remaining_w, top(&remaining)),
        remaining,
        color,
    );
    let (left, right) = (
        rect.left() + elapsed_w + theme::LCD_TIME_GAP,
        rect.right() - remaining_w - theme::LCD_TIME_GAP,
    );
    let bar_rect = Rect::from_min_max(
        egui::pos2(left, rect.top()),
        egui::pos2(right, rect.bottom()),
    );
    if bar_rect.width() < 8.0 {
        return;
    }
    // Same x-range, taller grab: the timestamps at either end stay untouched. Measured at
    // 1280 × 820: the painted track is 15.5 px, the grab strip 25 px.
    let hit_rect = Rect::from_min_max(egui::pos2(left, hit.top()), egui::pos2(right, hit.bottom()));
    let out = widgets::bar_at(
        ui,
        bar_rect,
        hit_rect,
        ui.id().with("seek"),
        widgets::BarValue {
            fraction,
            enabled: total > 0.0 && state.seekable,
        },
        widgets::BarStyle::seek(),
    );
    if let Some(v) = out.live {
        cx.act(Action::SeekLive(state.duration.mul_f32(v)));
    }
    if let Some(v) = out.commit {
        cx.act(Action::Seek(state.duration.mul_f32(v)));
    }
}

/// Idle and hover colours for a toggle glyph. An ON toggle is an accent-coloured *glyph on
/// a surface*, so both of its colours come from the `accent_text` side of the palette; on
/// the dark palette those are the accent and its dim, exactly as before.
fn toggle_colors(on: bool) -> (egui::Color32, egui::Color32) {
    if on {
        (theme::p().accent_text, theme::p().accent_text_dim)
    } else {
        (theme::p().text_low, theme::p().text_hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UI-SPEC v1.2 §Durations. The trap is the *remaining* side: it is built from a
    /// subtraction, so an hour-long track spends most of its play time with a remainder
    /// that is itself past the hour.
    #[test]
    fn lcd_times_pass_the_hour() {
        let t = |pos, dur| times(Duration::from_secs(pos), Duration::from_secs(dur));
        assert_eq!(t(65, 215), ("1:05".into(), "-2:30".into()));
        assert_eq!(t(0, 3599), ("0:00".into(), "-59:59".into()));
        // A two-hour set: each side crosses the hour in turn, neither wraps at 59:59.
        assert_eq!(t(65, 7200), ("1:05".into(), "-1:58:55".into()));
        assert_eq!(t(3665, 7200), ("1:01:05".into(), "-58:55".into()));
        // Exactly on the hour, and a position past the end (a stale progress tick).
        assert_eq!(t(3600, 3600), ("1:00:00".into(), "-0:00".into()));
        assert_eq!(t(99, 10), ("1:39".into(), "-0:00".into()));
    }

    /// UI-SPEC v1.2 §Player bar: the ensemble is centred as a unit and the LCD shrinks
    /// first, so the only way it can clip is if the two fixed groups plus their gaps no
    /// longer fit the narrowest window. They must leave the LCD more than the
    /// `LCD_ART * 2` it needs to stay drawn at all.
    #[test]
    fn ensemble_fits_the_minimum_window() {
        // The bar's usable width: the panel's inner margin eats `PANEL_PAD` on each side.
        let bar = theme::WINDOW_MIN[0] - 2.0 * theme::PANEL_PAD;
        // A deliberately generous speaker icon — the real one measures well under this.
        let sides = transport_w() + right_group_w(48.0) + 4.0 * theme::LCD_PAD;
        let lcd = bar - sides;
        assert!(lcd > theme::LCD_ART * 2.0, "LCD would vanish: {lcd} px");
        assert!(
            lcd >= theme::LCD_ART + 4.0 * theme::TIME_W,
            "cramped: {lcd} px"
        );
    }
}
