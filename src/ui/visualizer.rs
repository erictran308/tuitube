//! The visualizer's bars: thin vertical lines a column wide with a column
//! between them, as many as fit, spread over the bands from bass to treble,
//! each topped to an eighth of a row, in colors going round the rainbow
//! across them and slowly over time.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::visualizer::{BANDS, Spectrum};

/// A bar's top, filled an eighth more each (0 isn't drawn).
const EIGHTHS: [&str; 8] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];

/// Columns per bar: the bar, then the gap before the next.
const SLOT: u16 = 2;

/// How much of the rainbow lies across the bars, in degrees: red to
/// magenta, not back round to red.
const HUE_SPAN: f32 = 300.0;

/// Draws the bars in `area`, centered, over what's there (the player bar's
/// background shows between them).
pub(super) fn draw(buf: &mut Buffer, area: Rect, spectrum: &Spectrum) {
    // The last bar needs no gap after it.
    let bars = (area.width + 1) / SLOT;
    if bars == 0 || area.height == 0 {
        return;
    }
    let heights = spectrum.heights();
    let rows = usize::from(area.height);
    let left = area.x + (area.width - (bars * SLOT - 1)) / 2;
    for bar in 0..bars {
        let across = if bars == 1 {
            0.5
        } else {
            f32::from(bar) / f32::from(bars - 1)
        };
        let eighths = (level(heights, across) * (rows * 8) as f32).round() as usize;
        let hue = spectrum.hue() + across * HUE_SPAN;
        let x = left + bar * SLOT;
        for row in 0..rows {
            let filled = eighths.saturating_sub(row * 8).min(8);
            if filled == 0 {
                break;
            }
            let symbol = if filled == 8 { "█" } else { EIGHTHS[filled] };
            // Brighter toward the top.
            let value = 0.6 + 0.4 * (row + 1) as f32 / rows as f32;
            buf[(x, area.bottom() - 1 - row as u16)]
                .set_symbol(symbol)
                .set_fg(hsv(hue, 0.75, value));
        }
    }
}

/// The height `across` the bands (0 the lowest, 1 the highest), between
/// the two bands it falls between, 0 to 1.
fn level(heights: &[f32; BANDS], across: f32) -> f32 {
    let at = across.clamp(0.0, 1.0) * (BANDS - 1) as f32;
    let i = (at.floor() as usize).min(BANDS - 2);
    let t = at - i as f32;
    (heights[i] * (1.0 - t) + heights[i + 1] * t).clamp(0.0, 1.0)
}

/// A color from its hue (degrees), saturation and value (0 to 1).
fn hsv(hue: f32, saturation: f32, value: f32) -> Color {
    let hue = hue.rem_euclid(360.0) / 60.0;
    let chroma = value * saturation;
    let x = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let (r, g, b) = match hue as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = value - chroma;
    let byte = |c: f32| ((c + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(byte(r), byte(g), byte(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn each_bar_is_its_bands_height_or_between_two() {
        let mut heights = [0.0; BANDS];
        heights[3] = 1.0;
        heights[4] = 0.5;
        let at = |band: usize| band as f32 / (BANDS - 1) as f32;
        assert_eq!(level(&heights, at(3)), 1.0);
        assert_eq!(level(&heights, at(4)), 0.5);
        assert!((level(&heights, (at(3) + at(4)) / 2.0) - 0.75).abs() < 1e-5);
        assert_eq!(level(&heights, at(9)), 0.0);
        assert_eq!(level(&[1.0; BANDS], 1.0), 1.0, "the right edge");
    }

    #[test]
    fn bars_are_lines_with_a_gap_between_them() {
        // Loud everywhere, for long enough that every bar is half up or more.
        let mut spectrum = Spectrum::default();
        for _ in 0..12 {
            spectrum.hear([0.0; BANDS], Instant::now());
        }
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        draw(&mut buf, area, &spectrum);
        let row: String = (0..10).map(|x| buf[(x, 1)].symbol()).collect();
        // Five bars in nine columns, centered in ten.
        assert_eq!(row, "█ █ █ █ █ ");
        let colors: std::collections::HashSet<String> = (0..5)
            .map(|i| format!("{:?}", buf[(i * 2, 1)].fg))
            .collect();
        assert_eq!(colors.len(), 5, "each its own color");
    }

    #[test]
    fn colors_go_round_the_rainbow() {
        assert_eq!(hsv(0.0, 1.0, 1.0), Color::Rgb(255, 0, 0));
        assert_eq!(hsv(120.0, 1.0, 1.0), Color::Rgb(0, 255, 0));
        assert_eq!(hsv(240.0, 1.0, 1.0), Color::Rgb(0, 0, 255));
        assert_eq!(hsv(360.0 + 120.0, 1.0, 1.0), hsv(120.0, 1.0, 1.0));
        assert_eq!(hsv(-120.0, 1.0, 1.0), hsv(240.0, 1.0, 1.0));
    }
}
