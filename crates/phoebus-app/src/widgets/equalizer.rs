//! The three-bar "this one is playing" equalizer that stands where a track number would be
//! (UI-SPEC v1.2 §Track rows).
//!
//! It is painted, not animated by a texture and not spelled with a glyph: three `ACCENT`
//! bars whose heights are sampled from one cosine each. The three phases are deliberately
//! *not* evenly spaced around the 1.2 s loop — at 1/3-turn offsets the bars read as a wave
//! marching sideways, which looks like a progress indicator; at these they look like an
//! analyser.
//!
//! ## Repaint pacing
//!
//! Frame pacing is this widget's own business, and it is the only reason the app ever needs
//! to run faster than its usual 250 ms playing tick. [`paint`] therefore asks for the next
//! frame itself, and only when it actually drew a *moving* equalizer: a view with nothing
//! playing never calls it, and a paused row calls it with `None` and gets frozen bars and no
//! repaint request. One equalizer on screen ⇒ ~12.5 fps; none ⇒ whatever the rest of the app
//! asked for.

use std::time::Duration;

use egui::{Color32, CornerRadius, Rect, Ui};

/// Bars in one equalizer.
pub const BARS: usize = 3;
/// Length of the animation loop, in seconds (UI-SPEC v1.2: ~1.2 s).
pub const PERIOD: f64 = 1.2;
/// Phase of each bar as a fraction of a loop. Co-prime-ish offsets: no two bars share a
/// crest, and the set never repeats inside one period.
pub const PHASE: [f64; BARS] = [0.0, 0.37, 0.71];
/// Shortest a bar ever gets, as a fraction of the box height — a bar that reaches zero
/// reads as a rendering glitch, not as a quiet band.
pub const MIN: f32 = 0.25;
/// Width of one bar.
pub const BAR_W: f32 = 3.5;
/// Gap between two bars.
pub const BAR_GAP: f32 = 2.5;
/// Height of the box the bars stand in.
pub const HEIGHT: f32 = 16.0;
/// Total width of one equalizer.
pub const WIDTH: f32 = BARS as f32 * BAR_W + (BARS as f32 - 1.0) * BAR_GAP;
/// Gap to the next frame while an equalizer is on screen. UI-SPEC v1.2 asks for ≤ 100 ms;
/// 80 ms is ~15 samples per loop, which is smooth at this size without spinning the CPU.
pub const REPAINT: Duration = Duration::from_millis(80);

/// Height of bar `bar` at `time` seconds, as a fraction of the box ([`MIN`]..=1.0).
///
/// Pure, so the animation can be unit-tested without a window: the only thing [`paint`]
/// adds is where the rectangles land.
pub fn level(bar: usize, time: f64) -> f32 {
    let phase = PHASE[bar % BARS];
    let turn = (time / PERIOD + phase) * std::f64::consts::TAU;
    // 0 at the trough, 1 at the crest.
    let unit = (0.5 - 0.5 * turn.cos()) as f32;
    MIN + (1.0 - MIN) * unit
}

/// Paint one equalizer, bottom-aligned and centred in `rect`.
///
/// `time` is `Some(seconds)` — pass `ui.input(|i| i.time)` — while the track is playing, and
/// `None` while it is paused, which freezes every bar at its `t = 0` pose. That pose is three
/// visibly different heights on purpose: a paused row still has to read as "this is the
/// track", and three equal stubs would read as a dead widget.
pub fn paint(ui: &Ui, rect: Rect, color: Color32, time: Option<f64>) {
    let t = time.unwrap_or(0.0);
    let box_h = HEIGHT.min(rect.height());
    let bottom = (rect.center().y + box_h * 0.5).round();
    let left = (rect.center().x - WIDTH * 0.5).round();
    let painter = ui.painter();
    for bar in 0..BARS {
        let h = (box_h * level(bar, t)).max(1.0).round();
        let x = left + bar as f32 * (BAR_W + BAR_GAP);
        painter.rect_filled(
            Rect::from_min_max(egui::pos2(x, bottom - h), egui::pos2(x + BAR_W, bottom)),
            CornerRadius::ZERO,
            color,
        );
    }
    // Only a moving equalizer costs frames. See the module header.
    if time.is_some() {
        ui.ctx().request_repaint_after(REPAINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The animation is a fraction of a box, never a pixel count: a bar may not leave its
    /// box, and may not vanish either.
    #[test]
    fn every_bar_stays_between_its_floor_and_the_ceiling() {
        for step in 0..240 {
            let t = step as f64 * 0.01;
            for bar in 0..BARS {
                let level = level(bar, t);
                assert!(
                    (MIN..=1.0).contains(&level),
                    "bar {bar} at t={t} is {level}"
                );
            }
        }
    }

    /// The loop is [`PERIOD`] long — the whole point of driving it off `i.time`, which never
    /// resets, is that it is periodic rather than cumulative.
    #[test]
    fn the_animation_repeats_once_per_period() {
        for bar in 0..BARS {
            for step in 0..12 {
                let t = step as f64 * 0.1;
                let now = level(bar, t);
                let later = level(bar, t + PERIOD);
                assert!(
                    (now - later).abs() < 1e-5,
                    "bar {bar} drifted by {} after one loop",
                    now - later
                );
            }
        }
    }

    /// Independent phases: the three bars must never be the same height at the same moment,
    /// which is exactly what a single shared phase would produce.
    #[test]
    fn the_three_bars_move_independently() {
        let mut agreed = 0;
        for step in 0..240 {
            let t = step as f64 * 0.005;
            let (a, b, c) = (level(0, t), level(1, t), level(2, t));
            if (a - b).abs() < 0.01 && (b - c).abs() < 0.01 {
                agreed += 1;
            }
        }
        assert_eq!(agreed, 0, "the bars moved as one block");
    }

    /// The frozen (paused) pose is `t = 0`, and it has to be legible: three different
    /// heights, none of them the full box.
    #[test]
    fn the_frozen_pose_is_three_distinct_heights() {
        let heights: Vec<f32> = (0..BARS).map(|bar| level(bar, 0.0)).collect();
        for (i, a) in heights.iter().enumerate() {
            for b in &heights[i + 1..] {
                assert!(
                    (a - b).abs() > 0.05,
                    "the frozen bars {a} and {b} are the same height"
                );
            }
        }
        assert_eq!(heights[0], MIN, "bar 0 sits at its trough at t = 0");
    }

    /// UI-SPEC v1.2 puts a 100 ms ceiling on the repaint the equalizer asks for.
    #[test]
    fn the_repaint_request_is_inside_the_spec() {
        assert!(REPAINT <= Duration::from_millis(100));
        assert!(
            REPAINT >= Duration::from_millis(30),
            "no free-running frames"
        );
    }
}
