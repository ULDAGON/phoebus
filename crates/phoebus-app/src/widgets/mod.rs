//! Hand-painted building blocks. egui's stock widgets are too round and too grey for this
//! design, so anything the user actually looks at is drawn with `ui.painter()` against the
//! [`crate::theme`] tokens.

pub mod album_card;
pub mod equalizer;
pub mod menus;
pub mod player_bar;
pub mod queue;
pub mod song_picker;
pub mod song_row;

use std::sync::Arc;

use egui::{
    Align2, Color32, FontId, Galley, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
    text::LayoutJob,
};

use crate::theme;

// ---------------------------------------------------------------------------------------
// Track-row metrics (UI-SPEC v1.2 §Track rows)
// ---------------------------------------------------------------------------------------
//
// One set of numbers for all four list views — album detail, playlist, Songs, search — so
// a row is the same shape whichever page it is on. They live here rather than in
// `theme.rs` only because that file is being rewritten in parallel; they belong next to
// the other sizing constants once the dust settles.

/// Height of every track row.
pub const ROW_H: f32 = 40.0;
/// Width of the leading state column: track number, `▶`, `⏸` or the equalizer.
pub const LEAD_W: f32 = 28.0;
/// Gap between the leading state column and whatever the row starts with.
pub const LEAD_GAP: f32 = 16.0;
/// Artwork square in a playlist / search row, sized for [`ROW_H`].
pub const ROW_ART: f32 = 28.0;
/// Gap between the duration and the `⋯` button.
pub const DUR_GAP: f32 = 8.0;
/// Side of the `⋯` button's square hit rect (UI-SPEC §Feel's 24 px floor).
pub const MORE_W: f32 = 24.0;
/// Side of the favourite heart's square hit rect (UI-SPEC v1.3 §Favorites: "a 24 px HEART
/// column immediately LEFT of the duration"). Same square as [`MORE_W`], for the same
/// reason — it is the other end of the same 24 px floor.
pub const HEART_W: f32 = 24.0;
/// Gap between the heart column and the duration. [`DUR_GAP`]'s twin: the whole tail is one
/// rhythm of 24 px target, 8 px air, 40 px readout, 8 px air, 24 px target.
pub const HEART_GAP: f32 = DUR_GAP;
/// Side of one painted ellipsis dot.
pub const DOT: f32 = 3.0;
/// Centre-to-centre spacing of the ellipsis dots.
pub const DOT_STEP: f32 = 5.0;

/// Width the right end of a row reserves: heart, gap, duration, gap, `⋯` button.
///
/// One number for all five list views, which is what puts every heart in the app on one
/// right-aligned column whatever the row is otherwise made of (UI-SPEC v1.3 §Favorites).
pub fn tail_w() -> f32 {
    HEART_W + HEART_GAP + theme::TIME_W + DUR_GAP + MORE_W
}

// The four list views used to have three different row heights and two artwork sizes, all
// still named in `theme.rs` (which is being rewritten in parallel, so they are not deleted
// from here). These pin the direction of the v1.2 change — one row height, taller than the
// two it replaces and shorter than the fat playlist row; one artwork square, between the
// two it replaces — and keep the superseded names referenced until that cleanup lands.
const _: () = assert!(ROW_H > theme::ROW_TRACK && ROW_H > theme::ROW_SONG);
const _: () = assert!(ROW_H < theme::ROW_PLAYLIST);
const _: () = assert!(ROW_ART > theme::SONG_ART && ROW_ART < theme::PLAYLIST_ART);

/// Lay out one line of text, truncating with `…` at `max_width`.
///
/// egui caches galleys by content hash, so calling this every frame with the same string
/// is a hash lookup, not a re-layout.
pub fn truncated(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// Lay out `icon` and `text` as ONE galley: a [`theme`] icon at `icon_size`, then
/// [`theme::ICON_TEXT_GAP`], then the label in `font`.
///
/// Both runs are left uncoloured ([`Color32::PLACEHOLDER`]), so the caller decides the
/// colour at paint time (`painter.galley(pos, galley, color)`) and every interaction state
/// of a button shares one cache entry instead of one per colour.
///
/// The two runs are [`egui::Align::Center`]-aligned rather than left on egui's default
/// `BOTTOM`, and that is the whole point of the helper. `BOTTOM` aligns the two *boxes*,
/// which puts Phosphor's ink — it stops 0.035 em above its own descender line — about
/// 2 px below a Latin baseline at these sizes; centring puts the icon's ink centre
/// (0.513 of its box) within ~0.5 px of the capitals' (0.475 of theirs). Formatting the
/// icon into the string instead, as `format!("{GLYPH} PLAY")` used to, cannot do either:
/// one string is one size, so the icon was locked to the label's.
///
/// An empty `icon` produces the label alone, with no leading gap — Settings' `APPLY &
/// RESCAN` and the `DARK`/`LIGHT` pair are the same button shape with nothing to draw in
/// front of them, and inventing an icon for a phrase is worse than leaving it bare.
pub fn icon_text(ui: &Ui, icon: &str, icon_size: f32, text: &str, font: FontId) -> Arc<Galley> {
    let format = |font_id: FontId| egui::TextFormat {
        font_id,
        color: Color32::PLACEHOLDER,
        valign: egui::Align::Center,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let gap = if icon.is_empty() {
        0.0
    } else {
        job.append(icon, 0.0, format(theme::font_icon(icon_size)));
        theme::ICON_TEXT_GAP
    };
    job.append(text, gap, format(font));
    ui.painter().layout_job(job)
}

/// Paint one truncated line, left-aligned and vertically centred on `pos`.
pub fn text_left(
    ui: &Ui,
    pos: egui::Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) {
    let galley = truncated(ui, text, font, color, max_width);
    let y = pos.y - galley.size().y * 0.5;
    ui.painter().galley(egui::pos2(pos.x, y), galley, color);
}

/// An UPPERCASE micro-label: `Small`, `TEXT_LOW`, letter-spaced.
pub fn micro(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(spaced(text))
            .font(theme::font_small())
            .color(theme::p().text_low),
    );
}

/// A label in the heaviest weight the app has.
///
/// The bundled monospace face ships one weight, so `RichText::strong()` only changes the
/// colour — it does not embolden anything. Real weight therefore has to be faked: the same
/// galley is painted twice, [`theme::FAKE_BOLD`] apart, which thickens every stem by a
/// fraction of a pixel without smearing the glyph.
pub fn label_bold(ui: &mut Ui, text: &str, font: FontId, color: Color32) {
    let galley = truncated(ui, text, font, color, ui.available_width());
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let painter = ui.painter();
    painter.galley(rect.min, galley.clone(), color);
    painter.galley(rect.min + Vec2::new(theme::FAKE_BOLD, 0.0), galley, color);
}

/// Insert hair spaces between characters — the cheap terminal-ish letter-spacing the spec
/// asks for. Only ever applied to short static labels.
pub fn spaced(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// How an [`icon_button`] is sized and coloured.
#[derive(Clone, Copy, Debug)]
pub struct Icon {
    /// Glyph font size.
    pub size: f32,
    /// Side of the square hit rect, raised to at least [`theme::HIT_MIN`].
    pub side: f32,
    /// Idle colour.
    pub idle: Color32,
    /// Hover colour.
    pub hover: Color32,
}

impl Icon {
    /// An auto-sized icon: the hit rect grows with the glyph, floored at
    /// [`theme::HIT_MIN`]. What every icon button except the transport row uses.
    pub fn new(size: f32, idle: Color32, hover: Color32) -> Icon {
        Icon {
            size,
            side: (size + theme::ICON_PAD).max(theme::HIT_MIN),
            idle,
            hover,
        }
    }

    /// Fix the hit rect to an exact square — the transport row, where UI-SPEC wants all
    /// three buttons identical whatever their glyph measures.
    pub fn sized(self, side: f32) -> Icon {
        Icon {
            side: side.max(theme::HIT_MIN),
            ..self
        }
    }
}

/// An icon-only button: a square hit target of at least [`theme::HIT_MIN`], a centred
/// glyph, and a tooltip.
///
/// There is no optical nudge. There used to be one, for the play button alone, because its
/// glyph came from a different fallback face than the two beside it (see
/// [`theme::ICON_TRANSPORT`]); with one icon face the whole set shares a centre line and
/// [`Align2::CENTER_CENTER`] on the hit rect is the correct answer for all of them.
pub fn icon_button(ui: &mut Ui, glyph: &str, icon: Icon, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(icon.side), Sense::click());
    let color = theme::hover_color(response.hovered(), icon.idle, icon.hover);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        theme::font_icon(icon.size),
        color,
    );
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(
            egui::RichText::new(tooltip)
                .font(theme::font_small())
                .color(theme::p().text_mid),
        )
    }
}

/// Which of the three button looks to paint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// ACCENT fill, ON_ACCENT text.
    Primary,
    /// BG2 fill, hairline, TEXT_HI.
    Secondary,
    /// BG2 fill, hairline, TEXT_LOW, no hover state and no click.
    Disabled,
}

/// `PLAY` behind its icon — the one filled-yellow button shape in the app.
pub fn primary_button(ui: &mut Ui, icon: &str, label: &str) -> Response {
    button(ui, icon, label, Kind::Primary)
}

/// `SHUFFLE` behind its icon — BG2 fill, hairline, `TEXT_HI`.
pub fn secondary_button(ui: &mut Ui, icon: &str, label: &str) -> Response {
    button(ui, icon, label, Kind::Secondary)
}

/// The same shape with nothing behind it: greyed, unhoverable, and it says why.
///
/// UI-SPEC reserves the accent for "active / playing / primary action", so a button that
/// cannot do anything must not wear it — and must not report clicks either.
pub fn disabled_button(ui: &mut Ui, icon: &str, label: &str, tooltip: &str) -> Response {
    let response = button(ui, icon, label, Kind::Disabled);
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(
            egui::RichText::new(tooltip)
                .font(theme::font_small())
                .color(theme::p().text_mid),
        )
    }
}

fn button(ui: &mut Ui, icon: &str, label: &str, kind: Kind) -> Response {
    let galley = icon_text(ui, icon, theme::ICON_SMALL, label, theme::font_body());
    let size = Vec2::new(
        galley.size().x + 28.0,
        (galley.size().y + 14.0).max(theme::HIT_MIN + 4.0),
    );
    let sense = if kind == Kind::Disabled {
        Sense::hover()
    } else {
        Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter();
    let hovered = response.hovered();
    let (fill, text_color) = match (kind, hovered) {
        (Kind::Primary, false) => (theme::p().accent, theme::p().on_accent),
        (Kind::Primary, true) => (theme::p().accent_dim, theme::p().on_accent),
        (Kind::Secondary, false) => (theme::p().bg2, theme::p().text_hi),
        (Kind::Secondary, true) => (theme::p().border, theme::p().text_hi),
        (Kind::Disabled, _) => (theme::p().bg2, theme::p().text_low),
    };
    painter.rect(
        rect,
        theme::corner(),
        fill,
        if kind == Kind::Primary {
            Stroke::NONE
        } else {
            theme::hairline()
        },
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() * 0.5, galley, text_color);
    response
}

/// What a [`bar_at`] reported this frame.
#[derive(Clone, Debug)]
pub struct BarOut {
    /// Fraction under the pointer while dragging (0.0..=1.0).
    pub live: Option<f32>,
    /// Fraction to commit: the drag was released, or the track was clicked.
    pub commit: Option<f32>,
    /// The bar's own response — hover state, tooltips.
    pub response: Response,
}

/// How a flat bar is painted. The track is always [`theme::BAR_TRACK_H`] of `BORDER`;
/// what differs between the seek bar and the volume bar is the fill and the knob.
#[derive(Clone, Copy, Debug)]
pub struct BarStyle {
    /// Colour of the filled (elapsed / current) part.
    pub fill: Color32,
    /// Knob colour at rest, or `None` for a knob that only appears on hover.
    pub knob: Option<Color32>,
    /// Knob colour while hovered or dragged.
    pub knob_hot: Color32,
}

impl BarStyle {
    /// The LCD's seek bar: raw-accent fill (it is a filled area, and the accent is the
    /// point of it), knob only while the pointer is on it. The knob is a 6 px marker, so it
    /// takes `accent_text` — on a light palette a raw neon knob would vanish.
    pub fn seek() -> BarStyle {
        BarStyle {
            fill: theme::p().accent,
            knob: None,
            knob_hot: theme::p().accent_text,
        }
    }

    /// The volume bar: `text_mid` fill and a `text_hi` knob that is ALWAYS visible, so the
    /// level can be read without hovering. The accent is reserved for the interaction.
    pub fn volume() -> BarStyle {
        BarStyle {
            fill: theme::p().text_mid,
            knob: Some(theme::p().text_hi),
            knob_hot: theme::p().accent_text,
        }
    }
}

/// A flat progress/seek/volume bar: 3 px `BORDER` track, `style` fill, square knob. Drawn
/// by hand because egui's `Slider` is far too round for this design.
///
/// `rect` is where the bar is *painted* (its width is the 0..1 range); `hit` is what the
/// pointer can grab, which UI-SPEC §Feel wants at least [`theme::HIT_MIN`] tall even
/// though the track itself is 3 px. The two share an x-range; only the height differs.
/// The caller supplies both because the player bar lays its own row out.
pub fn bar_at(
    ui: &mut Ui,
    rect: Rect,
    hit: Rect,
    id: egui::Id,
    value: BarValue,
    style: BarStyle,
) -> BarOut {
    let BarValue { fraction, enabled } = value;
    let response = ui.interact(
        hit,
        id,
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    paint_bar(
        ui,
        rect,
        fraction,
        style,
        (response.hovered() || response.dragged()) && enabled,
    );
    let value_at = |x: f32| ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let pointer = response.interact_pointer_pos().map(|p| value_at(p.x));
    let live = pointer.filter(|_| enabled && response.dragged());
    let commit = pointer.filter(|_| enabled && (response.drag_stopped() || response.clicked()));
    BarOut {
        live,
        commit,
        response,
    }
}

/// What a bar currently reads, and whether it may be dragged at all.
#[derive(Clone, Copy, Debug)]
pub struct BarValue {
    /// Where the fill ends, 0.0..=1.0.
    pub fraction: f32,
    /// A track the decoder refuses to seek gets a dead scrubber rather than one that
    /// silently does nothing.
    pub enabled: bool,
}

/// Paint a flat bar into `rect` without any interaction. `hot` is "hovered or dragged".
pub fn paint_bar(ui: &Ui, rect: Rect, fraction: f32, style: BarStyle, hot: bool) {
    let fraction = fraction.clamp(0.0, 1.0);
    let painter = ui.painter_at(rect);
    let y = rect.center().y;
    let track = Rect::from_min_max(
        egui::pos2(rect.left(), y - theme::BAR_TRACK_H * 0.5),
        egui::pos2(rect.right(), y + theme::BAR_TRACK_H * 0.5),
    );
    painter.rect_filled(track, egui::CornerRadius::ZERO, theme::p().border);
    let filled_x = rect.left() + rect.width() * fraction;
    if fraction > 0.0 {
        painter.rect_filled(
            Rect::from_min_max(track.min, egui::pos2(filled_x, track.max.y)),
            egui::CornerRadius::ZERO,
            style.fill,
        );
    }
    let knob = if hot {
        Some(style.knob_hot)
    } else {
        style.knob
    };
    if let Some(color) = knob {
        // Kept inside the track's x-range: at 0 % and 100 % a centred square would hang
        // half of itself off the end of the bar.
        let half = theme::BAR_KNOB * 0.5;
        let cx = filled_x.clamp(rect.left() + half, rect.right() - half);
        painter.rect_filled(
            Rect::from_min_max(
                egui::pos2(cx - half, y - half),
                egui::pos2(cx + half, y + half),
            ),
            egui::CornerRadius::ZERO,
            color,
        );
    }
}

/// A full-width row that fills with `BG2` on hover — the base of every list row.
pub fn row(ui: &mut Ui, height: f32, sense: Sense) -> (Rect, Response) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    (rect, response)
}

/// Fill a row rect for its interaction state (hover = `BG2`, playing = accent wash).
pub fn row_background(ui: &Ui, rect: Rect, hovered: bool, playing: bool) {
    let fill = match (playing, hovered) {
        (true, _) => theme::p().selection_bg,
        (false, true) => theme::p().bg2,
        (false, false) => return,
    };
    ui.painter().rect_filled(rect, theme::corner(), fill);
}

/// A 1 px horizontal `BORDER` separator across `rect`'s bottom edge.
pub fn hairline_bottom(ui: &Ui, rect: Rect) {
    hairline_bottom_from(ui, rect, rect.left());
}

/// The same separator, starting at `x` instead of at the row's left edge.
///
/// UI-SPEC v1.2 §Track rows wants every list divider to begin at the TITLE column, leaving
/// the leading state column (track number, `▶`, equalizer) hanging outside the ruled part
/// of the table — the Apple Music look. Nothing else changes: still 1 px, still `BORDER`,
/// still on the pixel centre so it does not smear across two rows.
pub fn hairline_bottom_from(ui: &Ui, rect: Rect, x: f32) {
    ui.painter().line_segment(
        [
            egui::pos2(x.min(rect.right()), rect.bottom() - 0.5),
            egui::pos2(rect.right(), rect.bottom() - 0.5),
        ],
        theme::hairline(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the empty-playlist header leans on: a disabled button is not merely painted
    /// grey, it does not sense clicks at all, so a press cannot be silently swallowed the
    /// way the old accent-filled `▶ PLAY` swallowed it.
    #[test]
    fn a_disabled_button_does_not_sense_clicks() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(600.0, 200.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let live = primary_button(ui, theme::GLYPH_PLAY, "PLAY");
            let dead = disabled_button(ui, theme::GLYPH_PLAY, "PLAY", "NOTHING TO PLAY YET");
            assert!(live.sense.senses_click());
            assert!(!dead.sense.senses_click(), "nothing to click");
            assert!(!dead.clicked());
            assert_eq!(
                live.rect.size(),
                dead.rect.size(),
                "the two states must not shift the layout"
            );
        });
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        out.textures_delta.clear();
    }

    /// UI-SPEC §Player bar: `⏮ ▶/⏸ ⏭` are three IDENTICAL square hit rects in one
    /// vertically-centred row with equal gaps. The play button is the one that can break
    /// this — it swaps glyph mid-row — and neither of its two states may move or resize
    /// its target.
    #[test]
    fn the_transport_buttons_are_identical_squares() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(600.0, 200.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = theme::TRANSPORT_GAP;
                let step = Icon::new(
                    theme::ICON_TRANSPORT,
                    theme::p().text_hi,
                    theme::p().accent_text,
                )
                .sized(theme::TRANSPORT_HIT);
                let prev = icon_button(ui, theme::GLYPH_PREV, step, "PREVIOUS");
                let play = icon_button(ui, theme::GLYPH_PLAY, step, "PLAY");
                let pause = icon_button(ui, theme::GLYPH_PAUSE, step, "PAUSE");
                let next = icon_button(ui, theme::GLYPH_NEXT, step, "NEXT");

                let square = Vec2::splat(theme::TRANSPORT_HIT);
                for (name, response) in [
                    ("prev", &prev),
                    ("play", &play),
                    ("pause", &pause),
                    ("next", &next),
                ] {
                    assert_eq!(
                        response.rect.size(),
                        square,
                        "{name} is not the same square"
                    );
                    assert_eq!(
                        response.rect.center().y,
                        prev.rect.center().y,
                        "{name} is off the row's centre line"
                    );
                }
                assert_eq!(play.rect.left() - prev.rect.right(), theme::TRANSPORT_GAP);
                assert_eq!(next.rect.left() - pause.rect.right(), theme::TRANSPORT_GAP);
            });
        });
        out.textures_delta.clear();
    }

    /// [`icon_text`] must produce ONE row whose icon is sized independently of the label,
    /// and must add nothing at all when there is no icon.
    ///
    /// The independence is the whole reason the helper exists: `format!("{GLYPH} PLAY")`
    /// laid the icon out at the label's size, which is what made the back row's `←` look
    /// like a typo next to `ALBUMS`.
    #[test]
    fn an_icon_label_is_one_row_sized_independently_of_its_label() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(600.0, 200.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let font = theme::font_small();
            let bare = icon_text(ui, "", theme::ICON_INLINE, "ALBUMS", font.clone());
            let small = icon_text(ui, theme::GLYPH_BACK, 10.0, "ALBUMS", font.clone());
            let large = icon_text(ui, theme::GLYPH_BACK, 20.0, "ALBUMS", font);
            for (name, g) in [("bare", &bare), ("small", &small), ("large", &large)] {
                assert_eq!(g.rows.len(), 1, "{name} wrapped");
            }
            // An empty icon costs nothing — not even the gap.
            assert_eq!(
                bare.size().x,
                truncated(
                    ui,
                    "ALBUMS",
                    theme::font_small(),
                    Color32::WHITE,
                    f32::INFINITY
                )
                .size()
                .x,
                "an empty icon still took room"
            );
            // Phosphor advances exactly one em, so the widths differ by exactly the size
            // difference: the icon is following its own size, not the label's.
            assert_eq!(
                large.size().x - small.size().x,
                10.0,
                "the icon did not follow `icon_size` ({} vs {})",
                small.size().x,
                large.size().x
            );
            assert_eq!(
                small.size().x - bare.size().x,
                10.0 + theme::ICON_TEXT_GAP,
                "the icon and its gap are not both in the measured width"
            );
        });
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        out.textures_delta.clear();
    }

    /// The volume knob is the readout, so it must be painted whether or not the pointer is
    /// anywhere near it — unlike the seek knob, which only appears on hover.
    #[test]
    fn only_the_volume_bar_keeps_its_knob_at_rest() {
        assert_eq!(BarStyle::volume().knob, Some(theme::p().text_hi));
        assert_eq!(BarStyle::volume().fill, theme::p().text_mid);
        assert_eq!(BarStyle::seek().knob, None);
        assert_eq!(BarStyle::seek().fill, theme::p().accent);
    }
}
