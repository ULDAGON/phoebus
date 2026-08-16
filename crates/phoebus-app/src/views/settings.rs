//! The Settings view: where the music lives, and what the app looks like.
//!
//! Two sections, exactly as UI-SPEC §Settings and v1.4 §Sidebar footer word them:
//!
//! * `LIBRARY` — the root as a text input (pre-filled with what is active), an inline
//!   `NOT A DIRECTORY` error, `APPLY & RESCAN` and `RESET TO DEFAULT`, a live count of what
//!   the current root actually yielded, and `RESCAN` for that root as it stands. When
//!   `$PHOEBUS_LIBRARY` is set it outranks everything, so the input is disabled and says so —
//!   but `RESCAN` keeps working, since it changes no setting.
//! * `THEME` — `DARK`/`LIGHT`, six preset swatches, egui's colour picker for anything else,
//!   and `RESET` back to the default yellow. Every change applies on the next frame and is
//!   saved.
//!
//! Under both, pinned below the scroll area rather than inside it, sits the build stamp
//! ([`VERSION`]) in the bottom-right corner.
//!
//! The view owns none of this: it raises [`Action::Rescan`], [`Action::SetLibraryRoot`],
//! [`Action::SetThemeMode`] and [`Action::SetAccent`], and the app decides what happens —
//! including whether the typed path is a directory at all, which is why the error flag lives
//! in [`State`] and is written by the app, not here.

use std::path::{Path, PathBuf};

use egui::{Sense, Ui, Vec2};

use crate::nav::{Action, Ctx};
use crate::theme;
use crate::views;
use crate::widgets;

/// The six accent presets UI-SPEC v1.4 offers, the default yellow first.
pub const PRESETS: [(&str, [u8; 3]); 6] = [
    ("YELLOW", [0xFF, 0xFB, 0x00]),
    ("TEAL", [0x19, 0xB0, 0x92]),
    ("ORANGE", [0xF0, 0x94, 0x1C]),
    ("PURPLE", [0x8B, 0x54, 0xCF]),
    ("WARM GRAY", [0xC6, 0xC2, 0xBB]),
    ("ICE BLUE", [0xE5, 0xF1, 0xFF]),
];

/// Side of a preset swatch.
const SWATCH: f32 = 24.0;
/// Gap between swatches.
const SWATCH_GAP: f32 = 8.0;
/// Outline drawn around the swatch that is currently in use.
const SWATCH_ACTIVE_W: f32 = 2.0;

/// The build stamp: `v` and the workspace version, baked in at compile time so a screenshot
/// of Settings always names the build it came from.
const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// What the Settings view remembers between frames.
///
/// `path` is `None` until the view is first drawn, which is what makes the input pre-fill
/// itself from whatever root is *active* — and re-fill after the app changes the root, since
/// applying a root clears it again.
#[derive(Default)]
pub struct State {
    /// The path being edited, or `None` to take it from [`Info::prefill`] next frame.
    pub path: Option<String>,
    /// The last `APPLY & RESCAN` named something that is not a directory.
    pub not_a_directory: bool,
}

impl State {
    /// Forget the edit, so the input re-fills from the active root.
    pub fn reset_input(&mut self) {
        self.path = None;
        self.not_a_directory = false;
    }
}

/// The library facts the view needs but cannot work out for itself.
#[derive(Clone, Copy)]
pub struct Info<'a> {
    /// The root being scanned right now.
    pub active_root: &'a Path,
    /// `~/.phoebus` — what `RESET TO DEFAULT` restores.
    pub default_root: &'a Path,
    /// `$PHOEBUS_LIBRARY`, when it is set: it outranks the setting, so the input is dead.
    pub env_override: Option<&'a str>,
    /// The root as the user typed it in a previous session, if any.
    pub configured: Option<&'a str>,
}

impl Info<'_> {
    /// What an untouched input shows: the user's own spelling when it is the thing in force,
    /// otherwise the resolved root.
    pub fn prefill(&self) -> String {
        match (self.env_override, self.configured) {
            (Some(_), _) | (None, None) => display_path(self.active_root),
            (None, Some(configured)) => configured.to_string(),
        }
    }
}

/// Draw the view.
pub fn show(ui: &mut Ui, cx: &mut Ctx, st: &mut State, info: &Info) {
    views::page(ui, |ui| {
        // The stamp is taken out of the height BEFORE the scroll area rather than drawn
        // inside it: reserving the strip is what pins the line to the bottom-right corner
        // whether the settings fit or scroll, and what stops it ever landing on `RESCAN` or
        // a swatch on a `WINDOW_MIN`-tall window.
        let stamp = stamp_height(ui);
        let scroll_h = (ui.available_height() - stamp - ui.spacing().item_spacing.y).max(0.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(scroll_h)
            .show(ui, |ui| {
                views::heading(ui, "SETTINGS");
                library_section(ui, cx, st, info);
                ui.add_space(theme::SECTION_GAP);
                theme_section(ui, cx);
            });
        version_stamp(ui, stamp);
    });
}

/// The strip [`version_stamp`] needs: one Small line, plus the air that keeps it off the
/// content above.
fn stamp_height(ui: &Ui) -> f32 {
    theme::CARD_TEXT_GAP + ui.text_style_height(&egui::TextStyle::Small)
}

/// [`VERSION`] in Small `TEXT_LOW`, flush with the bottom-right corner of the reserved
/// strip. Not a widget: it says nothing the user can act on, so it senses nothing.
fn version_stamp(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let color = theme::p().text_low;
    let galley = widgets::truncated(ui, VERSION, theme::font_small(), color, rect.width());
    let pos = egui::pos2(
        rect.right() - galley.size().x,
        rect.bottom() - galley.size().y,
    );
    ui.painter().galley(pos, galley, color);
}

fn library_section(ui: &mut Ui, cx: &mut Ctx, st: &mut State, info: &Info) {
    views::section(ui, "LIBRARY");

    let locked = info.env_override.is_some();
    let text = st.path.get_or_insert_with(|| info.prefill());
    ui.add_enabled(
        !locked,
        egui::TextEdit::singleline(text)
            .font(egui::TextStyle::Body)
            .text_color(theme::p().text_hi)
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(8, 6)),
    );
    ui.add_space(theme::CARD_TEXT_GAP);

    views::subheading(ui, &format!("DEFAULT: {}", display_path(info.default_root)));
    match info.env_override {
        Some(value) => {
            ui.add_space(2.0);
            views::subheading(
                ui,
                &format!(
                    "{} OVERRIDES THIS ROOT — INPUT DISABLED",
                    phoebus_core::LIBRARY_ENV
                ),
            );
            ui.add_space(2.0);
            views::subheading(ui, value);
        }
        None if st.not_a_directory => {
            ui.add_space(2.0);
            views::subheading(ui, &widgets::spaced("NOT A DIRECTORY"));
        }
        None => {}
    }
    ui.add_space(theme::CARD_TEXT_GAP * 1.5);

    ui.horizontal(|ui| {
        if locked {
            widgets::disabled_button(ui, "", "APPLY & RESCAN", phoebus_core::LIBRARY_ENV);
            widgets::disabled_button(ui, "", "RESET TO DEFAULT", phoebus_core::LIBRARY_ENV);
        } else {
            let typed = st.path.clone().unwrap_or_default();
            if widgets::primary_button(ui, "", "APPLY & RESCAN").clicked() {
                cx.act(Action::SetLibraryRoot(Some(typed)));
            }
            if widgets::secondary_button(ui, "", "RESET TO DEFAULT").clicked() {
                cx.act(Action::SetLibraryRoot(None));
            }
        }
    });
    ui.add_space(theme::CARD_TEXT_GAP * 1.5);

    views::subheading(
        ui,
        &format!(
            "{} songs{}{} albums{}{} artists",
            cx.lib.track_count(),
            theme::SEP,
            cx.lib.album_count(),
            theme::SEP,
            cx.lib.artist_count()
        ),
    );
    ui.add_space(theme::CARD_TEXT_GAP);

    // The counts and a plain re-scan of the root already in force: this pair used to sit in
    // the sidebar footer, and UI-SPEC v1.4 §Sidebar footer moves it here whole. It stays
    // live under `$PHOEBUS_LIBRARY` — rescanning changes no setting, so the env override has
    // nothing to say about it, and it is then the only way to pick up new files.
    if widgets::secondary_button(ui, "", "RESCAN").clicked() {
        cx.act(Action::Rescan);
    }
}

fn theme_section(ui: &mut Ui, cx: &mut Ctx) {
    views::section(ui, "THEME");

    // The desktop is driving (an Omarchy theme file): say so, and from where. The
    // swatches below still work — a pick lands on top of the desktop's surfaces until
    // the file next changes — but the file is the reason the palette may not match what
    // was last chosen here.
    if let Some(source) = theme::source() {
        widgets::micro(ui, &format!("FOLLOWING {source}"));
        ui.add_space(theme::CARD_TEXT_GAP);
    }

    let palette = theme::p();
    widgets::micro(ui, "MODE");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for mode in [
            phoebus_core::ThemeMode::Dark,
            phoebus_core::ThemeMode::Light,
        ] {
            let label = mode.as_str().to_ascii_uppercase();
            let clicked = if palette.mode == mode {
                widgets::primary_button(ui, "", &label).clicked()
            } else {
                widgets::secondary_button(ui, "", &label).clicked()
            };
            if clicked {
                cx.act(Action::SetThemeMode(mode));
            }
        }
    });

    ui.add_space(theme::CARD_TEXT_GAP * 1.5);
    widgets::micro(ui, "ACCENT");
    ui.add_space(6.0);

    let current = theme::rgb(palette.accent);
    let mut custom = current;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SWATCH_GAP;
        for (name, rgb) in PRESETS {
            if swatch(ui, rgb, rgb == current, name).clicked() {
                cx.act(Action::SetAccent(rgb));
            }
        }
        // egui's own picker for anything the six presets do not cover. It paints the
        // current accent, so without the label it reads as a seventh preset.
        ui.add_space(SWATCH_GAP);
        widgets::micro(ui, "CUSTOM");
        if ui.color_edit_button_srgb(&mut custom).changed() && custom != current {
            cx.act(Action::SetAccent(custom));
        }
        ui.add_space(SWATCH_GAP);
        if reset_link(ui).clicked() {
            cx.act(Action::SetAccent(PRESETS[0].1));
        }
    });

    ui.add_space(theme::CARD_TEXT_GAP);
    views::subheading(ui, &phoebus_core::format_hex_color(current));
}

/// One 24 px preset square: 1 px `BORDER`, and a 2 px outline in the colour that reads
/// against the accent when it is the one in use.
fn swatch(ui: &mut Ui, rgb: [u8; 3], active: bool, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SWATCH), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::corner(), theme::color(rgb));
    let (width, color) = if active {
        (SWATCH_ACTIVE_W, theme::p().text_hi)
    } else if response.hovered() {
        (theme::HAIRLINE_W, theme::p().text_mid)
    } else {
        (theme::HAIRLINE_W, theme::p().border)
    };
    painter.rect_stroke(
        rect,
        theme::corner(),
        egui::Stroke::new(width, color),
        egui::StrokeKind::Inside,
    );
    response.on_hover_text(
        egui::RichText::new(tooltip)
            .font(theme::font_small())
            .color(theme::p().text_mid),
    )
}

/// `RESET`, `TEXT_LOW` and hover `TEXT_HI` — a link, not a button: it undoes rather than acts.
fn reset_link(ui: &mut Ui) -> egui::Response {
    let text = widgets::spaced("RESET");
    let galley = widgets::truncated(
        ui,
        &text,
        theme::font_small(),
        theme::p().text_low,
        f32::INFINITY,
    );
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(galley.size().x, SWATCH.max(theme::HIT_MIN)),
        Sense::click(),
    );
    let color = theme::hover_color(response.hovered(), theme::p().text_low, theme::p().text_hi);
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    response
}

/// A path with `~` put back in front of the home directory, so the input reads the way the
/// helper line below it does.
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    match home() {
        Some(home) if text == home => "~".to_string(),
        Some(home) if text.starts_with(&format!("{home}/")) => format!("~{}", &text[home.len()..]),
        _ => text,
    }
}

fn home() -> Option<String> {
    let text = phoebus_core::home_dir().display().to_string();
    (!text.is_empty()).then_some(text)
}

/// The absolute path a typed root resolves to: `~` expanded, everything else verbatim.
/// `None` restores [`phoebus_core::default_library_root`].
pub fn resolve_typed(typed: Option<&str>) -> PathBuf {
    let home = phoebus_core::home_dir();
    match typed.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => phoebus_core::expand_tilde(text, &home),
        None => phoebus_core::default_library_root(&home),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_prefills_with_whatever_is_actually_in_force() {
        let active = Path::new("/music/Media");
        let default = Path::new("/home/nobody/.phoebus");

        // Nothing configured: the resolved root.
        let plain = Info {
            active_root: active,
            default_root: default,
            env_override: None,
            configured: None,
        };
        assert_eq!(plain.prefill(), "/music/Media");

        // Configured: the user's own spelling, tilde and all.
        let configured = Info {
            configured: Some("~/Music/Media"),
            ..plain
        };
        assert_eq!(configured.prefill(), "~/Music/Media");

        // The env outranks the setting, so showing the setting would be a lie.
        let overridden = Info {
            env_override: Some("/elsewhere"),
            configured: Some("~/Music/Media"),
            ..plain
        };
        assert_eq!(overridden.prefill(), "/music/Media");
    }

    #[test]
    fn typed_roots_expand_and_default() {
        let home = phoebus_core::home_dir();
        assert_eq!(resolve_typed(Some("  ~/Music  ")), home.join("Music"));
        assert_eq!(resolve_typed(Some("/abs")), PathBuf::from("/abs"));
        assert_eq!(resolve_typed(Some("   ")), home.join(".phoebus"));
        assert_eq!(resolve_typed(None), home.join(".phoebus"));
    }

    /// UI-SPEC v1.4 §Accent presets: exactly these six, in this order — and v1.2 §Colors'
    /// *first swatch = default*, which is what makes `RESET` (it hands back `PRESETS[0]`)
    /// restore the real default.
    #[test]
    fn the_presets_are_the_six_ui_spec_colours_the_default_first() {
        assert_eq!(
            PRESETS.map(|(_, rgb)| rgb),
            [
                [0xFF, 0xFB, 0x00], // yellow (default)
                [0x19, 0xB0, 0x92], // teal
                [0xF0, 0x94, 0x1C], // orange
                [0x8B, 0x54, 0xCF], // purple
                [0xC6, 0xC2, 0xBB], // warm gray
                [0xE5, 0xF1, 0xFF], // ice blue
            ]
        );
        assert_eq!(theme::rgb(theme::DEFAULT_ACCENT), PRESETS[0].1);
        let mut seen: Vec<[u8; 3]> = PRESETS.iter().map(|(_, rgb)| *rgb).collect();
        seen.sort_unstable();
        let unique = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), unique, "a duplicate preset");
        assert_eq!(
            phoebus_core::format_hex_color(PRESETS[0].1),
            phoebus_core::DEFAULT_ACCENT
        );
    }

    /// The stamp is a compile-time `concat!`, so the only thing that can go wrong is the
    /// shape: a leading `v` and the three numeric components of the workspace version.
    #[test]
    fn the_version_stamp_reads_as_a_version() {
        let digits = VERSION.strip_prefix('v').expect("no leading v");
        let parts: Vec<&str> = digits.split('.').collect();
        assert_eq!(parts.len(), 3, "{VERSION} is not major.minor.patch");
        for part in parts {
            assert!(
                part.starts_with(|c: char| c.is_ascii_digit()),
                "{VERSION} has a non-numeric component"
            );
        }
    }

    #[test]
    fn display_path_puts_the_tilde_back() {
        let home = phoebus_core::home_dir();
        assert_eq!(display_path(&home), "~");
        assert_eq!(display_path(&home.join(".phoebus")), "~/.phoebus");
        assert_eq!(display_path(Path::new("/opt/music")), "/opt/music");
    }
}
