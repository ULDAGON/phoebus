//! Hex color parsing for the persisted accent color.
//!
//! Core owns this (rather than the app) because [`crate::AppState`] validates and
//! normalizes `accent` on load: a hand-edited `state.json` must never be able to hand the
//! UI a color it cannot paint. The app converts the returned `[u8; 3]` into whatever its
//! toolkit uses (`egui::Color32::from_rgb`).

/// Parse `#RRGGBB` into `[r, g, b]`.
///
/// Tolerant in exactly the ways a hand-edited file or a pasted value needs: surrounding
/// whitespace is trimmed, the leading `#` is optional, and the digits may be upper- or
/// lowercase. Anything else — a short form (`#RGB`), an alpha channel (`#RRGGBBAA`), a
/// named color — is `None`, which is what makes this usable as a validator.
pub fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// Render `[r, g, b]` as the canonical `#RRGGBB` (uppercase) form.
///
/// [`crate::AppState`] stores accents in this form, so two spellings of one color compare
/// equal and the JSON on disk stays stable.
pub fn format_hex_color(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_forms_a_settings_view_can_produce() {
        assert_eq!(parse_hex_color("#E8FF2E"), Some([0xE8, 0xFF, 0x2E]));
        assert_eq!(parse_hex_color("e8ff2e"), Some([0xE8, 0xFF, 0x2E]));
        assert_eq!(parse_hex_color("  #2ef0ff  "), Some([0x2E, 0xF0, 0xFF]));
        assert_eq!(parse_hex_color("#000000"), Some([0, 0, 0]));
        assert_eq!(parse_hex_color("#FFFFFF"), Some([255, 255, 255]));
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(parse_hex_color(""), None);
        assert_eq!(parse_hex_color("#"), None);
        assert_eq!(parse_hex_color("#FFF"), None, "no short form");
        assert_eq!(parse_hex_color("#E8FF2EFF"), None, "no alpha");
        assert_eq!(parse_hex_color("#E8FF2G"), None, "not hex");
        assert_eq!(parse_hex_color("neon yellow"), None);
        assert_eq!(parse_hex_color("#E8 FF2E"), None);
    }

    #[test]
    fn round_trips_through_the_canonical_form() {
        for s in ["#E8FF2E", "#2ef0ff", "ff2e9e", " #5DFF2E "] {
            let rgb = parse_hex_color(s).expect("parses");
            let canonical = format_hex_color(rgb);
            assert_eq!(canonical.len(), 7);
            assert!(canonical.starts_with('#'));
            assert_eq!(parse_hex_color(&canonical), Some(rgb));
        }
        assert_eq!(format_hex_color([0, 0, 0]), "#000000");
        assert_eq!(format_hex_color([0xE8, 0xFF, 0x2E]), "#E8FF2E");
    }
}
