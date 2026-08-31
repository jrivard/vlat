// vlat - A colorful, pretty and over-engineered yet easy to use ping monitoring utility.
// Copyright (C) 2026  Jason D. Rivard <code@jrivard.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Bubble view.
//!
//! Each target is rendered as a floating filled oval whose size is proportional
//! to its windowed average RTT (larger = slower).  Bubbles drift around the
//! screen with gentle physics: higher jitter makes a bubble move faster and
//! change direction more erratically; drops flash the bubble red.  Soft
//! repulsion keeps them from permanently stacking on top of each other.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Paragraph},
};
use crate::cli::SortMode;
use crate::state::TargetState;
use super::ViewCtx;
use super::{
    compute_no_data,
    lerp_rgb,
    layout::{build_legend_line, render_dialogs, render_window_label},
};

// ── visual constants ──────────────────────────────────────────────────────────

// Terminal cells are roughly 2:1 height:width in pixels.  To render a visually
// round bubble we give it twice as many columns as rows.
const ASPECT: f64 = 2.0;

// Minimum and maximum bubble row-radius.
const MIN_RY: f64 = 2.0;
const MAX_RY: f64 = 12.0;

// ── motion constants ──────────────────────────────────────────────────────────

// Base speed (rows/step) at zero jitter, and extra speed added at max jitter.
const BASE_SPEED:   f64 = 0.025;
const JITTER_SPEED: f64 = 0.14;

// Maximum random angle perturbation (radians) per step at zero / max jitter.
const BASE_PERTURB:   f64 = 0.06;
const JITTER_PERTURB: f64 = 0.55;

// Bubbles start pushing each other when their distance falls below this multiple
// of the sum of their radii.
const REPULSION_MARGIN: f64 = 1.3;

// Strength of the spring that pulls each bubble toward its metric-driven
// preferred vertical position (low metric = top, high metric = bottom).
const Y_SPRING: f64 = 0.008;

// ── types ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Bubble {
    x:          f64,   // center column (float for sub-cell motion)
    y:          f64,   // center row
    vx:         f64,   // velocity in cols/step
    vy:         f64,   // velocity in rows/step
    display_ry: f64,   // current row-radius (EMA-smoothed toward target)
}

#[derive(Clone)]
pub struct BubbleState {
    bubbles: Vec<Bubble>,
    rng:     u64,
}

impl BubbleState {
    pub fn new(n: usize, w: u16, h: u16) -> Self {
        let seed = crate::time::SystemTime::now()
            .duration_since(crate::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEAD_BEEF_1337_CAFE);
        let mut rng = seed;
        let bubbles = (0..n).map(|_| mk_bubble(&mut rng, w, h)).collect();
        BubbleState { bubbles, rng }
    }

    pub fn step(&mut self, states: &[TargetState], global_max_jitter: f64, w: u16, h: u16, sort_mode: &SortMode) {
        if w == 0 || h == 0 { return; }
        let n = states.len();

        while self.bubbles.len() < n {
            let b = mk_bubble(&mut self.rng, w, h);
            self.bubbles.push(b);
        }
        self.bubbles.truncate(n);

        let down: Vec<bool> = states.iter().map(|s| !s.waiting && bubble_is_down(s)).collect();
        let down_count = down.iter().filter(|&&d| d).count() as u16;

        let global_max_metric = states.iter().zip(&down)
            .filter(|(s, &d)| !s.waiting && !d)
            .map(|(s, _)| bubble_metric(s, sort_mode))
            .fold(0.0f64, f64::max)
            .max(1.0);

        let wf = w as f64;
        // Down targets are pinned to fixed half-bubble markers at the bottom
        // (drawn separately, outside the physics sim) rather than floating -
        // reserve that strip so a real bubble can't drift onto it.
        let ry_down    = down_marker_ry(w, h, down_count);
        let reserved_h = down_reserved_rows(h, ry_down);
        let hf = (h as f64 - reserved_h as f64).max(1.0);

        // Per-bubble position and radius update
        for (i, bubble) in self.bubbles.iter_mut().enumerate() {
            if down[i] { continue; }
            let state = states.get(i);

            let jitter_norm = jitter_norm(state, global_max_jitter);

            // EMA-smooth radius toward data-driven target
            let metric_val = state.map_or(0.0, |s| bubble_metric(s, sort_mode));
            let norm = (metric_val / global_max_metric).clamp(0.0, 1.0);
            let target_ry = MIN_RY + norm * (MAX_RY - MIN_RY);
            bubble.display_ry = bubble.display_ry * 0.92 + target_ry * 0.08;
            let ry = bubble.display_ry;
            let rx = ry * ASPECT;

            // Compute new speed and apply random angular perturbation
            let speed      = BASE_SPEED + jitter_norm * JITTER_SPEED;
            let max_perturb = BASE_PERTURB + jitter_norm * JITTER_PERTURB;
            let perturb = ((rng_next(&mut self.rng) as f64 / u64::MAX as f64) * 2.0 - 1.0)
                          * max_perturb;

            let (vx, vy) = rotate(bubble.vx, bubble.vy, perturb);

            // Re-normalise to target speed in logical (isotropic) space then
            // convert back to screen-space (cols, rows).
            let mag_logical = ((vx / ASPECT).powi(2) + vy.powi(2)).sqrt();
            if mag_logical > 1e-9 {
                bubble.vx = (vx / ASPECT) / mag_logical * speed * ASPECT;
                bubble.vy =  vy            / mag_logical * speed;
            }

            bubble.x += bubble.vx;
            bubble.y += bubble.vy;

            // Spring: pull toward the vertical position implied by the metric.
            // Low metric → top of screen, high metric → bottom.
            let preferred_y = ry + norm * (hf - 2.0 * ry).max(0.0);
            bubble.vy += (preferred_y - bubble.y) * Y_SPRING;

            // Wall bounce (keep centre at least radius away from each edge)
            if bubble.x - rx < 0.0 { bubble.x = rx;       if bubble.vx < 0.0 { bubble.vx = -bubble.vx; } }
            if bubble.x + rx > wf  { bubble.x = wf - rx;  if bubble.vx > 0.0 { bubble.vx = -bubble.vx; } }
            if bubble.y - ry < 0.0 { bubble.y = ry;       if bubble.vy < 0.0 { bubble.vy = -bubble.vy; } }
            if bubble.y + ry > hf  { bubble.y = hf - ry;  if bubble.vy > 0.0 { bubble.vy = -bubble.vy; } }
        }

        // Accumulate repulsion forces in a separate array to avoid borrow conflicts
        let mut forces: Vec<(f64, f64)> = vec![(0.0, 0.0); n];
        for i in 0..n {
            if down[i] { continue; }
            for j in (i + 1)..n {
                if down[j] { continue; }
                let (xi, yi, ri) = (self.bubbles[i].x, self.bubbles[i].y, self.bubbles[i].display_ry);
                let (xj, yj, rj) = (self.bubbles[j].x, self.bubbles[j].y, self.bubbles[j].display_ry);
                let dx = xi - xj;
                let dy = yi - yj;
                // Distance in logical (isotropic) space
                let dist_logical = ((dx / ASPECT).powi(2) + dy.powi(2)).sqrt();
                let min_logical  = (ri + rj) * REPULSION_MARGIN;
                if dist_logical < min_logical && dist_logical > 1e-3 {
                    let overlap = min_logical - dist_logical;
                    let force   = (overlap / min_logical) * 0.12;
                    let nx = (dx / ASPECT) / dist_logical;
                    let ny = dy              / dist_logical;
                    forces[i].0 += nx * force * ASPECT;
                    forces[i].1 += ny * force;
                    forces[j].0 -= nx * force * ASPECT;
                    forces[j].1 -= ny * force;
                }
            }
        }

        let max_speed = BASE_SPEED + JITTER_SPEED;
        for (i, bubble) in self.bubbles.iter_mut().enumerate() {
            if down[i] { continue; }
            bubble.vx += forces[i].0;
            bubble.vy += forces[i].1;
            // Cap speed so a pile-up can't launch bubbles off-screen
            let mag = ((bubble.vx / ASPECT).powi(2) + bubble.vy.powi(2)).sqrt();
            if mag > max_speed * 1.5 {
                bubble.vx = bubble.vx / mag * max_speed * 1.5;
                bubble.vy = bubble.vy / mag * max_speed * 1.5;
            }
        }
    }
}

// ── private helpers ───────────────────────────────────────────────────────────

fn mk_bubble(rng: &mut u64, w: u16, h: u16) -> Bubble {
    let wf = w.max(4) as f64;
    let hf = h.max(4) as f64;
    let x = (rng_next(rng) as f64 / u64::MAX as f64) * wf;
    let y = (rng_next(rng) as f64 / u64::MAX as f64) * hf;
    let angle = (rng_next(rng) as f64 / u64::MAX as f64) * std::f64::consts::TAU;
    Bubble {
        x, y,
        vx: angle.cos() * BASE_SPEED * ASPECT,
        vy: angle.sin() * BASE_SPEED,
        display_ry: MIN_RY,
    }
}

fn rotate(vx: f64, vy: f64, angle: f64) -> (f64, f64) {
    let (s, c) = angle.sin_cos();
    (vx * c - vy * s, vx * s + vy * c)
}

fn rng_next(s: &mut u64) -> u64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *s >> 17
}

/// True when a target has no successful pings in the current window (all
/// recent probes were drops). Its RTT/loss metric is meaningless (win_avg
/// reads 0.0, the "best" possible value), so it can't be sized/colored as a
/// normal bubble - it's drawn as a fixed half-bubble marker instead. Mirrors
/// the "all_dropping" check used by the radar/worm/scatter views.
fn bubble_is_down(state: &TargetState) -> bool {
    state.window.is_empty() && state.win_drops > 0
}

// Bounds for the down-target flatline marker's half-width scale: a few
// columns wide rather than the full MIN_RY..MAX_RY range live bubbles use,
// since size here is just a fixed viewport-scaled presentation, not
// data-driven. Kept smaller than live bubbles get to (~half the max) so a
// row of dead targets reads as a subdued strip along the floor, not a
// competing row of bubbles.
const DOWN_MIN_RY: f64 = 1.0;
const DOWN_MAX_RY: f64 = 3.0;

/// Half-width scale (in bubble-radius units, for consistency with live
/// bubble sizing) for down-target flatline markers - scaled gently with the
/// viewport but capped so they can't crowd out the live play area, and
/// shrunk to fit side-by-side with the other down markers across the
/// available width.
fn down_marker_ry(w: u16, h: u16, n_down: u16) -> f64 {
    if n_down == 0 { return 0.0; }
    let pref       = (h as f64 * 0.12).clamp(DOWN_MIN_RY, DOWN_MAX_RY);
    let slot_w     = w as f64 / n_down as f64;
    let max_rx     = (slot_w * 0.8) / 2.0;
    let max_ry_fit = (max_rx / ASPECT).max(1.0);
    pref.min(max_ry_fit)
}

/// Rows reserved at the bottom of the bubble area for down-target markers:
/// the flatline (bottom row) plus one clear row above it for the target's
/// label.
fn down_reserved_rows(h: u16, ry: f64) -> u16 {
    if ry <= 0.0 { return 0; }
    (ry.ceil() as u16 + 1).min(h.saturating_sub(2).max(1))
}

fn jitter_norm(state: Option<&TargetState>, global_max_jitter: f64) -> f64 {
    if global_max_jitter <= 0.0 { return 0.0; }
    let j = state.map_or(0.0, |s| if s.waiting { 0.0 } else { s.win_jitter_avg() });
    (j / global_max_jitter).clamp(0.0, 1.0)
}

/// Return the metric value used to size this bubble.  When the sort mode is a
/// numeric metric the bubble size reflects that metric; otherwise latency.
pub fn bubble_metric(state: &TargetState, sort: &SortMode) -> f64 {
    if state.waiting { return 0.0; }
    match sort {
        SortMode::None | SortMode::Name => state.win_avg(),
        SortMode::Avg    => state.win_avg(),
        SortMode::Loss   => state.win_loss_pct(),
        SortMode::Std    => state.win_stddev(),
        SortMode::Jitter => state.win_jitter_avg(),
        SortMode::Streak => state.cur_drop_streak as f64,
        SortMode::P50    => state.win_median(),
        SortMode::P95    => state.win_p95(),
        SortMode::P99    => state.win_p99(),
        SortMode::P01    => state.win_p01(),
        SortMode::P10    => state.win_p10(),
        SortMode::Cv     => state.win_cv(),
        SortMode::Srtt   => state.srtt,
        SortMode::Mtr    => state.win_mtr().unwrap_or_else(|| state.win_avg()),
    }
}

// ── draw ──────────────────────────────────────────────────────────────────────

// Braille dot bit layout within one terminal cell (2 sub-cols × 4 sub-rows):
//   sub-col: 0     1
//   row 0:  0x01  0x08
//   row 1:  0x02  0x10
//   row 2:  0x04  0x20
//   row 3:  0x40  0x80
// Adding the set bits to U+2800 gives the braille character.
const BRAILLE_BITS: [[u8; 2]; 4] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

/// Draw a single filled oval into `buf` using braille characters.
/// Each terminal cell covers a 2×4 grid of braille dots, giving 4× vertical
/// and 2× horizontal resolution compared to solid-block rendering.
/// `bx`/`by` are the bubble centre in bubble-area coordinates.
#[allow(clippy::too_many_arguments)]
fn draw_oval(
    buf:        &mut ratatui::buffer::Buffer,
    area:       Rect,
    bx:         f64,
    by:         f64,
    ry:         f64,
    base_rgb:   (u8, u8, u8),
    drop_rgb:   (u8, u8, u8),
    drop_flash: u8,
) {
    let rx = ry * ASPECT;

    // Sub-pixel radii: each terminal col = 2 braille sub-cols,
    // each terminal row = 4 braille sub-rows.  Sub-pixels are square,
    // so rx_sub = ry_sub for a visually round circle (ASPECT = 2.0 cancels).
    let rx_sub = rx * 2.0;
    let ry_sub = ry * 4.0;
    let cx_sub = bx * 2.0;
    let cy_sub = by * 4.0;

    let col_lo = (bx - rx - 1.0).floor() as i32;
    let col_hi = (bx + rx + 1.0).ceil()  as i32;
    let row_lo = (by - ry - 1.0).floor() as i32;
    let row_hi = (by + ry + 1.0).ceil()  as i32;

    let flash_t = if drop_flash > 0 { (drop_flash as f64 / 6.0).clamp(0.0, 1.0) } else { 0.0 };

    for tc_row in row_lo..=row_hi {
        for tc_col in col_lo..=col_hi {
            let screen_col = area.x as i32 + tc_col;
            let screen_row = area.y as i32 + tc_row;
            if screen_col < area.x as i32
                || screen_col >= (area.x + area.width)  as i32
                || screen_row < area.y as i32
                || screen_row >= (area.y + area.height) as i32
            {
                continue;
            }

            let mut bits: u8 = 0;
            let mut d_sum: f64 = 0.0;
            let mut n_lit: u32 = 0;

            for (dr, row_bits) in BRAILLE_BITS.iter().enumerate() {
                for (dc, &bit) in row_bits.iter().enumerate() {
                    let sx = tc_col as f64 * 2.0 + dc as f64 + 0.5;
                    let sy = tc_row  as f64 * 4.0 + dr as f64 + 0.5;
                    let d = ((sx - cx_sub) / rx_sub).powi(2)
                          + ((sy - cy_sub) / ry_sub).powi(2);
                    if d <= 1.0 {
                        bits |= bit;
                        d_sum += d;
                        n_lit += 1;
                    }
                }
            }

            if bits == 0 { continue; }

            // Radial brightness shading based on average depth of lit dots
            let avg_d = d_sum / n_lit as f64;
            let brightness = (1.0 - avg_d * 0.45).max(0.55);
            let (r, g, b) = (
                (base_rgb.0 as f64 * brightness) as u8,
                (base_rgb.1 as f64 * brightness) as u8,
                (base_rgb.2 as f64 * brightness) as u8,
            );
            let (r, g, b) = lerp_rgb(r, g, b, drop_rgb.0, drop_rgb.1, drop_rgb.2, flash_t);

            let braille_ch = char::from_u32(0x2800u32 + bits as u32).unwrap_or('⠿');
            let cell = &mut buf[(screen_col as u16, screen_row as u16)];
            cell.set_symbol(&braille_ch.to_string());
            cell.set_style(Style::default().fg(Color::Rgb(r, g, b)).bg(Color::Black));
        }
    }
}

/// Draw a flatlined marker for a down target: a flat baseline of bottom-row
/// braille dots with a small upward tick at its centre, like a heart monitor
/// with no pulse. Drawn on the bottom row of `area`, spanning `rx` columns
/// either side of `bx`.
fn draw_flatline(
    buf:      &mut ratatui::buffer::Buffer,
    area:     Rect,
    bx:       f64,
    rx:       f64,
    base_rgb: (u8, u8, u8),
) {
    if area.height == 0 { return; }
    let row = area.height - 1;

    let col_lo = (bx - rx).floor().max(0.0) as i32;
    let col_hi = ((bx + rx).ceil() as i32).min(area.width as i32 - 1);
    if col_hi < col_lo { return; }
    let mid = (col_lo + col_hi) / 2;

    const BASELINE: u8 = 0x40 | 0x80;           // bottom-row dots, both columns
    const BLIP:     u8 = 0x01 | 0x08 | BASELINE; // + top-row dots for the tick

    let style = Style::default()
        .fg(Color::Rgb(base_rgb.0, base_rgb.1, base_rgb.2))
        .bg(Color::Black);

    for tc_col in col_lo..=col_hi {
        let screen_col = area.x as i32 + tc_col;
        if screen_col < area.x as i32 || screen_col >= (area.x + area.width) as i32 { continue; }
        let bits = if tc_col == mid { BLIP } else { BASELINE };
        let ch = char::from_u32(0x2800u32 + bits as u32).unwrap_or('⣀');
        let cell = &mut buf[(screen_col as u16, area.y + row)];
        cell.set_symbol(&ch.to_string());
        cell.set_style(style);
    }
}

/// Draw the target name and ms value centered inside the bubble. Callers are
/// expected to have already checked `label_fits_inside` - this always draws
/// both lines regardless of whether they'd spill past the bubble's edge.
#[allow(clippy::too_many_arguments)]
fn draw_bubble_label(
    buf:      &mut ratatui::buffer::Buffer,
    area:     Rect,
    bx:       f64,
    by:       f64,
    _ry:      f64,
    label:    &str,
    avg_ms:   f64,
    base_rgb: (u8, u8, u8),
) {
    let ms_str = fmt_bubble_ms(avg_ms);

    // Bubble color as fg on black bg — readable everywhere, no colored block.
    let text_style = Style::default()
        .fg(Color::Rgb(base_rgb.0, base_rgb.1, base_rgb.2))
        .bg(Color::Black)
        .add_modifier(Modifier::BOLD);

    let center_col = bx.round() as i32;
    let center_row = by.round() as i32;

    write_centered(buf, area, center_col, center_row - 1, label,    text_style);
    write_centered(buf, area, center_col, center_row,     &ms_str,  text_style);
}

fn write_centered(
    buf:   &mut ratatui::buffer::Buffer,
    area:  Rect,
    cx:    i32,
    cy:    i32,
    text:  &str,
    style: Style,
) {
    if cy < 0 || cy >= area.height as i32 { return; }
    let chars: Vec<char> = text.chars().collect();
    let start_col = cx - (chars.len() as i32 / 2);
    for (i, ch) in chars.iter().enumerate() {
        let col = start_col + i as i32;
        if col < 0 || col >= area.width as i32 { continue; }
        let cell = &mut buf[(area.x + col as u16, area.y + cy as u16)];
        cell.set_symbol(&ch.to_string());
        cell.set_style(style);
    }
}

fn fmt_bubble_ms(avg_ms: f64) -> String {
    if avg_ms >= 1000.0 {
        format!("{:.1}s", avg_ms / 1000.0)
    } else {
        format!("{:.0}ms", avg_ms)
    }
}

/// A live (non-down) bubble's screen geometry and styling, snapshotted after
/// the oval is drawn so the label pass can decide - per bubble - whether its
/// name/value fit inside the oval or need to be moved outside it.
struct LiveBubble {
    slot:       usize,
    bx:         f64,
    by:         f64,
    ry:         f64,
    rx:         f64,
    base_rgb:   (u8, u8, u8),
    metric_val: f64,
}

/// Approximate on-screen bounding box of a bubble, matching the padding
/// `draw_oval` itself uses - used so external labels/leader lines never land
/// on top of an unrelated bubble.
fn bubble_bounds(bx: f64, by: f64, ry: f64, rx: f64) -> (i32, i32, i32, i32) {
    let col_lo = (bx - rx - 1.0).floor() as i32;
    let col_hi = (bx + rx + 1.0).ceil()  as i32;
    let row_lo = (by - ry - 1.0).floor() as i32;
    let row_hi = (by + ry + 1.0).ceil()  as i32;
    (col_lo, col_hi, row_lo, row_hi)
}

/// True when both label rows (`draw_bubble_label` writes the name one row
/// above center, the ms value on the center row) land fully on-screen *and*
/// fully within the bubble's circular silhouette at that row - i.e. the text
/// would actually be readable sitting on top of the bubble, not spilling
/// past its edge onto the black background beyond.
fn label_fits_inside(
    area_w:  u16,
    max_row: i32,
    bx: f64, by: f64, ry: f64, rx: f64,
    label_len: usize, ms_len: usize,
) -> bool {
    let half_w = |dy: f64| -> f64 {
        if ry <= 0.0 { return 0.0; }
        let t = dy / ry;
        if t.abs() >= 1.0 { 0.0 } else { rx * (1.0 - t * t).sqrt() }
    };
    if (half_w(1.0).floor() * 2.0) < label_len as f64 { return false; }
    if (half_w(0.0).floor() * 2.0) < ms_len as f64    { return false; }

    let center_col = bx.round() as i32;
    let center_row = by.round() as i32;
    let row_ok = |cy: i32, len: usize| -> bool {
        if cy < 0 || cy >= max_row { return false; }
        let start = center_col - (len as i32 / 2);
        start >= 0 && start + len as i32 <= area_w as i32
    };
    row_ok(center_row - 1, label_len) && row_ok(center_row, ms_len)
}

/// Draw every live bubble's name/value label: inside the bubble when it
/// fits (as before), or - when the bubble is too small or too close to the
/// edge to hold it - as a compact label placed outside the bubble and tied
/// back to it with a dotted leader line, the same technique the scatter view
/// uses for crowded/overlapping points.
fn draw_bubble_labels(
    buf:     &mut ratatui::buffer::Buffer,
    area:    Rect,
    max_row: i32,
    states:  &[TargetState],
    live:    &[LiveBubble],
    ascii:   bool,
) {
    let leader_ch = if ascii { ":" } else { "\u{250a}" };

    // Occupancy seeded with every bubble's bounding box, so external labels
    // and leader lines never get drawn over an unrelated bubble.
    let mut row_occupied: std::collections::HashMap<i32, Vec<(i32, i32)>> = std::collections::HashMap::new();
    for b in live {
        let (col_lo, col_hi, row_lo, row_hi) = bubble_bounds(b.bx, b.by, b.ry, b.rx);
        for row in row_lo..=row_hi {
            row_occupied.entry(row).or_default().push((col_lo, col_hi));
        }
    }
    let fits = |occ: &std::collections::HashMap<i32, Vec<(i32, i32)>>, row: i32, start: i32, end: i32| {
        occ.get(&row).is_none_or(|ivs| ivs.iter().all(|&(a, b)| end <= a || start >= b))
    };

    const REACH: i32 = 10;

    // Pass 1: labels that fit inside their own bubble, drawn exactly as
    // before. Anything that doesn't fit is deferred to pass 2.
    let mut overflow: Vec<&LiveBubble> = Vec::new();
    for b in live {
        let state = &states[b.slot];
        let ms_str = fmt_bubble_ms(b.metric_val);
        if label_fits_inside(
            area.width, max_row, b.bx, b.by, b.ry, b.rx,
            state.label.chars().count(), ms_str.chars().count(),
        ) {
            draw_bubble_label(buf, area, b.bx, b.by, b.ry, &state.label, b.metric_val, b.base_rgb);
        } else {
            overflow.push(b);
        }
    }

    // Pass 2: bubbles too small (or too close to the edge) for an in-bubble
    // label get a compact external label instead. Search outward from just
    // above the bubble, then just below it, for the nearest row with enough
    // free horizontal room.
    for b in overflow {
        let state = &states[b.slot];
        let display = match state.label.rfind(" (") {
            Some(pos) if state.label.ends_with(')') => &state.label[..pos],
            _ => state.label.as_str(),
        };
        let ms_str = fmt_bubble_ms(b.metric_val);
        let text = format!(" {} {}", display, ms_str);
        let text_len = text.chars().count() as i32;
        if text_len > area.width as i32 { continue; }

        let anchor_col = b.bx.round() as i32;
        let top_row    = (b.by - b.ry).floor() as i32;
        let bot_row    = (b.by + b.ry).ceil()  as i32;

        let mut candidates: Vec<i32> = Vec::new();
        for d in 0..=REACH { candidates.push(top_row - 1 - d); }
        for d in 0..=REACH { candidates.push(bot_row + 1 + d); }

        let half = text_len / 2;
        let mut placed: Option<(i32, i32)> = None; // (row, start_col)
        for &row in &candidates {
            if row < 0 || row >= max_row { continue; }
            let start = (anchor_col - half).clamp(0, (area.width as i32 - text_len).max(0));
            let end = start + text_len;
            if end > area.width as i32 { continue; }
            if fits(&row_occupied, row, start, end) {
                placed = Some((row, start));
                break;
            }
        }
        let Some((row, start)) = placed else { continue };
        row_occupied.entry(row).or_default().push((start, start + text_len));

        // Leader: dotted vertical line from the bubble edge to the label row.
        let (lr, lg, lb) = lerp_rgb(b.base_rgb.0, b.base_rgb.1, b.base_rgb.2, 0, 0, 0, 0.45);
        let leader_style = Style::default().fg(Color::Rgb(lr, lg, lb)).bg(Color::Black);
        let (lo, hi) = if row < top_row { (row + 1, top_row - 1) } else { (bot_row + 1, row - 1) };
        if lo <= hi && anchor_col >= 0 && anchor_col < area.width as i32 {
            for r in lo..=hi {
                if r < 0 || r >= area.height as i32 { continue; }
                let cell = &mut buf[(area.x + anchor_col as u16, area.y + r as u16)];
                if cell.symbol() == " " {
                    cell.set_symbol(leader_ch);
                    cell.set_style(leader_style);
                }
            }
        }

        let text_style = Style::default()
            .fg(Color::Rgb(b.base_rgb.0, b.base_rgb.1, b.base_rgb.2))
            .bg(Color::Black);
        for (i, ch) in text.chars().enumerate() {
            let c = start + i as i32;
            if c < 0 || c >= area.width as i32 { continue; }
            let cell = &mut buf[(area.x + c as u16, area.y + row as u16)];
            cell.set_symbol(&ch.to_string());
            cell.set_style(text_style);
        }
    }
}

// ── public draw entry point ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_bubble(
    frame:             &mut Frame,
    states:            &[TargetState],
    bs:                &BubbleState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, log_fmt, tick, dialog, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    {
        let (min_w, min_h) = super::min_size(
            "bubble", states.len(), show_col_keys, show_headers, col_widths, mode_labels,
            args.column_vis.mode,
        );
        if area.width < min_w || area.height < min_h {
            super::layout::draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let rows_per_target: u16 = if show_headers { 1 } else { 0 };
    let n = states.len() as u16;
    let show_legend = states.len() > 1;

    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    for _ in 0..(n * rows_per_target) {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if show_legend { constraints.push(Constraint::Length(1)); }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset  = if show_col_keys { 2usize } else { 0 };
    let header_rows      = (n * rows_per_target) as usize;
    let bubble_chunk_idx = col_keys_offset + header_rows;
    let legend_chunk_idx = col_keys_offset + header_rows + 1;

    // Black background for the whole view
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let col_key_areas = if show_col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    if show_legend {
        let default_order: Vec<usize> = (0..states.len()).collect();
        let legend_line = build_legend_line(states, args, &default_order, chunks[legend_chunk_idx].width as usize);
        frame.render_widget(Paragraph::new(legend_line), chunks[legend_chunk_idx]);
    }

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);

    // ── bubble rendering ──────────────────────────────────────────────────────
    let bubble_area = chunks[bubble_chunk_idx];
    if bubble_area.width == 0 || bubble_area.height == 0 { return; }

    let global_max_metric = states.iter()
        .filter(|s| !s.waiting && !bubble_is_down(s))
        .map(|s| bubble_metric(s, sort_mode))
        .fold(0.0f64, f64::max)
        .max(1.0);

    let drop_rgb = {
        let c = args.theme.drop_color;
        if let Color::Rgb(r, g, b) = c { (r, g, b) } else { (255, 60, 60) }
    };

    let buf = frame.buffer_mut();

    // Render all bubbles — no specific z-ordering needed. Down targets have
    // no meaningful RTT/loss to size or color a bubble by (win_avg reads 0.0,
    // the "best" possible value) and don't float in the physics sim - they
    // get a fixed marker below instead.
    let mut down_slots: Vec<usize> = Vec::new();
    let mut live: Vec<LiveBubble> = Vec::new();
    for (i, bubble) in bs.bubbles.iter().enumerate() {
        if i >= states.len() { break; }
        let state = &states[i];
        if !state.waiting && bubble_is_down(state) {
            down_slots.push(i);
            continue;
        }

        let metric_val = if state.waiting { 0.0 } else { bubble_metric(state, sort_mode) };
        let norm = (metric_val / global_max_metric).clamp(0.0, 1.0);
        let base_rgb = args.theme.gradient_color(norm);

        draw_oval(
            buf, bubble_area,
            bubble.x, bubble.y,
            bubble.display_ry,
            base_rgb, drop_rgb,
            state.drop_flash,
        );

        if !state.waiting {
            live.push(LiveBubble {
                slot: i,
                bx: bubble.x, by: bubble.y,
                ry: bubble.display_ry, rx: bubble.display_ry * ASPECT,
                base_rgb, metric_val,
            });
        }
    }

    // Rows reserved at the bottom for down-target markers, computed once here
    // so both the label pass (which must not place anything on top of them)
    // and the marker-drawing block below share the same numbers.
    let down_n  = down_slots.len() as u16;
    let down_ry = down_marker_ry(bubble_area.width, bubble_area.height, down_n);
    let down_reserved_h = down_reserved_rows(bubble_area.height, down_ry);
    let labels_max_row  = (bubble_area.height.saturating_sub(down_reserved_h)) as i32;

    draw_bubble_labels(buf, bubble_area, labels_max_row, states, &live, args.ascii);

    // ── Down targets: flatlined markers pinned to the bottom edge ─────────────
    // A dead target has no meaningful RTT/loss to size or color a bubble by
    // (win_avg reads 0.0, the "best" possible value) and doesn't float in the
    // physics sim - it gets a fixed marker instead: a flat braille baseline
    // with a small centre tick, evoking a heart monitor with no pulse. (A
    // static "X" glyph, and a filled dome shape, were both tried first and
    // dropped - too small at these sizes to read as anything but a blob.)
    // The marker's width still scales with `down_marker_ry` so it grows a
    // little on larger viewports, and its color slowly breathes toward
    // drop-red and back - same ~8s/1Hz triangle cadence as the "waiting"
    // breathing text in layout.rs, just applied to color instead of brightness.
    if !down_slots.is_empty() && bubble_area.width > 0 && bubble_area.height > 0 {
        let n  = down_n;
        let ry = down_ry;
        let reserved_h = down_reserved_h;

        if reserved_h > 0 {
            let down_area = Rect {
                x:      bubble_area.x,
                y:      bubble_area.y + bubble_area.height - reserved_h,
                width:  bubble_area.width,
                height: reserved_h,
            };
            let rx     = ry * ASPECT;
            let slot_w = down_area.width as f64 / n as f64;

            let pulse_step: u8 = match tick % 8 {
                0 | 7 => 0,
                1 | 6 => 1,
                2 | 5 => 2,
                _     => 3,
            };
            let pulse_t = pulse_step as f64 / 3.0 * 0.75; // 0.0..0.75 blend toward drop_rgb

            for (idx, &slot) in down_slots.iter().enumerate() {
                let bx = (idx as f64 + 0.5) * slot_w;
                let (cr, cg, cb) = args.theme.target_color(slot);
                let (pr, pg, pb) = lerp_rgb(cr, cg, cb, drop_rgb.0, drop_rgb.1, drop_rgb.2, pulse_t);

                draw_flatline(buf, down_area, bx, rx, (pr, pg, pb));

                let full_label  = format!("{} (down)", states[slot].label);
                let short_label = states[slot].label.clone();
                let label: &str = if full_label.chars().count() as f64 <= slot_w { &full_label } else { &short_label };
                let label_style = Style::default()
                    .fg(Color::Rgb(cr, cg, cb))
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD);

                let cx = bx.round() as i32;
                write_centered(buf, down_area, cx, 0, label, label_style);
            }
        }
    }

    // ── dialog overlay ────────────────────────────────────────────────────────
    let space_hidden = 0u32;
    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(
        frame, area, args, dialog, frozen, sort_mode,
        states.len(), "bubble", tick, !log_fmt.is_empty(),
        space_hidden, no_data, show_col_keys, show_headers,
    );

    if frozen {
        super::dialogs::draw_frozen_notice(frame, area, args.ascii, &args.theme, tick);
    }
}
