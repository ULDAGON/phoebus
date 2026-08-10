//! Every design token in Phoebus, plus the one function that pours them into egui.
//!
//! This module is the single source of truth for colour, type and geometry: **no other
//! file in the crate may contain a colour or a layout literal**. If a number matters
//! twice, it lives here.
//!
//! Colour is a *runtime* value. A [`Palette`] is derived from a [`ThemeMode`] and one accent
//! colour and published process-wide, so every call site reads `theme::p().text_hi` instead
//! of a constant. Type, geometry, glyphs and timings stay `const`: they do not depend on the
//! theme.
//!
//! The dark palette is the UI-SPEC v1.2 one: blue-slate surfaces, three greys, one yellow.
//! The accent is rationed whatever its hue — it may only ever mean *playing*, *active* or
//! *primary action*. When nothing is playing the app is strictly neutral.

use std::sync::{OnceLock, PoisonError, RwLock};

use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Vec2,
};
use phoebus_core::{AppState, ThemeMode};

// ---------------------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------------------

/// UI-SPEC v1.2's yellow — the accent of a fresh install.
pub const DEFAULT_ACCENT: Color32 = Color32::from_rgb(0xFF, 0xFB, 0x00);

/// Accents that used to be [`DEFAULT_ACCENT`] in an earlier build. A `state.json` still
/// holding one of these belongs to a user who never picked an accent at all, so it is
/// migrated to the current default rather than treated as a deliberate choice
/// (UI-SPEC v1.2 §Colors).
pub const LEGACY_ACCENTS: [Color32; 1] = [Color32::from_rgb(0xE8, 0xFF, 0x2E)];

/// WCAG AA for body text. [`Palette::accent_text`] is moved away from `bg0` until it clears
/// this ratio.
const MIN_CONTRAST: f32 = 4.5;

/// How far a `*_dim` hover colour is moved toward `bg0`.
///
/// 0.40 rather than a round third: it is the fraction that reproduced UI-SPEC v1.1's
/// documented `ACCENT_DIM` (`#8F9E1B` from `#E8FF2E` over `#0A0A0A`, within 5 per channel),
/// and it kept the same weight when the palette moved to blue-slate — a hover that is
/// visibly dimmer than the fill without losing the hue.
const DIM_MIX: f32 = 0.40;

/// Alpha of the accent wash behind a selected / playing row (~12 %, UI-SPEC §Design tokens).
const SELECTION_ALPHA: u8 = 31;
/// Alternate-row stripe on a dark surface: white at ~2 %.
const STRIPE_ALPHA_DARK: u8 = 5;
/// …and on a light one: black at ~3 %. White over paper is invisible, so the stripe has to
/// change sign with the mode; the weight is matched by eye, not by alpha.
const STRIPE_ALPHA_LIGHT: u8 = 8;

/// Alpha of the [`Palette::scrim`] pad behind an outline glyph on artwork (~43 %).
///
/// Heavy for a "subtle" scrim because it is doing a job no surface token does: a 24 px
/// outline has to survive over a cover that may be white, black or a photograph of a
/// crowd, and it is only ever painted inside the glyph's own rounded box, so what looks
/// like a lot of alpha is a 24 px square of it in the corner of a 180 px cover.
const SCRIM_ALPHA: u8 = 110;

/// Text painted on a light accent fill.
const NEAR_BLACK: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0A);
/// Text painted on a dark accent fill.
const NEAR_WHITE: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF2);

/// Every colour the UI can paint with, resolved for one mode and one accent.
///
/// `Copy`, and small enough that call sites take a whole palette (`theme::p()`) rather than
/// reaching for individual tokens through a lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Which mode this palette was built for.
    pub mode: ThemeMode,
    /// Window background.
    pub bg0: Color32,
    /// Sidebar and player bar.
    pub bg1: Color32,
    /// Cards, hover fills, inputs.
    pub bg2: Color32,
    /// 1 px hairlines everywhere.
    pub border: Color32,
    /// Primary text.
    pub text_hi: Color32,
    /// Secondary text (artist names, counts).
    pub text_mid: Color32,
    /// Tertiary text (section labels, timestamps).
    pub text_low: Color32,
    /// The accent exactly as the user chose it. **Fills only**: seek fill, the `▶ PLAY`
    /// button, the album-card badge, the hovered-cover outline, Settings swatches.
    pub accent: Color32,
    /// Hovered / pressed form of an accent *fill*.
    pub accent_dim: Color32,
    /// The accent as **text or a glyph on a surface**: the accent moved away from `bg0`
    /// until it reaches [`MIN_CONTRAST`]. In dark mode a neon accent already clears the bar
    /// and this is the accent itself; on paper it is the same hue, darkened.
    pub accent_text: Color32,
    /// Hovered form of [`Palette::accent_text`].
    pub accent_text_dim: Color32,
    /// Text on an accent fill: near-black on a light accent, near-white on a dark one.
    pub on_accent: Color32,
    /// Selected / playing row wash: the accent at ~12 %.
    pub selection_bg: Color32,
    /// Alternate-row stripe.
    pub stripe: Color32,
    /// A translucent pad painted *on artwork*, under a glyph that has to stay legible over
    /// a photograph — the album card's outline heart (UI-SPEC v1.3 §Favorites: "a subtle
    /// dark scrim behind it for legibility on busy art").
    ///
    /// Like [`Palette::stripe`] it changes sign with the mode, and for the same reason: the
    /// glyph on top of it is `text_hi`, which is near-white in dark mode and near-black on
    /// paper. A pad that darkened the cover under a near-black heart would hide it.
    pub scrim: Color32,
}

/// Resolve the palette for a mode and an accent.
///
/// Pure: the same inputs always give the same colours, which is what makes the contrast
/// rules testable without a window.
pub fn palette(mode: ThemeMode, accent: Color32) -> Palette {
    // UI-SPEC v1.2 §Colors. Dark is blue-slate: `bg1` (sidebar, player bar) is *darker*
    // than `bg0` (the content views), so the chrome recedes and the router reads as the lit
    // surface. Text tokens are the v1.1 ones, unchanged.
    let (bg0, bg1, bg2, border, text_hi, text_mid, text_low) = match mode {
        ThemeMode::Dark => (
            Color32::from_rgb(0x0E, 0x13, 0x1C),
            Color32::from_rgb(0x0A, 0x0E, 0x15),
            Color32::from_rgb(0x16, 0x1D, 0x2A),
            Color32::from_rgb(0x20, 0x28, 0x39),
            Color32::from_rgb(0xF2, 0xF2, 0xF2),
            Color32::from_rgb(0x9A, 0x9A, 0x9A),
            Color32::from_rgb(0x57, 0x57, 0x57),
        ),
        ThemeMode::Light => (
            Color32::from_rgb(0xD4, 0xD4, 0xD4),
            Color32::from_rgb(0xC8, 0xC8, 0xC8),
            Color32::from_rgb(0xDE, 0xDE, 0xDE),
            Color32::from_rgb(0xB6, 0xB6, 0xB6),
            Color32::from_rgb(0x14, 0x14, 0x14),
            Color32::from_rgb(0x5A, 0x5A, 0x5A),
            Color32::from_rgb(0x9A, 0x9A, 0x98),
        ),
    };
    let accent_text = readable(accent, bg0);
    let (stripe, scrim) = match mode {
        ThemeMode::Dark => (
            Color32::from_rgba_unmultiplied(255, 255, 255, STRIPE_ALPHA_DARK),
            Color32::from_rgba_unmultiplied(0, 0, 0, SCRIM_ALPHA),
        ),
        ThemeMode::Light => (
            Color32::from_rgba_unmultiplied(0, 0, 0, STRIPE_ALPHA_LIGHT),
            Color32::from_rgba_unmultiplied(255, 255, 255, SCRIM_ALPHA),
        ),
    };
    Palette {
        mode,
        bg0,
        bg1,
        bg2,
        border,
        text_hi,
        text_mid,
        text_low,
        accent,
        accent_dim: mix(accent, bg0, DIM_MIX),
        accent_text,
        accent_text_dim: mix(accent_text, bg0, DIM_MIX),
        on_accent: on_accent(accent),
        selection_bg: Color32::from_rgba_unmultiplied(
            accent.r(),
            accent.g(),
            accent.b(),
            SELECTION_ALPHA,
        ),
        stripe,
        scrim,
    }
}

/// Move `color` away from `bg` until it reads as text on it.
///
/// UI-SPEC: *the accent darkened toward black until it reaches ≥ 4.5:1 against BG0*. That is
/// written for the light palette, where black is the end of the scale furthest from the
/// background; on a dark background the same sentence means white. The direction is
/// therefore whichever extreme has more contrast with `bg` — the colour's own luminance is
/// no guide, since a *black* accent on `#0E131C` is darker than the background and still has
/// to be lightened.
///
/// A colour that already clears the bar — the default yellow on `#0E131C` at ~16.9:1 — is
/// returned untouched, hue and all.
fn readable(color: Color32, bg: Color32) -> Color32 {
    if contrast(color, bg) >= MIN_CONTRAST {
        return color;
    }
    let target = if contrast(Color32::BLACK, bg) >= contrast(Color32::WHITE, bg) {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    // `target` clears 4.5 against either `bg0` and "passes" is monotone in `t` once the
    // t = 0 case has been ruled out above, so this converges on the smallest move that
    // works — the accent keeps as much of its hue as legibility allows.
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if contrast(mix(color, target, mid), bg) >= MIN_CONTRAST {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(color, target, hi)
}

/// Whichever of near-black / near-white reads better on `accent`.
fn on_accent(accent: Color32) -> Color32 {
    if contrast(NEAR_BLACK, accent) >= contrast(NEAR_WHITE, accent) {
        NEAR_BLACK
    } else {
        NEAR_WHITE
    }
}

/// Blend `t` of `b` into `a` (both opaque), in sRGB space.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// WCAG 2.x relative luminance of an opaque colour.
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG 2.x contrast ratio between two opaque colours, 1.0..=21.0.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

// ---------------------------------------------------------------------------------------
// The live palette
// ---------------------------------------------------------------------------------------

static CURRENT: OnceLock<RwLock<Palette>> = OnceLock::new();

fn current() -> &'static RwLock<Palette> {
    CURRENT.get_or_init(|| RwLock::new(palette(ThemeMode::Dark, DEFAULT_ACCENT)))
}

/// The palette the UI is painting with right now.
///
/// Deliberately one letter: it is read a few hundred times per frame and every call site
/// reads it inline (`theme::p().text_mid`). A poisoned lock hands the palette back anyway —
/// a colour is never worth a panic in a paint loop.
pub fn p() -> Palette {
    *current().read().unwrap_or_else(PoisonError::into_inner)
}

/// Publish a new palette process-wide. Callers holding a [`Context`] want [`apply`], which
/// also rebuilds egui's own styling.
pub fn set(mode: ThemeMode, accent: Color32) -> Palette {
    let next = palette(mode, accent);
    *current().write().unwrap_or_else(PoisonError::into_inner) = next;
    next
}

// ---------------------------------------------------------------------------------------
// PHOEBUS_THEME
// ---------------------------------------------------------------------------------------

/// Environment override for one run: `dark`, `light`, `dark,#RRGGBB` or `light,#RRGGBB`
/// (ARCHITECTURE.md). It never reaches `state.json` — the same deal
/// [`crate::controller::ENV_START_MUTED`] gets for the volume.
pub const ENV_THEME: &str = "PHOEBUS_THEME";

/// What `PHOEBUS_THEME` asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Override {
    /// The mode to paint this run.
    pub mode: ThemeMode,
    /// The accent to paint this run, or `None` to keep the persisted one.
    pub accent: Option<Color32>,
}

/// Parse a `PHOEBUS_THEME` value. `None` for anything that is not `mode[,#RRGGBB]`.
pub fn parse_override(raw: &str) -> Option<Override> {
    let (mode, accent) = match raw.split_once(',') {
        Some((mode, accent)) => (mode, Some(accent)),
        None => (raw, None),
    };
    let mode = ThemeMode::parse(mode)?;
    let accent = match accent {
        Some(text) => Some(color(phoebus_core::parse_hex_color(text)?)),
        None => None,
    };
    Some(Override { mode, accent })
}

/// Read and parse `PHOEBUS_THEME`. Silent — [`resolve`] does the talking, because the
/// controller asks this question too and one run should not log the answer twice.
pub fn env_override() -> Option<Override> {
    parse_override(&std::env::var(ENV_THEME).ok()?)
}

/// The mode and accent this run should paint with: `PHOEBUS_THEME` if it is set and valid,
/// otherwise what `state.json` remembers.
///
/// Called once, at start-up, and the only place the override is announced — including when
/// it is malformed, which is a typo worth a warning rather than a silent fall-back to dark.
pub fn resolve(state: &AppState) -> (ThemeMode, Color32) {
    let saved = state
        .accent_rgb()
        .map_or(DEFAULT_ACCENT, |rgb| migrate(color(rgb)));
    let raw = std::env::var(ENV_THEME).ok();
    match raw.as_deref().map(|raw| (raw, parse_override(raw))) {
        Some((raw, Some(over))) => {
            log::info!("{ENV_THEME}={raw} overrides the saved theme for this run (not saved)");
            (over.mode, over.accent.unwrap_or(saved))
        }
        Some((raw, None)) => {
            log::warn!("{ENV_THEME}={raw:?} is not `dark`/`light`[,#RRGGBB]; ignoring it");
            (state.theme_mode, saved)
        }
        None => (state.theme_mode, saved),
    }
}

/// Carry a saved accent forward across a change of default.
///
/// An accent that is bit-for-bit a *previous* default was never chosen — it is what a
/// fresh install wrote before this build existed. Handing it back unchanged would freeze
/// every existing user on the old yellow forever, so it is remapped to [`DEFAULT_ACCENT`];
/// anything else, including a colour the user happened to pick that equals the *current*
/// default, is left exactly as it was found.
///
/// Applied in [`resolve`], i.e. at the one point where `state.json` becomes the live
/// palette, so the migrated value flows on through the ordinary save path.
pub fn migrate(accent: Color32) -> Color32 {
    if LEGACY_ACCENTS.contains(&accent) {
        DEFAULT_ACCENT
    } else {
        accent
    }
}

/// `[r, g, b]` (core's colour currency) as an egui colour.
pub fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// An egui colour as `[r, g, b]`, for the round trip back into `state.json`.
pub fn rgb(color: Color32) -> [u8; 3] {
    [color.r(), color.g(), color.b()]
}

// ---------------------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------------------

/// View titles.
pub const SIZE_HEADING: f32 = 24.0;
/// Album titles on detail pages.
pub const SIZE_SUB: f32 = 16.0;
/// Body text.
pub const SIZE_BODY: f32 = 13.5;
/// Micro-labels, timestamps, counts.
pub const SIZE_SMALL: f32 = 11.0;
/// Button labels.
pub const SIZE_BUTTON: f32 = 13.5;
/// Sidebar section labels — one step below [`SIZE_SMALL`] (UI-SPEC v1.2 §Sidebar sections),
/// so `LIBRARY` and `PLAYLISTS` read as headings *of* their rows rather than as more rows.
pub const SIZE_MICRO: f32 = 10.0;
/// Context-menu and submenu items — one step below [`SIZE_BODY`] (UI-SPEC v1.2 §Menus).
pub const SIZE_MENU: f32 = 11.5;

/// Name of the extra `Sub` text style (16 px) registered by [`install`].
pub const SUB_STYLE: &str = "Sub";

/// 24 px monospace.
pub fn font_heading() -> FontId {
    FontId::new(SIZE_HEADING, FontFamily::Monospace)
}

/// 16 px monospace.
pub fn font_sub() -> FontId {
    FontId::new(SIZE_SUB, FontFamily::Monospace)
}

/// 13.5 px monospace.
pub fn font_body() -> FontId {
    FontId::new(SIZE_BODY, FontFamily::Monospace)
}

/// 11 px monospace.
pub fn font_small() -> FontId {
    FontId::new(SIZE_SMALL, FontFamily::Monospace)
}

/// 10 px monospace — sidebar section labels.
pub fn font_micro() -> FontId {
    FontId::new(SIZE_MICRO, FontFamily::Monospace)
}

/// 11.5 px monospace — every context-menu item.
pub fn font_menu() -> FontId {
    FontId::new(SIZE_MENU, FontFamily::Monospace)
}

/// A glyph-sized monospace font.
///
/// Every `ICON_*` in this file is passed through here. The size lands on the Phosphor face
/// rather than on the mono one because the only codepoints ever handed to it are the
/// `GLYPH_*` Private-Use ones, which no other bundled face carries (see [`install_fonts`]).
pub fn font_icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// The same glyph-sized font, on the **filled** Phosphor face ([`ICON_FILL_FONT`]).
///
/// A family of its own rather than a fallback, because the two Phosphor weights map the
/// *same* codepoints (`heart` is U+E2A8 in both) and a fallback chain stops at the first
/// face that answers — see [`install_fonts`]. Nothing but the icons this returns can reach
/// that family, so no text in the app can accidentally land on it.
pub fn font_icon_fill(size: f32) -> FontId {
    FontId::new(size, icon_fill_family())
}

/// The egui family [`ICON_FILL_FONT`] is the sole member of.
pub fn icon_fill_family() -> FontFamily {
    FontFamily::Name(ICON_FILL_FAMILY.into())
}

/// Noto Sans JP Regular — the Japanese subset of Source Han Sans, static (non-variable) CFF.
///
/// Source (downloaded verbatim, 4 533 028 bytes, SHA-256
/// `dff723ba59d57d136764a04b9b2d03205544f7cd785a711442d6d2d085ac5073`):
/// <https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf>
///
/// Licensed under the SIL Open Font License 1.1 — the licence ships next to the file in
/// `assets/LICENSE-NotoSansJP.txt`.
///
/// The language-specific *subset* OTF rather than the pan-CJK `NotoSansCJKjp` (≈16 MB) or the
/// variable `NotoSansJP[wght].ttf` (≈9.6 MB): it is the smallest single file that covers
/// Japanese kana + kanji, and being static there is no default-instance question for the
/// rasteriser to get wrong.
const CJK_FONT: &[u8] = include_bytes!("../assets/NotoSansJP-Regular.otf");

/// The key [`CJK_FONT`] is registered under. egui dedupes [`Context::add_font`] by this name,
/// so re-installing is a map lookup.
const CJK_FONT_NAME: &str = "NotoSansJP";

/// Japanese text that must render — one hiragana, one katakana, one kanji, one full-width
/// punctuation mark. Real strings out of the Apple Music library (`ANALOG レアリティ`,
/// `死のダンス`).
#[cfg(test)]
const CJK_SAMPLE: &str = "のレ死、";

/// Phosphor Icons, Regular weight — the app's entire icon set, in one face.
///
/// Source (downloaded verbatim, 488 636 bytes, SHA-256
/// `06b91e022b7ee899a63efced879392a74f0bacbda54e4467e9f663220d173a10`):
/// <https://raw.githubusercontent.com/phosphor-icons/web/master/src/regular/Phosphor.ttf>
///
/// Licensed **MIT** (© 2020–2021 Phosphor Icons) — the licence ships next to the file in
/// `assets/LICENSE-Phosphor.txt`.
///
/// The whole 1 513-glyph face rather than a subset: subsetting would make the asset a
/// build artefact of this repository instead of a file anyone can re-download and
/// checksum, and 0.5 MB next to the 4.5 MB CJK fallback buys nothing worth that.
///
/// Every *icon* lives in the Private Use Area (U+E000..U+EE83), which is exactly why the
/// face can be a *fallback* rather than a family of its own: no real text can reach an
/// icon, and no icon can be reached by real text.
///
/// The face is not, however, purely PUA. It also maps `a`–`z`, `-` and the space, because
/// upstream it is driven by `liga` ligatures — a page writes `ph-play` and the font folds
/// it into one glyph — and those letters are blanks. They are harmless here only because
/// [`install_fonts`] inserts the face at the *lowest* priority, behind a mono face that
/// covers all of ASCII; `tests::japanese_renders_without_disturbing_the_mono_face` measures
/// every printable ASCII character to keep it that way.
const ICON_FONT: &[u8] = include_bytes!("../assets/Phosphor.ttf");

/// The key [`ICON_FONT`] is registered under, on the same dedupe-by-name terms as
/// [`CJK_FONT_NAME`].
const ICON_FONT_NAME: &str = "Phosphor";

/// Phosphor Icons, **Fill** weight — the solid counterpart of [`ICON_FONT`], and the only
/// reason a second icon face is bundled at all.
///
/// Source (downloaded verbatim, 449 252 bytes, SHA-256
/// `a53f5d2630cab5e3b7536ecb9d69d71519a2190298c22b1f8d770dd37bc2940a`):
/// <https://raw.githubusercontent.com/phosphor-icons/web/master/src/fill/Phosphor-Fill.ttf>
///
/// Licensed **MIT** (© 2020–2021 Phosphor Icons), the same licence as the Regular face —
/// it ships next to the file in `assets/LICENSE-Phosphor-Fill.txt`.
///
/// UI-SPEC v1.3 §Favorites needs exactly one glyph out of it: a *hearted* heart is
/// [`GLYPH_HEART`] drawn solid, and Phosphor Regular has no fill anywhere (the same fact
/// that sized [`ICON_MARK`] down). Rather than hand-fill an outline — a heart is a curve,
/// not a box — the upstream fill weight is bundled whole, on the same
/// "re-downloadable and checksummable" terms as [`ICON_FONT`].
const ICON_FILL_FONT: &[u8] = include_bytes!("../assets/Phosphor-Fill.ttf");

/// The key [`ICON_FILL_FONT`] is registered under.
const ICON_FILL_FONT_NAME: &str = "Phosphor-Fill";

/// The egui font family [`ICON_FILL_FONT`] is alone in — see [`font_icon_fill`].
const ICON_FILL_FAMILY: &str = "phosphor-fill";

/// Give both font families the icon face and a Japanese **fallback** (UI-SPEC v1.2 §CJK).
///
/// Both are appended with [`FontPriority::Lowest`], in this order, so every family reads
/// `[Hack, Phosphor, NotoSansJP]`. Three consequences, all of them wanted:
///
/// * Latin still comes from the bundled mono face — the terminal look is untouched, and
///   advances stay monospaced, because neither fallback is ever consulted for ASCII.
/// * Row height follows the *first* font of a family, so neither an icon nor a Japanese
///   title changes the height of the line it sits in.
/// * The icons are reachable from the **same family** as the text, which is what lets one
///   [`crate::widgets::icon_text`] galley hold a 14 px icon and an 11 px label in one run
///   (a separate [`FontFamily::Name`] would force two galleys and two hand-aligned paints
///   for every labelled button in the app). It is safe only because Phosphor is confined
///   to the PUA: a face carrying Latin could not be inserted here without changing it.
///
/// The filled icon face gets a family of its own, `phosphor-fill`, and is its only member.
/// It cannot be a third fallback: the two Phosphor weights carry the **same** codepoints
/// (`heart` is U+E2A8 in `src/regular/style.css` and in `src/fill/style.css` alike, and so
/// is every other icon — they are one icon list drawn twice). Appending it would put it
/// behind a face that already answers for all of them, so it would never be reached;
/// inserting it in front would turn every icon in the app solid. A separate family is the
/// only arrangement in which *both* weights are reachable, and it costs what a separate
/// family always costs — [`crate::widgets::icon_text`] cannot mix a filled icon into a text
/// run, so the one glyph that needs it is painted on its own (`song_row::heart`).
///
/// Called from [`install_style`], i.e. on every theme change: `add_font` compares font *names*
/// and returns without touching the atlas once the face is in, so that costs a lookup.
fn install_fonts(ctx: &Context) {
    let families = [FontFamily::Monospace, FontFamily::Proportional].map(|family| {
        egui::epaint::text::InsertFontFamily {
            family,
            priority: egui::epaint::text::FontPriority::Lowest,
        }
    });
    for (name, data) in [(ICON_FONT_NAME, ICON_FONT), (CJK_FONT_NAME, CJK_FONT)] {
        ctx.add_font(egui::epaint::text::FontInsert::new(
            name,
            egui::FontData::from_static(data),
            families.to_vec(),
        ));
    }
    ctx.add_font(egui::epaint::text::FontInsert::new(
        ICON_FILL_FONT_NAME,
        egui::FontData::from_static(ICON_FILL_FONT),
        vec![egui::epaint::text::InsertFontFamily {
            family: icon_fill_family(),
            // The family does not exist until this call creates it (egui's `add_font`
            // does `families.entry(..).or_default()`), so the face is both the highest
            // and the lowest priority in it. `Lowest` is what the other two say.
            priority: egui::epaint::text::FontPriority::Lowest,
        }],
    ));
}

// ---------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------

/// Default window size.
pub const WINDOW_DEFAULT: [f32; 2] = [1280.0, 820.0];
/// Minimum window size.
pub const WINDOW_MIN: [f32; 2] = [980.0, 640.0];

/// Sidebar width: `.default` for a fresh install, `.min`/`.max` for how far the divider
/// between it and the content may be dragged (UI-SPEC v1.4 §Panel widths).
///
/// The three draggable widths are the one place where a layout number is not a plain `f32`
/// this module owns outright. They are persisted in `state.json`, so
/// [`phoebus_core::PanelWidth`] holds the numbers — core is what has to clamp a hand-edited
/// file — and these are the app's names for them. The rule the module header states still
/// holds: no other file in the crate spells the numbers out.
pub const SIDEBAR_W: phoebus_core::PanelWidth = phoebus_core::SIDEBAR_WIDTH;
/// Player bar height.
pub const PLAYER_BAR_H: f32 = 64.0;
/// Up Next drawer width (C2), and the range its divider may be dragged over.
pub const QUEUE_W: phoebus_core::PanelWidth = phoebus_core::QUEUE_WIDTH;

/// Padding around every view's content.
pub const VIEW_PAD: f32 = 24.0;
/// Horizontal padding inside the sidebar and the player bar.
pub const PANEL_PAD: f32 = 14.0;
/// Indent of a sidebar item under its section label (UI-SPEC v1.2 §Sidebar sections).
pub const SECTION_INDENT: f32 = 12.0;

/// Extra space above the sidebar's wordmark, under which the macOS traffic lights float.
///
/// The window has no titlebar of its own (UI-SPEC v1.2 §Window chrome): the content view is
/// full-size and the buttons are drawn by the OS *over* the sidebar's top-left corner. This
/// is the room they need. Linux keeps its decorations, so nothing overlaps and it is zero —
/// a `cfg` rather than a runtime check because the window style is fixed at build time.
#[cfg(target_os = "macos")]
pub const TITLEBAR_PAD: f32 = 28.0;
/// Nothing floats over a normally-decorated window.
#[cfg(not(target_os = "macos"))]
pub const TITLEBAR_PAD: f32 = 0.0;

/// MINIMUM album card width. Cards stretch beyond this to fill the row's leftover width,
/// and snap back the moment the leftovers fit another whole column at this size.
pub const CARD_W: f32 = 180.0;
/// Gap between album cards.
pub const GRID_GUTTER: f32 = 16.0;
/// Gap between an album cover and its title.
pub const CARD_TEXT_GAP: f32 = 8.0;
/// The `▶` badge on a hovered album card.
pub const PLAY_BADGE: f32 = 28.0;

/// Cover size in the album-detail header.
pub const DETAIL_COVER: f32 = 232.0;
/// Album-detail tracklist row height.
pub const ROW_TRACK: f32 = 36.0;
/// Songs-view row height.
pub const ROW_SONG: f32 = 34.0;
/// Songs-view artwork square.
pub const SONG_ART: f32 = 24.0;
/// Initial width of the Songs table's `ARTIST` and `ALBUM` columns.
pub const SONG_COL_W: f32 = 190.0;
/// Sidebar nav / playlist row height (also the minimum hit target).
pub const ROW_NAV: f32 = 26.0;
/// Width of the track-number column in a tracklist.
pub const TRACK_NO_W: f32 = 24.0;

/// Width of the Artists view's left-hand artist list, and the range its divider may be
/// dragged over. The view narrows `.max` further at draw time so the album side always
/// keeps one [`CARD_W`] card.
pub const ARTIST_LIST_W: phoebus_core::PanelWidth = phoebus_core::ARTIST_LIST_WIDTH;
/// Artists-view list row height (name over album count).
pub const ROW_ARTIST: f32 = 40.0;
/// Playlist / search song row height (artwork, title over artist, album, time).
pub const ROW_PLAYLIST: f32 = 48.0;
/// Artwork square in a playlist / search song row.
pub const PLAYLIST_ART: f32 = 36.0;
/// Vertical gap between the sections of the Search view.
pub const SECTION_GAP: f32 = 20.0;

/// Maximum width of the player bar's centre "LCD".
pub const LCD_MAX_W: f32 = 560.0;
/// Height of the LCD box inside the 64 px player bar.
pub const LCD_H: f32 = 52.0;
/// Artwork square inside the LCD.
pub const LCD_ART: f32 = 48.0;
/// Up Next row artwork.
pub const QUEUE_ART: f32 = 28.0;
/// Up Next row height.
pub const ROW_QUEUE: f32 = 40.0;
/// How many upcoming tracks the drawer lists.
pub const QUEUE_MAX: usize = 50;

/// The add-songs picker's width, as a share of the window (UI-SPEC v1.4 §Add songs).
///
/// A share rather than a fixed width so the one modal in the app is always visibly *inside*
/// the window, with the dimmed view showing on either side of it — that gap is the whole
/// signal that the surface behind is still there and merely blocked.
pub const MODAL_W_FRAC: f32 = 0.7;
/// Ceiling on [`MODAL_W_FRAC`]. Past this a row's title column grows faster than anything
/// in it, and a two-column list of songs starts reading as a table with a hole in the
/// middle. Sized just above the player bar's [`LCD_MAX_W`], the app's other centred box.
pub const MODAL_MAX_W: f32 = 640.0;
/// The add-songs picker's height, as a share of the window. Uncapped: the taller the
/// window, the more songs a scroll shows, which is the only thing height buys here.
pub const MODAL_H_FRAC: f32 = 0.7;

/// Seek / volume track thickness.
pub const BAR_TRACK_H: f32 = 3.0;
/// Side of the square knob shown while hovering a bar.
pub const BAR_KNOB: f32 = 6.0;
/// Clickable height of a flat bar. The visible track stays [`BAR_TRACK_H`]; this is the
/// grab area, and UI-SPEC §Feel puts a 24 px floor under every hit target.
pub const BAR_HIT_H: f32 = HIT_MIN;
/// Width of the volume bar itself (the speaker icon sits to its left).
pub const VOLUME_W: f32 = 110.0;
/// Gap between the speaker icon and the volume bar.
pub const VOLUME_LABEL_GAP: f32 = 8.0;

/// Transport glyph size — the same for prev, play/pause and next (UI-SPEC §Player bar).
///
/// There is no optical nudge on the play button any more, and there is no longer anything
/// for one to correct. The four transport glyphs are one face now, and Phosphor draws them
/// on a shared centre line: measured off the outlines at a 200 px em, the ink boxes of
/// `SKIP_BACK` / `PLAY` / `PAUSE` / `SKIP_FORWARD` centre at 100.5 / 100.0 / 100.0 / 100.5
/// — half a unit of 200, i.e. 0.05 px at this size. The old `PLAY_NUDGE` existed only
/// because `▶` (U+25B6) and `⏸` (U+23F8) came from two different system fallback faces.
pub const ICON_TRANSPORT: f32 = 18.0;
/// Side of each transport button's square hit rect. All three are identical.
pub const TRANSPORT_HIT: f32 = 32.0;
/// Gap between the three transport buttons.
pub const TRANSPORT_GAP: f32 = 8.0;
/// Padding inside the LCD box.
pub const LCD_PAD: f32 = 8.0;
/// Width reserved for an `M:SS` timestamp in a track-list row.
pub const TIME_W: f32 = 40.0;
/// Gap between the LCD's timestamps and the seek track.
///
/// The seek row measures its two readouts instead of reserving a fixed column, because
/// `-1:58:55` is half again as wide as `-3:09` and must not run under the track (UI-SPEC
/// v1.2 §Durations). This is the clearance left on either side of it.
pub const LCD_TIME_GAP: f32 = 12.0;
/// Secondary icon glyph size: shuffle, repeat, the queue toggle, the album-card play badge,
/// and the icon in front of a [`SIZE_BUTTON`] label (`PLAY`, `SHUFFLE`).
pub const ICON_SMALL: f32 = 15.0;
/// An icon set beside a [`SIZE_SMALL`] micro-label: the back row, the volume speaker, the
/// playlist rename pencil.
///
/// Sized off the label rather than off the icon: Phosphor's ink fills ~0.63 em vertically
/// (`ARROW_LEFT` measures 126/200 of the em), and Hack's cap height is ~0.72 em, so 14 px
/// of icon puts ~8.8 px of ink beside ~7.9 px of capital — a hair taller than the letters,
/// which is what an icon has to be to read as their equal. The old `←` at the label's own
/// 11 px was ~6 px of ink against those 7.9 px, and looked like a typo.
pub const ICON_INLINE: f32 = 14.0;
/// The leading state column's play / pause affordance (`song_row::lead`).
///
/// One step above [`ICON_SMALL`] so its ink (`PLAY` is 0.815 em ⇒ ~13 px) matches the
/// [`crate::widgets::equalizer::HEIGHT`] bars it swaps places with.
pub const ICON_LEAD: f32 = 16.0;
/// The sort caret on an active column header.
///
/// Above the [`SIZE_SMALL`] label it annotates, because a Phosphor caret is a *chevron* and
/// carries only 0.38 em of ink vertically: at the label's own 11 px it measures 4.2 px
/// against 7.9 px capitals and disappears. At 12 px it is 4.5 px of stroke as wide as a
/// letter, which is the weight the solid `▲` used to have.
pub const ICON_SORT: f32 = 12.0;
/// The manual-queue diamond in the Up Next drawer.
///
/// The one icon deliberately sized *below* the text it marks. Phosphor Regular has no fill,
/// so `DIAMOND` is an outline, and an outline reads as much heavier than the solid `◆` it
/// replaces at the same size — at 12 px it measured taller than the row's title capitals
/// and stopped being a marker. 10 px puts 8.75 px of outline beside 9.7 px of capital.
pub const ICON_MARK: f32 = 10.0;
/// The favourite heart, in a track row and in an album card's corner (UI-SPEC v1.3
/// §Favorites).
///
/// One size for both, because it is one control: the 24 px column a row gives it and the
/// 24 px corner box a cover gives it are the same square, and a heart that changed weight
/// between the two places you meet it would read as two different marks. 14 px is
/// [`ICON_INLINE`]'s size for the same reason the back arrow uses it — the outline state
/// stands beside `SIZE_SMALL` durations — and Phosphor's `heart` carries 0.66 em of ink
/// horizontally, so the solid state fills ~9.2 px of the 24 px column without crowding the
/// duration next to it.
pub const ICON_HEART: f32 = 14.0;
/// Padding around a glyph in an auto-sized icon button.
pub const ICON_PAD: f32 = 10.0;
/// Gap between an icon and the label it introduces, in a [`crate::widgets::icon_text`] run.
pub const ICON_TEXT_GAP: f32 = 8.0;
/// Minimum hit target.
pub const HIT_MIN: f32 = 24.0;
/// Offset of the second pass in a fake-bold label. The bundled monospace face has no bold
/// weight, so the only way to render "heavier" is to paint the galley twice.
pub const FAKE_BOLD: f32 = 0.5;
/// Width of the ACCENT bar marking an active nav row.
pub const ACTIVE_BAR_W: f32 = 2.0;

/// Corner radius of cards and buttons (panels stay square).
pub const CORNER: u8 = 2;
/// Hairline thickness.
pub const HAIRLINE_W: f32 = 1.0;

/// Inner margin of a context-menu / submenu popup (UI-SPEC v1.2 §Menus).
///
/// Set once on the [`egui::Style`] in [`install_style`], because the popup frame is built
/// from the style *before* the menu body runs — by the time
/// [`crate::widgets::menus::styled`] gets its `Ui`, the margin is already painted. egui
/// draws tooltips from the same field, so they gain the same air; that is the only thing
/// this reaches beyond the menus, and it suits a design that is already spaced out.
pub const MENU_MARGIN: i8 = 10;
/// Padding around one context-menu item (UI-SPEC v1.2 §Menus).
pub const MENU_ITEM_PAD: Vec2 = Vec2::new(8.0, 5.0);

/// Restart-instead-of-previous threshold.
pub const PREV_RESTART_SECS: f32 = 3.0;
/// Repaint interval while playing.
pub const REPAINT_MS: u64 = 250;
/// Debounce before `state.json` is rewritten.
pub const SAVE_DEBOUNCE_MS: u64 = 1000;
/// Minimum gap between live seeks while dragging the seek bar (≤ 2/s).
pub const LIVE_SEEK_MS: u64 = 500;
/// Volume step for `⌘↑` / `⌘↓`.
pub const VOLUME_STEP: f32 = 0.05;

// ---------------------------------------------------------------------------------------
// Glyphs
// ---------------------------------------------------------------------------------------
//
// Every icon in Phoebus is one codepoint of Phosphor Regular ([`ICON_FONT`]), named here
// after its upstream icon and nowhere else. They replaced a bag of Unicode symbols
// (`⏮ ▶ ⏸ ⏭ 🔀 ⟲ ≡ ♪ 🔍 ← ◆ ▲ ▼`) that the bundled faces answered for one at a time, out
// of four different fallbacks, at four different optical weights and two different
// baselines — which is precisely what made the `←` next to `ALBUMS` look broken.
//
// The codepoints are the ones in `src/regular/style.css` of the upstream repo; each is
// spelled out below so a reader can check one against that file without a font editor.
// `theme::tests::every_icon_has_a_glyph` fails the build if any of them stops resolving.

/// Previous track — `skip-back`, U+E5A4.
pub const GLYPH_PREV: &str = "\u{e5a4}";
/// Next track — `skip-forward`, U+E5A6.
pub const GLYPH_NEXT: &str = "\u{e5a6}";
/// Play — `play`, U+E3D0.
pub const GLYPH_PLAY: &str = "\u{e3d0}";
/// Pause — `pause`, U+E39E.
pub const GLYPH_PAUSE: &str = "\u{e39e}";
/// Shuffle — `shuffle`, U+E422. The crossed arrows UI-SPEC wanted from `⤨` (U+2928), which
/// no bundled face carried.
pub const GLYPH_SHUFFLE: &str = "\u{e422}";
/// Repeat all — `repeat`, U+E3F6.
pub const GLYPH_REPEAT: &str = "\u{e3f6}";
/// Repeat one — `repeat-once`, U+E3F8. A glyph of its own, with the `1` drawn *inside* the
/// loop: it replaces the old `⟲` + superscript `¹` pair, which was two faces stuck together
/// and grew the button when it appeared.
pub const GLYPH_REPEAT_ONE: &str = "\u{e3f8}";
/// Queue drawer toggle — `list`, U+E2F0. UI-SPEC draws this control as `≡`, and of
/// Phosphor's several list icons this is the one that *is* `≡`.
pub const GLYPH_QUEUE: &str = "\u{e2f0}";
/// Artwork placeholder — `music-note`, U+E33C. The single note UI-SPEC spells `♪`; the
/// beamed `music-notes` is denser than the 11 px placeholders can carry.
pub const GLYPH_NOTE: &str = "\u{e33c}";
/// Search field prefix — `magnifying-glass`, U+E30C. UI-SPEC asks for `⌕` (U+2315), absent
/// from the bundled fonts.
pub const GLYPH_SEARCH: &str = "\u{e30c}";
/// Back arrow — `arrow-left`, U+E058.
pub const GLYPH_BACK: &str = "\u{e058}";
/// Manual-queue marker in the Up Next drawer — `diamond`, U+E1EC.
pub const GLYPH_MANUAL: &str = "\u{e1ec}";
/// Ascending sort indicator on an active column header — `caret-up`, U+E13C.
pub const GLYPH_SORT_ASC: &str = "\u{e13c}";
/// Descending sort indicator on an active column header — `caret-down`, U+E136.
pub const GLYPH_SORT_DESC: &str = "\u{e136}";
/// Volume — `speaker-simple-high`, U+E450. Replaces the `VOL` micro-label UI-SPEC allowed
/// "or a verified-non-tofu speaker glyph" for; the straight-bar *simple* variant holds
/// together at [`ICON_INLINE`] where the curved waves of `speaker-high` start to merge.
pub const GLYPH_VOLUME: &str = "\u{e450}";
/// Rename a playlist — `pencil-simple`, U+E3B4. The `✎` UI-SPEC asked for, which used to be
/// spelled out as the word `RENAME` because that glyph was tofu.
pub const GLYPH_RENAME: &str = "\u{e3b4}";
/// Submenu marker on `Add to Playlist` — `caret-right`, U+E13A.
pub const GLYPH_SUBMENU: &str = "\u{e13a}";
/// Add a song to a playlist — `plus`, U+E3D4. The `ADD SONGS` button and, in the picker
/// (UI-SPEC v1.4 §Add songs), the leading button of every row that is not on the list yet.
pub const GLYPH_PLUS: &str = "\u{e3d4}";
/// A song the playlist already has — `check`, U+E182. [`GLYPH_PLUS`]'s other state, and a
/// readout rather than a button: the picker only ever adds.
pub const GLYPH_CHECK: &str = "\u{e182}";
/// Dismiss the add-songs picker — `x`, U+E4F6.
pub const GLYPH_CLOSE: &str = "\u{e4f6}";
/// Favourite — `heart`, U+E2A8. The one glyph the app draws in **two** weights: outline
/// from [`ICON_FONT`] via [`font_icon`] for "not hearted", solid from [`ICON_FILL_FONT`]
/// via [`font_icon_fill`] for "hearted". One constant serves both, because the codepoint is
/// the same in either face — which is exactly why they cannot share a family
/// (see [`install_fonts`]).
pub const GLYPH_HEART: &str = "\u{e2a8}";
/// Separator between metadata fragments.
pub const SEP: &str = " · ";

/// Every icon the app paints, for the test that proves they all resolve to real outlines.
#[cfg(test)]
const ALL_GLYPHS: [(&str, &str); 21] = [
    ("PREV", GLYPH_PREV),
    ("NEXT", GLYPH_NEXT),
    ("PLAY", GLYPH_PLAY),
    ("PAUSE", GLYPH_PAUSE),
    ("SHUFFLE", GLYPH_SHUFFLE),
    ("REPEAT", GLYPH_REPEAT),
    ("REPEAT_ONE", GLYPH_REPEAT_ONE),
    ("QUEUE", GLYPH_QUEUE),
    ("NOTE", GLYPH_NOTE),
    ("SEARCH", GLYPH_SEARCH),
    ("BACK", GLYPH_BACK),
    ("MANUAL", GLYPH_MANUAL),
    ("SORT_ASC", GLYPH_SORT_ASC),
    ("SORT_DESC", GLYPH_SORT_DESC),
    ("VOLUME", GLYPH_VOLUME),
    ("RENAME", GLYPH_RENAME),
    ("SUBMENU", GLYPH_SUBMENU),
    ("PLUS", GLYPH_PLUS),
    ("CHECK", GLYPH_CHECK),
    ("CLOSE", GLYPH_CLOSE),
    ("HEART", GLYPH_HEART),
];

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/// The 1 px `border` hairline used between panels and around cards.
pub fn hairline() -> Stroke {
    Stroke::new(HAIRLINE_W, p().border)
}

/// A 1 px accent outline (hovered cover). A fill-side usage: the raw accent.
pub fn accent_line() -> Stroke {
    Stroke::new(HAIRLINE_W, p().accent)
}

/// Card / button corner radius.
pub fn corner() -> CornerRadius {
    CornerRadius::same(CORNER)
}

/// Pick a text colour from an interaction state: idle → `idle`, hovered → `hover`.
pub fn hover_color(hovered: bool, idle: Color32, hover: Color32) -> Color32 {
    if hovered { hover } else { idle }
}

// ---------------------------------------------------------------------------------------
// Style installation
// ---------------------------------------------------------------------------------------

/// Switch the whole app to `mode` + `accent`: publish the palette and rebuild egui's style
/// so the built-in widgets follow the hand-painted ones.
///
/// Cheap enough to call on every theme change — after the first call it re-lays no text and
/// allocates no textures, it only rewrites two `Style` structs. (The first call also registers
/// the CJK fallback face; see [`install_fonts`].)
pub fn apply(ctx: &Context, mode: ThemeMode, accent: Color32) {
    let next = set(mode, accent);
    install_style(ctx, &next);
}

/// Paint the whole egui [`Context`] in the *current* palette and in monospace type.
///
/// Test-only: the app always knows which palette it wants and goes through [`apply`]. Every
/// headless render test in the crate needs the styling, and none of them wants to touch the
/// process-wide palette, so this is the shape they share.
#[cfg(test)]
pub fn install(ctx: &Context) {
    install_style(ctx, &p());
}

/// Paint the whole egui [`Context`] in `p`'s colours and monospace type, and make sure the
/// Japanese fallback face is loaded.
///
/// Everything egui draws by itself (menus, tooltips, scrollbars, text edits, striped rows)
/// picks its colours up from here, so the manual painting elsewhere in the crate and the
/// built-in widgets always agree — including after a live theme switch.
fn install_style(ctx: &Context, p: &Palette) {
    install_fonts(ctx);
    ctx.set_theme(if p.mode.is_dark() {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    // Both stored styles are written, so the one `set_theme` selects is ours either way.
    ctx.all_styles_mut(|style| {
        style.text_styles = std::collections::BTreeMap::from([
            (TextStyle::Small, font_small()),
            (TextStyle::Body, font_body()),
            (TextStyle::Monospace, font_body()),
            (
                TextStyle::Button,
                FontId::new(SIZE_BUTTON, FontFamily::Monospace),
            ),
            (TextStyle::Heading, font_heading()),
            (TextStyle::Name(SUB_STYLE.into()), font_sub()),
        ]);

        let hairline = Stroke::new(HAIRLINE_W, p.border);
        let v = &mut style.visuals;
        // Built-in widgets branch on this (and eframe reads it back for the window chrome),
        // so it has to follow the palette rather than stay pinned to `true`.
        v.dark_mode = p.mode.is_dark();
        v.panel_fill = p.bg0;
        v.window_fill = p.bg1;
        v.extreme_bg_color = p.bg2;
        v.faint_bg_color = p.stripe;
        v.code_bg_color = p.bg2;
        v.hyperlink_color = p.accent_text;
        v.text_edit_bg_color = Some(p.bg2);
        v.weak_text_color = Some(p.text_low);
        v.warn_fg_color = p.accent_text;
        v.error_fg_color = p.accent_text;
        v.selection.bg_fill = p.selection_bg;
        v.selection.stroke = Stroke::new(HAIRLINE_W, p.accent_text);
        v.window_stroke = hairline;
        v.window_corner_radius = corner();
        v.menu_corner_radius = corner();
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::NONE;
        v.striped = false;
        v.button_frame = true;
        v.slider_trailing_fill = true;
        v.resize_corner_size = 0.0;

        // Non-interactive: plain labels, panel separators, frame outlines.
        let w = &mut v.widgets.noninteractive;
        w.bg_fill = p.bg1;
        w.weak_bg_fill = p.bg1;
        w.bg_stroke = hairline;
        w.fg_stroke = Stroke::new(1.0, p.text_hi);
        w.corner_radius = corner();
        w.expansion = 0.0;

        // Idle interactive: flat, no fill — the surface shows through.
        let w = &mut v.widgets.inactive;
        w.bg_fill = p.bg2;
        w.weak_bg_fill = Color32::TRANSPARENT;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke = Stroke::new(1.0, p.text_mid);
        w.corner_radius = corner();
        w.expansion = 0.0;

        // Hover on a neutral surface is `bg2` — never the accent.
        for w in [
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.bg_fill = p.bg2;
            w.weak_bg_fill = p.bg2;
            w.bg_stroke = hairline;
            w.fg_stroke = Stroke::new(1.0, p.text_hi);
            w.corner_radius = corner();
            w.expansion = 0.0;
        }

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size = egui::vec2(HIT_MIN, HIT_MIN);
        style.spacing.menu_margin = Margin::same(MENU_MARGIN);
        style.spacing.window_margin = Margin::same(6);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = false;
        style.spacing.scroll.bar_inner_margin = 2.0;
        style.spacing.scroll.bar_outer_margin = 0.0;
        style.interaction.selectable_labels = false;
        style.animation_time = 0.0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every dark-mode colour, spelled out — UI-SPEC v1.2 §Colors read back off the
    /// implementation. Every dark-mode pixel in the app comes from one of these values.
    const DARK: [(&str, Color32); 10] = [
        ("bg0", Color32::from_rgb(0x0E, 0x13, 0x1C)),
        ("bg1", Color32::from_rgb(0x0A, 0x0E, 0x15)),
        ("bg2", Color32::from_rgb(0x16, 0x1D, 0x2A)),
        ("border", Color32::from_rgb(0x20, 0x28, 0x39)),
        ("text_hi", Color32::from_rgb(0xF2, 0xF2, 0xF2)),
        ("text_mid", Color32::from_rgb(0x9A, 0x9A, 0x9A)),
        ("text_low", Color32::from_rgb(0x57, 0x57, 0x57)),
        ("accent", Color32::from_rgb(0xFF, 0xFB, 0x00)),
        ("accent_text", Color32::from_rgb(0xFF, 0xFB, 0x00)),
        ("on_accent", Color32::from_rgb(0x0A, 0x0A, 0x0A)),
    ];

    /// The six presets UI-SPEC's Settings view offers, plus the two extremes and a colour
    /// dark enough to break the "just darken it" rule.
    const ACCENTS: [Color32; 9] = [
        Color32::from_rgb(0xFF, 0xFB, 0x00), // yellow (default)
        Color32::from_rgb(0x19, 0xB0, 0x92), // teal
        Color32::from_rgb(0xF0, 0x94, 0x1C), // orange
        Color32::from_rgb(0x8B, 0x54, 0xCF), // purple
        Color32::from_rgb(0xC6, 0xC2, 0xBB), // warm gray
        Color32::from_rgb(0xE5, 0xF1, 0xFF), // ice blue
        Color32::from_rgb(0xFF, 0xFF, 0xFF),
        Color32::from_rgb(0x00, 0x00, 0x00),
        Color32::from_rgb(0x10, 0x10, 0x60), // near-black navy: darkening cannot save it
    ];

    fn near(a: Color32, b: Color32, tolerance: i32) -> bool {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).abs();
        d(a.r(), b.r()) <= tolerance && d(a.g(), b.g()) <= tolerance && d(a.b(), b.b()) <= tolerance
    }

    /// UI-SPEC v1.2 §Colors, token by token, plus §Settings' *in dark mode the yellow
    /// already passes and stays unchanged*.
    #[test]
    fn the_dark_palette_is_the_v12_one() {
        let p = palette(ThemeMode::Dark, DEFAULT_ACCENT);
        let got: [(&str, Color32); 10] = [
            ("bg0", p.bg0),
            ("bg1", p.bg1),
            ("bg2", p.bg2),
            ("border", p.border),
            ("text_hi", p.text_hi),
            ("text_mid", p.text_mid),
            ("text_low", p.text_low),
            ("accent", p.accent),
            ("accent_text", p.accent_text),
            ("on_accent", p.on_accent),
        ];
        for ((name, want), (_, have)) in DARK.iter().zip(got) {
            assert_eq!(have, *want, "{name} moved");
        }
        assert_eq!(
            p.selection_bg,
            Color32::from_rgba_unmultiplied(0xFF, 0xFB, 0x00, SELECTION_ALPHA),
            "the ~12 % accent wash"
        );
        assert_eq!(
            p.stripe,
            Color32::from_rgba_unmultiplied(255, 255, 255, STRIPE_ALPHA_DARK)
        );
        // The hover form of an accent fill: the accent, 40 % of the way to `bg0`.
        assert!(
            near(p.accent_dim, Color32::from_rgb(0x9F, 0x9E, 0x0B), 2),
            "accent_dim drifted: {:?}",
            p.accent_dim
        );
        assert_eq!(p.accent_text_dim, p.accent_dim, "identical in dark mode");
        assert!(p.mode.is_dark());
        // Blue-slate, not grey: the surfaces have to keep a blue bias, and the chrome has
        // to stay *darker* than the content views it frames.
        assert!(p.bg0.b() > p.bg0.r() && p.bg2.b() > p.bg2.r());
        assert!(luminance(p.bg1) < luminance(p.bg0));
    }

    #[test]
    fn the_light_palette_is_the_v12_one() {
        let p = palette(ThemeMode::Light, DEFAULT_ACCENT);
        assert_eq!(p.bg0, Color32::from_rgb(0xD4, 0xD4, 0xD4));
        assert_eq!(p.bg1, Color32::from_rgb(0xC8, 0xC8, 0xC8));
        assert_eq!(p.bg2, Color32::from_rgb(0xDE, 0xDE, 0xDE));
        assert_eq!(p.border, Color32::from_rgb(0xB6, 0xB6, 0xB6));
        assert_eq!(p.text_hi, Color32::from_rgb(0x14, 0x14, 0x14));
        assert_eq!(p.text_mid, Color32::from_rgb(0x5A, 0x5A, 0x5A));
        assert_eq!(p.text_low, Color32::from_rgb(0x9A, 0x9A, 0x98));
        assert_eq!(p.accent, DEFAULT_ACCENT, "the accent itself never moves");
        assert_ne!(
            p.accent_text, p.accent,
            "the yellow is illegible on paper and must have been darkened"
        );
        // The stripe changes sign with the mode, or it is invisible.
        assert!(p.stripe.r() == 0 && p.stripe.a() > 0);
        // …and so does the artwork scrim, for the same reason: a near-black heart on paper
        // needs the cover *lightened* under it, not darkened.
        // `Color32` is premultiplied, so the sign has to be read off the straight form.
        let dark = palette(ThemeMode::Dark, DEFAULT_ACCENT)
            .scrim
            .to_srgba_unmultiplied();
        let light = p.scrim.to_srgba_unmultiplied();
        assert_eq!(dark[..3], [0, 0, 0], "dark mode darkens the cover");
        assert_eq!(light[..3], [255, 255, 255], "light mode lightens it");
        assert_eq!(dark[3], light[3], "…at the same weight");
        assert!(light[3] > 0 && light[3] < 255, "it is a scrim, not paint");
    }

    /// UI-SPEC v1.2 §Colors: *a persisted accent equal to a FORMER default loads as the
    /// current default*. Anything the user actually picked survives untouched.
    #[test]
    fn a_saved_accent_from_the_old_default_becomes_the_new_one() {
        let old = Color32::from_rgb(0xE8, 0xFF, 0x2E);
        assert!(LEGACY_ACCENTS.contains(&old));
        assert_eq!(migrate(old), DEFAULT_ACCENT);
        assert_eq!(migrate(DEFAULT_ACCENT), DEFAULT_ACCENT);
        for accent in ACCENTS.iter().skip(1) {
            assert_eq!(
                migrate(*accent),
                *accent,
                "{accent:?} was not chosen for us"
            );
        }

        // …and the migration is on the path `state.json` actually takes into the palette.
        let stale = AppState {
            accent: "#E8FF2E".to_string(),
            ..AppState::default()
        };
        assert_eq!(resolve(&stale).1, DEFAULT_ACCENT);
        // …and a colour that merely *dropped out* of the preset row in v1.4 is still a
        // deliberate choice: it loads exactly as it was saved, preset or not.
        let chosen = AppState {
            accent: "#2EF0FF".to_string(),
            ..AppState::default()
        };
        assert_eq!(resolve(&chosen).1, Color32::from_rgb(0x2E, 0xF0, 0xFF));
    }

    /// The contrast rule, which is the only thing that makes a custom accent safe.
    #[test]
    fn accent_text_always_clears_wcag_aa_against_bg0() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            for accent in ACCENTS {
                let p = palette(mode, accent);
                let ratio = contrast(p.accent_text, p.bg0);
                assert!(
                    ratio >= MIN_CONTRAST,
                    "{mode:?} {accent:?} -> {:?} is only {ratio:.2}:1",
                    p.accent_text
                );
                // …and it only ever moves a colour that had to move: an accent that already
                // reads on `bg0` is handed back untouched, hue and all.
                assert_eq!(
                    p.accent_text == accent,
                    contrast(accent, p.bg0) >= MIN_CONTRAST,
                    "{mode:?} {accent:?} was adjusted without needing it (or vice versa)"
                );
            }
        }
    }

    /// A dark accent on a dark background cannot be fixed by darkening it further; UI-SPEC
    /// words the rule for the common case, and the implementation moves *away* from the
    /// background instead.
    #[test]
    fn a_dark_accent_in_dark_mode_is_lightened_not_darkened() {
        let navy = Color32::from_rgb(0x10, 0x10, 0x60);
        let p = palette(ThemeMode::Dark, navy);
        assert!(
            luminance(p.accent_text) > luminance(navy),
            "{:?}",
            p.accent_text
        );
        assert!(contrast(p.accent_text, p.bg0) >= MIN_CONTRAST);
        // …and the mirror image: the same colour on paper is already readable.
        let light = palette(ThemeMode::Light, navy);
        assert_eq!(light.accent_text, navy, "already 4.5:1 on #D4D4D4");
    }

    #[test]
    fn on_accent_follows_the_accent_luminance() {
        for accent in ACCENTS {
            let p = palette(ThemeMode::Dark, accent);
            assert_eq!(
                p.on_accent,
                if luminance(accent) > 0.18 {
                    NEAR_BLACK
                } else {
                    NEAR_WHITE
                },
                "{accent:?} got the wrong text colour on top of it"
            );
            assert!(
                contrast(p.on_accent, accent) >= 4.0,
                "{accent:?}: label on the fill is only {:.2}:1",
                contrast(p.on_accent, accent)
            );
        }
    }

    #[test]
    fn contrast_matches_the_wcag_reference_values() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.001);
        // Symmetric, and the mid-grey reference point (#767676 on white is the AA floor).
        let grey = Color32::from_rgb(0x76, 0x76, 0x76);
        assert!((contrast(grey, Color32::WHITE) - contrast(Color32::WHITE, grey)).abs() < 1e-6);
        assert!(contrast(grey, Color32::WHITE) >= MIN_CONTRAST);
    }

    #[test]
    fn mixing_hits_both_ends_and_the_middle() {
        let a = Color32::from_rgb(0, 100, 200);
        let b = Color32::from_rgb(200, 0, 100);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 50, 150));
        assert_eq!(mix(a, b, -1.0), a, "clamped");
        assert_eq!(mix(a, b, 9.0), b, "clamped");
    }

    #[test]
    fn phoebus_theme_parses_every_documented_form() {
        let cyan = Color32::from_rgb(0x2E, 0xF0, 0xFF);
        assert_eq!(
            parse_override("light"),
            Some(Override {
                mode: ThemeMode::Light,
                accent: None
            })
        );
        assert_eq!(
            parse_override(" DARK "),
            Some(Override {
                mode: ThemeMode::Dark,
                accent: None
            })
        );
        assert_eq!(
            parse_override("dark,#2EF0FF"),
            Some(Override {
                mode: ThemeMode::Dark,
                accent: Some(cyan)
            })
        );
        assert_eq!(
            parse_override("light, #2ef0ff "),
            Some(Override {
                mode: ThemeMode::Light,
                accent: Some(cyan)
            })
        );
        for bad in ["", "sepia", "#2EF0FF", "light,", "light,red", "light,#2EF"] {
            assert_eq!(parse_override(bad), None, "{bad:?} must not parse");
        }
    }

    /// `PHOEBUS_THEME` wins over `state.json`, and only for the parts it names.
    #[test]
    fn resolve_prefers_the_environment_but_keeps_the_saved_accent() {
        let state = AppState {
            theme_mode: ThemeMode::Dark,
            accent: "#FF2E9E".to_string(),
            ..AppState::default()
        };
        let magenta = Color32::from_rgb(0xFF, 0x2E, 0x9E);
        assert_eq!(resolve(&state), (ThemeMode::Dark, magenta), "no env set");

        let over = parse_override("light").expect("parses");
        assert_eq!(over.accent, None);
        assert_eq!(
            (over.mode, over.accent.unwrap_or(magenta)),
            (ThemeMode::Light, magenta),
            "a bare mode keeps the saved accent"
        );
    }

    /// Rebuilding the style is what makes a live theme switch reach egui's own widgets —
    /// menus, scrollbars, text edits, striped rows.
    #[test]
    fn installing_a_palette_repaints_egui_itself() {
        let ctx = egui::Context::default();
        for mode in [ThemeMode::Dark, ThemeMode::Light, ThemeMode::Dark] {
            let want = palette(mode, DEFAULT_ACCENT);
            install_style(&ctx, &want);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(200.0, 100.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let v = ui.visuals();
                assert_eq!(v.dark_mode, mode.is_dark(), "{mode:?}: dark_mode flag");
                assert_eq!(v.panel_fill, want.bg0, "{mode:?}: panels");
                assert_eq!(v.window_fill, want.bg1, "{mode:?}: menus and tooltips");
                assert_eq!(v.extreme_bg_color, want.bg2, "{mode:?}: text edits");
                assert_eq!(v.faint_bg_color, want.stripe, "{mode:?}: striped rows");
                assert_eq!(v.widgets.inactive.fg_stroke.color, want.text_mid);
                assert_eq!(v.widgets.hovered.bg_fill, want.bg2);
                assert_eq!(v.selection.bg_fill, want.selection_bg);
                assert_eq!(
                    ui.style()
                        .text_styles
                        .get(&egui::TextStyle::Body)
                        .map(|f| f.size),
                    Some(SIZE_BODY),
                    "the type scale must survive a theme switch"
                );
                // The popup frame is built from the style before a menu body ever runs, so
                // UI-SPEC v1.2's menu margin can only live here (see `widgets::menus`).
                assert_eq!(
                    ui.style().spacing.menu_margin,
                    Margin::same(MENU_MARGIN),
                    "{mode:?}: the menu margin"
                );
            });
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            out.textures_delta.clear();
        }
    }

    /// UI-SPEC v1.2 §CJK: Japanese has to render, and it may not cost the Latin mono face.
    ///
    /// Both halves matter. `has_glyphs` proves the fallback is reachable *and* that the
    /// rasteriser can actually outline this file (a font it cannot parse contributes no
    /// glyphs at all); the widths prove `FontPriority::Lowest` really is a fallback — Hack
    /// still answers for ASCII, so every Latin advance is unchanged and monospaced.
    #[test]
    fn japanese_renders_without_disturbing_the_mono_face() {
        // Fonts do not exist until a pass has run, and an added face only lands at the start
        // of the next one — so both contexts get pumped once.
        let pumped = |styled: bool| {
            let ctx = egui::Context::default();
            if styled {
                install_style(&ctx, &palette(ThemeMode::Dark, DEFAULT_ACCENT));
            }
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(200.0, 100.0),
                )),
                ..Default::default()
            };
            // API-FACTS §3.7: dropping unapplied texture deltas panics.
            ctx.run_ui(input, |_| {}).textures_delta.clear();
            ctx
        };
        let ctx = pumped(true);
        let bare = pumped(false);

        for family in [FontFamily::Monospace, FontFamily::Proportional] {
            let id = FontId::new(SIZE_BODY, family.clone());
            assert!(
                !bare.fonts_mut(|f| f.has_glyphs(&id, CJK_SAMPLE)),
                "{family:?}: egui's bundled fonts were supposed to lack {CJK_SAMPLE}"
            );
            assert!(
                ctx.fonts_mut(|f| f.has_glyphs(&id, CJK_SAMPLE)),
                "{family:?}: {CJK_SAMPLE} has no glyphs — the fallback never loaded"
            );
            assert!(
                ctx.fonts_mut(|f| f.glyph_width(&id, 'の')) > 0.0,
                "{family:?}: a kana with no advance is a blank, not a glyph"
            );
        }

        let mono = font_body();
        // The WHOLE printable ASCII range, not a sample. [`ICON_FONT`] is not purely a PUA
        // face: it also carries `a`–`z`, `-` and the space, because the upstream web font
        // spells its icon names as `liga` ligatures (`ph-play`) and needs the letters to
        // build them from. Those glyphs are blanks, and if the face ever answered for them
        // — a change of insert priority would be enough — every lowercase letter in the app
        // would vanish while the tests still passed on `MiW01`. So: every character, and
        // both fallbacks measured against a context that has neither.
        let latin: String = (0x20u8..0x7F).map(char::from).collect();
        for c in latin.chars() {
            assert_eq!(
                ctx.fonts_mut(|f| f.glyph_width(&mono, c)),
                bare.fonts_mut(|f| f.glyph_width(&mono, c)),
                "{c:?} changed width — a bundled fallback is jumping the queue"
            );
        }
        let widths: Vec<f32> = latin
            .chars()
            .map(|c| ctx.fonts_mut(|f| f.glyph_width(&mono, c)))
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "the mono face stopped being monospaced: {widths:?}"
        );
        assert_eq!(
            ctx.fonts_mut(|f| f.row_height(&mono)),
            bare.fonts_mut(|f| f.row_height(&mono)),
            "row height follows the primary font, not the fallback"
        );
        assert_eq!(
            ctx.fonts_mut(|f| f.definitions().families[&FontFamily::Monospace]
                .last()
                .cloned()),
            Some(CJK_FONT_NAME.to_owned()),
            "the CJK face must be the LAST fallback of the mono family"
        );
    }

    /// The whole icon set, proved to be *outlines* rather than tofu.
    ///
    /// This is the test that a mistyped codepoint fails. `has_glyphs` is not enough on its
    /// own — a missing PUA codepoint is a perfectly ordinary "no glyph" answer — so each
    /// icon also has to have an advance, which only a rasterised outline gets.
    #[test]
    fn every_icon_resolves_to_a_phosphor_outline() {
        let ctx = egui::Context::default();
        install_style(&ctx, &palette(ThemeMode::Dark, DEFAULT_ACCENT));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(200.0, 100.0),
            )),
            ..Default::default()
        };
        // API-FACTS §3.7: dropping unapplied texture deltas panics.
        ctx.run_ui(input, |_| {}).textures_delta.clear();

        for family in [FontFamily::Monospace, FontFamily::Proportional] {
            let id = FontId::new(ICON_TRANSPORT, family.clone());
            for (name, glyph) in ALL_GLYPHS {
                assert!(
                    ctx.fonts_mut(|f| f.has_glyphs(&id, glyph)),
                    "{family:?}: GLYPH_{name} ({glyph:?}) has no glyph"
                );
                let c = glyph.chars().next().expect("one codepoint per icon");
                assert!(
                    (0xE000..=0xF8FF).contains(&(c as u32)),
                    "GLYPH_{name} is not in the Private Use Area, so it can collide with text"
                );
                // …and that it is *Phosphor's* outline, not one some other fallback
                // happens to claim. egui's default families already end in two emoji
                // faces, both of which sit ahead of anything added at `Lowest`, so "the
                // codepoint resolves" is not on its own proof that the right face won.
                // Phosphor is drawn on a square em and every one of its glyphs advances
                // exactly 1 em, which no proportional face does for a whole set at once.
                assert_eq!(
                    ctx.fonts_mut(|f| f.glyph_width(&id, c)),
                    ICON_TRANSPORT,
                    "{family:?}: GLYPH_{name} does not advance one em — another fallback \
                     answered for this codepoint"
                );
            }
        }

        // ---- and the FILL face, which is a family of its own -------------------------
        //
        // UI-SPEC v1.3 §Favorites: a hearted heart is [`GLYPH_HEART`] solid. It is the same
        // codepoint as the outline one, so the only thing that distinguishes the two states
        // on screen is which family the [`FontId`] names — which makes "the fill family
        // resolves this codepoint, on its own face" the whole contract of the second font.
        let fill = font_icon_fill(ICON_TRANSPORT);
        assert_eq!(fill.family, icon_fill_family());
        let heart = GLYPH_HEART.chars().next().expect("one codepoint");
        assert!(
            (0xE000..=0xF8FF).contains(&(heart as u32)),
            "GLYPH_HEART is not in the Private Use Area"
        );
        assert!(
            ctx.fonts_mut(|f| f.has_glyphs(&fill, GLYPH_HEART)),
            "the filled heart has no glyph — the fill face never loaded"
        );
        assert_eq!(
            ctx.fonts_mut(|f| f.glyph_width(&fill, heart)),
            ICON_TRANSPORT,
            "the filled heart does not advance one em — something other than Phosphor Fill \
             answered for it"
        );
        assert_eq!(
            ctx.fonts_mut(|f| f.definitions().families[&icon_fill_family()].clone()),
            vec![ICON_FILL_FONT_NAME.to_owned()],
            "the fill family must hold the fill face and nothing else"
        );
        // The two faces are different *drawings* of one codepoint, and the metrics cannot
        // see the difference: both advance one em, both lay out to the same box. The atlas
        // can. Rasterise each into it and count the ink that appeared — a solid heart must
        // cover materially more of its box than the outline of the same heart. Without this
        // the "fill" face could be a second copy of the outline one and every assertion
        // above would still pass.
        let outline_ink = ink_added(&ctx, &font_icon(INK_PROBE), heart);
        let filled_ink = ink_added(&ctx, &font_icon_fill(INK_PROBE), heart);
        assert!(
            filled_ink > outline_ink * 3 / 2,
            "the filled heart is not solid: {filled_ink} px of ink against the outline's \
             {outline_ink} — is `Phosphor-Fill.ttf` really the fill weight?"
        );
    }

    /// Size the ink probe rasterises at. Far above any size the app draws, because the
    /// measurement is a pixel count and a 14 px heart is only a few hundred pixels of it.
    const INK_PROBE: f32 = 96.0;

    /// Rasterise `c` from `font` and return how many pixels of ink that added to the font
    /// atlas. The atlas only ever grows, so the difference is this glyph's own coverage.
    fn ink_added(ctx: &Context, font: &FontId, c: char) -> usize {
        let ink = |ctx: &Context| {
            ctx.fonts_mut(|f| f.image().pixels.iter().filter(|p| p.a() > 0).count())
        };
        let before = ink(ctx);
        // Laying the glyph out is what rasterises it: `text_layout` calls
        // `FontImpl::allocate_glyph`, which is the only path that writes coverage into the
        // atlas. `glyph_width` alone would not — it answers from the metrics tables.
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(c.to_string(), font.clone(), Color32::WHITE);
        });
        ink(ctx) - before
    }

    /// The icon face must sit *between* the mono face and the CJK one, and must not answer
    /// for anything either of them already covers. If it ever did, every Latin advance in
    /// the app would change.
    #[test]
    fn the_icon_face_is_a_fallback_and_only_a_fallback() {
        let ctx = egui::Context::default();
        install_style(&ctx, &palette(ThemeMode::Dark, DEFAULT_ACCENT));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(200.0, 100.0),
            )),
            ..Default::default()
        };
        ctx.run_ui(input, |_| {}).textures_delta.clear();

        for family in [FontFamily::Monospace, FontFamily::Proportional] {
            let chain = ctx.fonts_mut(|f| f.definitions().families[&family].clone());
            let tail: Vec<&str> = chain
                .iter()
                .skip(chain.len().saturating_sub(2))
                .map(String::as_str)
                .collect();
            assert_eq!(
                tail,
                [ICON_FONT_NAME, CJK_FONT_NAME],
                "{family:?}: the two bundled fallbacks must be last, in this order ({chain:?})"
            );
            assert_ne!(
                chain.first().map(String::as_str),
                Some(ICON_FONT_NAME),
                "{family:?}: the icon face took over the primary slot"
            );
            assert!(
                !chain.iter().any(|f| f == ICON_FILL_FONT_NAME),
                "{family:?}: the FILL face joined a text family ({chain:?}) — it shares \
                 every codepoint with the outline one, so one of the two would be \
                 unreachable and the app would draw a single weight everywhere"
            );
        }
        // …and the fill family holds nothing but the fill face, so no text can reach it
        // either: it has no mono face to answer for Latin and nothing may be laid out in it.
        assert_eq!(
            ctx.fonts_mut(|f| f.definitions().families[&icon_fill_family()].clone()),
            vec![ICON_FILL_FONT_NAME.to_owned()]
        );
    }

    /// UI-SPEC v1.4 §Panel widths: the two ceilings are chosen together, so that a user who
    /// drags *both* dividers all the way out on the smallest window Phoebus will open still
    /// has a content column that can hold a whole album card and its page padding.
    ///
    /// This is the arithmetic that picked the numbers; if either ceiling is raised, this
    /// test is what says the album grid just went blank at 980 px.
    ///
    /// The second half drives the Artists view's *real* geometry through its draw guard at
    /// the far-right end of the drag, because the arithmetic alone was not enough: the
    /// dynamic ceiling left the album side exactly one card while the guard wanted more
    /// than one, so the one position a drag past the end clamps to was the one position
    /// with no albums on screen — and it persisted.
    #[test]
    fn the_panel_ceilings_leave_an_album_card_in_the_content_column() {
        use crate::views::artists::Split;
        use egui::{Rect, pos2, vec2};

        // Page widths the app can actually produce, plus a fractional sweep: a page rect is
        // not always integral (fractional `pixels_per_point`), and this boundary is exact.
        let pages = std::iter::once(
            // The default 1280 px window with the Up Next drawer open — the width that
            // regressed. Central column 1280 − 230 − 300, less `page`'s padding.
            1280.0 - SIDEBAR_W.default - QUEUE_W.default - 2.0 * VIEW_PAD,
        )
        .chain((0..=4000).map(|i| ARTIST_LIST_W.min + VIEW_PAD + CARD_W + i as f32 * 0.317));
        for full_w in pages {
            for left in [0.0, VIEW_PAD, 230.5, 1013.0 / 3.0] {
                let full = Rect::from_min_size(pos2(left, 12.0), vec2(full_w, 700.0));
                // Dragged as far right as it goes: the drag clamps to exactly the ceiling.
                let geom = Split::of(full, f32::MAX);
                assert!(
                    geom.shows_albums(),
                    "a {full_w} pt page at x={left}, divider dragged to its ceiling of {} pt, \
                     draws no albums at all",
                    Split::ceiling(full_w),
                );
            }
        }
        // …and that ceiling is the dynamic one on the width from the report, not the static
        // 520 pt — otherwise the case above would pass without ever touching the boundary.
        let squeezed = 1280.0 - SIDEBAR_W.default - QUEUE_W.default - 2.0 * VIEW_PAD;
        assert!(
            Split::ceiling(squeezed) < ARTIST_LIST_W.max,
            "the album-card ceiling no longer binds at {squeezed} pt, so this test proves \
             nothing about it"
        );

        let content = WINDOW_MIN[0] - SIDEBAR_W.max - QUEUE_W.max;
        assert!(
            content >= CARD_W + 2.0 * VIEW_PAD,
            "both dividers dragged out on a {} px window leaves {content} px of content, \
             short of one {CARD_W} px card plus its {VIEW_PAD} px padding",
            WINDOW_MIN[0],
        );
        for (name, w) in [
            ("sidebar", SIDEBAR_W),
            ("up next", QUEUE_W),
            ("artist list", ARTIST_LIST_W),
        ] {
            assert!(
                w.min < w.default && w.default < w.max,
                "{name}: a divider whose default sits on a bound can only be dragged one way"
            );
        }
    }
}
