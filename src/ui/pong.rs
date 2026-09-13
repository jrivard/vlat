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

//! Pong graph mode.
//!
//! HIDDEN (WIP): intentionally left out of the `v` view picker and the
//! digit-jump hotkeys (see `VIEW_DISPLAY_ORDER` / `VIEW_PICKER_ORDER` in
//! `ui/dialogs.rs`) while it's rough around the edges. The view itself is
//! fully implemented and still reachable directly via `--view pong` (kept
//! out of `--help` via `#[value(hide = true)]` in `cli.rs`) - do not delete
//! this module or its wiring in `app.rs`; re-add it to the picker/hotkey
//! order arrays once it's polished.
//!
//! All targets share one field - no lane subdivision.  Each ball bounces
//! left↔right and top↔bottom.  The period of one full horizontal bounce
//! cycle tracks RTT relative to the shared scale (the same scale used for
//! the RTT color gradient) - faster ball = lower latency relative to the
//! rest of the field.  The bounce angle tracks jitter - a stable target
//! moves nearly flat, a jittery one bounces steeply.  The row a ball
//! gravitates toward tracks its average RTT relative to the shared scale -
//! consistently slower targets sink toward the bottom of the field.
//! A fading trail shows recent trajectory.  Drops flash a red ×.

use std::collections::VecDeque;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::Block,
};
use crate::state::TargetState;
use super::ViewCtx;
use super::{
    compute_no_data,
    layout::{render_window_label, render_dialogs, draw_too_small},
};

// ── constants ─────────────────────────────────────────────────────────────────

const TRAIL_LEN:     usize = 28;
const BRICK_W:       u16   = 3;
const MAX_TAIL_DIST: f64   = 120.0; // chars of path length rendered as tail

// At the reference RTT the brick completes one round trip in this many 200ms
// ticks (= ~2.4 seconds).  Used as a fallback before the shared scale is known
// (e.g. the very first frame, before any probe has landed).
const REF_RTT_MS:     f64 = 50.0;
const REF_PERIOD_TICKS: f64 = 120.0;

// Once the shared scale (same value driving the RTT color gradient) is known,
// speed is calculated relative to it instead of the fixed REF_RTT_MS - this
// fraction of shared_scale becomes the "reference" RTT for one bounce cycle,
// so the field stays visually well-spread whether everything shown is a
// sub-millisecond LAN hop or a 200ms WAN path.
const SCALE_REF_FRACTION: f64 = 0.5;

// Clamp period so extreme RTTs still look good.
const MIN_PERIOD_TICKS: f64 = 40.0;   // fastest: ~8 s
const MAX_PERIOD_TICKS: f64 = 800.0;  // slowest: ~160 s

// Bounce angle (rows-per-col ratio) at zero and maximum jitter.
const MIN_VY_RATIO: f64 = 0.03; // stable target: nearly flat trajectory
const MAX_VY_RATIO: f64 = 0.45; // jittery target: steep bounce

// Fraction of the field height pulled per tick toward the RTT-driven baseline
// row - soft enough that the angle-driven wobble still dominates the visible
// motion, but consistently slower targets drift toward the bottom over time.
const GRAVITY_STRENGTH: f64 = 0.003;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Brightness of a tail position at `dist` chars behind the head.
fn tail_brightness(dist: f64) -> f64 {
    match dist as u32 {
        0..=2   => 1.00,
        3..=8   => 0.80,
        9..=18  => 0.55,
        19..=30 => 0.35,
        31..=42 => 0.22,
        43..=55 => 0.13,
        56..=70 => 0.07,
        _       => 0.0,
    }
}

// Braille fill chars - both columns, growing from the bottom of the cell.
// fill_level 0-3 maps to how far the line has descended into the row (quarter steps).
//   0 = top quarter  → just the two bottom dots     ⣀
//   1 = upper-mid    → bottom half                  ⣤
//   2 = lower-mid    → bottom three-quarters        ⣶
//   3 = bottom qtr   → full cell                    ⣿
const FILL_BOTTOM: [&str; 4] = ["⣀", "⣤", "⣶", "⣿"];

// Complementary fill from the TOP of the cell above - three levels + None.
// fill_level 0 → ⠿ (top 3/4),  1 → ⠛ (top 1/2),  2 → ⠉ (top 1/4),  3 → nothing.
const FILL_TOP: [Option<&str>; 4] = [Some("⠿"), Some("⠛"), Some("⠉"), None];

/// Shared left margin: longest display-label width + 2 gap chars.
/// All balls bounce off this column so their left edges stay aligned.
pub fn pong_left_margin(states: &[TargetState]) -> u16 {
    let max_len = states.iter().map(|s| {
        let raw = s.label.as_str();
        let name = if s.custom_label { raw }
                   else { raw.split_once(" (").map_or(raw, |(h, _)| h) };
        name.chars().count()
    }).max().unwrap_or(0);
    (max_len + 2) as u16
}

/// Ball speed (chars per tick) for the given RTT and lane width.
/// `shared_scale` is the same RTT scale used for the color gradient; when
/// known (> 0) it replaces the fixed REF_RTT_MS reference so speed stays
/// well-spread across whatever RTT range the current targets span.
fn speed_from_rtt(rtt_ms: f64, lane_w: u16, shared_scale: f64) -> f64 {
    let rtt       = rtt_ms.clamp(1.0, 5_000.0);
    let reference = if shared_scale > 0.0 { shared_scale * SCALE_REF_FRACTION } else { REF_RTT_MS };
    let period    = (REF_PERIOD_TICKS * rtt / reference)
        .clamp(MIN_PERIOD_TICKS, MAX_PERIOD_TICKS);
    (lane_w as f64 * 2.0) / period
}

/// Normalize a value against a global max, as `worm`/`bubble` do for jitter.
fn norm_against(value: f64, global_max: f64) -> f64 {
    if global_max <= 0.0 { return 0.0; }
    (value / global_max).clamp(0.0, 1.0)
}

// ── state ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Ball {
    pub x:            f64,  // current x position within play area [0, max_x]
    pub y_frac:       f64,  // vertical position [0.0, 1.0]
    vel_x:            f64,  // chars per tick, sign = direction
    vy_sign:          f64,  // ±1.0 - flips on top/bottom bounce
    vy_ratio:         f64,  // rows moved per col moved - tracks jitter, recomputed per probe
    baseline_y_frac:  f64,  // gravity target row [0.0, 1.0] - tracks avg RTT relative to shared_scale
    pub trail:        VecDeque<(f64, f64)>,
    pub last_rtt:     f64,
    pub drop_flash:   u8,
    #[allow(dead_code)]
    lcg:              u64,
}

impl Ball {
    fn new(lane_w: u16, seed: u64, left_margin: u16) -> Self {
        let mut lcg = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let max_x   = lane_w.saturating_sub(BRICK_W + left_margin) as f64;
        let start_x = (lcg >> 33) as f64 / u32::MAX as f64 * max_x;
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        let dir_x   = if (lcg >> 63) == 0 { 1.0f64 } else { -1.0f64 };
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        let start_y = (lcg >> 33) as f64 / u32::MAX as f64;
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        let vy_sign = if (lcg >> 63) == 0 { 1.0f64 } else { -1.0f64 };
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        // Initial angle, before any jitter data has arrived; overwritten on first probe.
        let vy_ratio = MIN_VY_RATIO + (lcg >> 33) as f64 / u32::MAX as f64 * (MAX_VY_RATIO - MIN_VY_RATIO);
        Ball {
            x:               start_x.clamp(0.0, max_x),
            y_frac:          start_y,
            vel_x:           dir_x * speed_from_rtt(REF_RTT_MS, lane_w, 0.0),
            vy_sign,
            vy_ratio,
            baseline_y_frac: start_y,
            trail:           VecDeque::new(),
            last_rtt:        REF_RTT_MS,
            drop_flash:      0,
            lcg,
        }
    }

    #[allow(dead_code)]
    fn lcg_next(&mut self) -> f64 {
        self.lcg = self.lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.lcg >> 33) as f64 / u32::MAX as f64
    }
}

#[derive(Clone)]
pub struct PongState {
    pub balls:     Vec<Ball>,
    pub n_targets: usize,
    prev_sent:     Vec<u64>,
    prev_drops:    Vec<u64>,
}

impl PongState {
    pub fn new(n_targets: usize, lane_w: u16, left_margin: u16) -> Self {
        PongState {
            balls:      (0..n_targets).map(|i| Ball::new(lane_w, (i as u64 + 1).wrapping_mul(0xDEAD_BEEF_1337_CAFE), left_margin)).collect(),
            n_targets,
            prev_sent:  vec![0u64; n_targets],
            prev_drops: vec![0u64; n_targets],
        }
    }

    /// Called every fast tick.  Advances ball positions and ingests new probe results.
    /// `shared_scale` is the RTT scale backing the color gradient (drives speed and the
    /// height baseline); `global_max_jitter` is the largest windowed jitter across all
    /// displayed targets (drives bounce angle), same as `worm`/`bubble` use it.
    pub fn step(&mut self, states: &[TargetState], shared_scale: f64, global_max_jitter: f64, lane_w: u16, draw_h: u16, left_margin: u16) {
        let n = self.n_targets.min(states.len());
        let max_x = (lane_w.saturating_sub(BRICK_W + left_margin) as f64).max(0.0);

        for (slot, state) in states.iter().enumerate().take(n) {
            let ball = &mut self.balls[slot];

            // ── ingest new probe result ────────────────────────────────────
            if !state.waiting && state.total_sent > self.prev_sent[slot] {
                self.prev_sent[slot] = state.total_sent;

                let cur_drops = state.drops as u64;
                if cur_drops > self.prev_drops[slot] {
                    self.prev_drops[slot] = cur_drops;
                    ball.drop_flash = 10;
                }

                let rtt = if state.last_rtt > 0.0
                    && state.last_rtt != f64::MAX
                    && state.last_rtt != f64::MIN
                {
                    state.last_rtt
                } else {
                    ball.last_rtt
                };
                ball.last_rtt = rtt;

                let recent_total = state.win_drops as usize + state.window.len();
                let drop_rate = if recent_total > 0 {
                    state.win_drops as f64 / recent_total as f64
                } else {
                    0.0
                };
                // At 100% drops: ~1 step per minute (fast tick = 200ms, 300 ticks/min).
                let drop_mult = (1.0 / 300.0) + (1.0 - 1.0 / 300.0) * (1.0 - drop_rate);
                let spd = speed_from_rtt(rtt, lane_w, shared_scale) * drop_mult;
                ball.vel_x = ball.vel_x.signum() * spd;

                // Angle tracks jitter - stable target bounces nearly flat, jittery one steeply.
                let jitter_n = norm_against(state.win_jitter_avg(), global_max_jitter);
                ball.vy_ratio = MIN_VY_RATIO + jitter_n * (MAX_VY_RATIO - MIN_VY_RATIO);

                // Baseline row tracks avg RTT relative to shared_scale - slower targets
                // gravitate toward the bottom of the field over time.
                let win_avg = if !state.window.is_empty() { state.win_avg() } else { state.avg_latency() };
                ball.baseline_y_frac = norm_against(win_avg, shared_scale);
            }

            // ── advance y - jitter-driven wobble plus a soft pull toward the
            // RTT baseline row, bounce off top/bottom ─────────────────────
            // vy_ratio is rows-per-col; scale by current x-speed and field height
            // so the visual angle stays fixed even as RTT changes vel_x.
            let vy_wobble  = ball.vy_sign * ball.vy_ratio * ball.vel_x.abs() / draw_h.max(1) as f64;
            let vy_gravity = (ball.baseline_y_frac - ball.y_frac) * GRAVITY_STRENGTH;
            ball.y_frac += vy_wobble + vy_gravity;
            if ball.y_frac >= 1.0 {
                ball.y_frac = 2.0 - ball.y_frac;
                ball.vy_sign = -1.0;
            } else if ball.y_frac <= 0.0 {
                ball.y_frac = -ball.y_frac;
                ball.vy_sign = 1.0;
            }

            // ── advance x and bounce ───────────────────────────────────────
            ball.x += ball.vel_x;
            if ball.x >= max_x {
                ball.x    = max_x - (ball.x - max_x).abs();
                ball.vel_x = -ball.vel_x.abs();
            } else if ball.x <= 0.0 {
                ball.x    = ball.x.abs();
                ball.vel_x = ball.vel_x.abs();
            }

            // ── record trail ──────────────────────────────────────────────
            ball.trail.push_back((ball.x, ball.y_frac));
            while ball.trail.len() > TRAIL_LEN {
                ball.trail.pop_front();
            }

            if ball.drop_flash > 0 { ball.drop_flash -= 1; }
        }
    }
}

// ── rendering ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_pong(
    frame:             &mut Frame,
    states:            &[TargetState],
    pong:              &mut PongState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, tick, dialog, sort_order, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    let n    = states.len() as u16;

    {
        let (min_w, min_h) = super::min_size("pong", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let rows_per_target: u16 = if !show_headers { 0 } else { 1 };

    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    constraints.extend((0..(n * rows_per_target)).map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Min(1)); // pong field

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset = if show_col_keys { 2usize } else { 0 };
    let header_rows    = (n * rows_per_target) as usize;
    let pong_chunk_idx = col_keys_offset + header_rows;

    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let col_key_areas = if show_col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);

    let pong_area = chunks[pong_chunk_idx];
    let field_w   = pong_area.width;
    let field_h   = pong_area.height;

    if field_w == 0 || field_h == 0 { return; }

    let n_targets = states.len();
    if n_targets == 0 { return; }

    // Shared left margin: wide enough for the longest label across all targets.
    let left_margin = pong_left_margin(states);

    // All balls share one field - no lane subdivision.
    let draw_h   = field_h;
    let field_top = pong_area.y;

    let buf = frame.buffer_mut();

    for &slot in sort_order.iter() {
        if slot >= n_targets { break; }

        let (tr, tg, tb) = args.theme.target_color(slot);
        let ball         = &pong.balls[slot];

        // ── label - tracks the ball's current y row ───────────────────────
        let raw_label = &states[slot].label;
        let label = if states[slot].custom_label {
            raw_label.as_str()
        } else {
            raw_label.split_once(" (").map_or(raw_label.as_str(), |(host, _)| host)
        };
        let label_row = (ball.y_frac * (draw_h as f64 - 1.0)).round() as u16;
        let cy_label  = (field_top + label_row).min(field_top + draw_h.saturating_sub(1));
        let label_dim = 1.0f64;
        for (i, ch) in label.chars().enumerate() {
            let lx = pong_area.x + i as u16;
            if lx >= pong_area.x + field_w { break; }
            let cell = &mut buf[(lx, cy_label)];
            let mut s = String::new(); s.push(ch);
            cell.set_symbol(&s);
            cell.set_style(Style::default().fg(Color::Rgb(
                (tr as f64 * label_dim) as u8,
                (tg as f64 * label_dim) as u8,
                (tb as f64 * label_dim) as u8,
            )));
        }

        // ── tail (interpolated, distance-based fading) ────────────────────
        let trail_vec: Vec<(f64, f64)> = ball.trail.iter().cloned().collect();
        let n_trail = trail_vec.len();

        if n_trail >= 2 {
            let mut dist_from_head = vec![0.0f64; n_trail];
            for i in (0..n_trail - 1).rev() {
                let seg_dx = (trail_vec[i + 1].0 - trail_vec[i].0).abs();
                dist_from_head[i] = dist_from_head[i + 1] + seg_dx;
            }

            for seg in 0..n_trail - 1 {
                if dist_from_head[seg] > MAX_TAIL_DIST { continue; }

                let (x0, y0) = trail_vec[seg];
                let (x1, y1) = trail_vec[seg + 1];
                let dx   = x1 - x0;
                let slen = dx.abs();
                if slen < 0.5 { continue; }

                let n_steps = slen.ceil() as usize;
                for step in 0..n_steps {
                    let t    = step as f64 / slen;
                    let dist = dist_from_head[seg + 1] + (1.0 - t) * slen;
                    if dist > MAX_TAIL_DIST { continue; }

                    let brightness = tail_brightness(dist);
                    if brightness == 0.0 { continue; }

                    let ix = x0 + dx * t;
                    let iy = y0 + (y1 - y0) * t;
                    let cx = pong_area.x + left_margin
                        + (ix.round() as u16).min(field_w.saturating_sub(left_margin + 1));

                    // Sub-pixel row: floor gives the primary cell; frac selects which
                    // quarter of the cell the line is in (0=top, 3=bottom).
                    let exact_row  = iy * (draw_h as f64 - 1.0);
                    let row0       = exact_row.floor() as i32;
                    let frac       = exact_row - row0 as f64;
                    let fill_level = ((frac * 4.0) as usize).min(3);

                    // Primary cell: braille bar growing from the bottom.
                    if row0 >= 0 && (row0 as u16) < draw_h {
                        let cy   = field_top + row0 as u16;
                        let cell = &mut buf[(cx, cy)];
                        cell.set_symbol(FILL_BOTTOM[fill_level]);
                        cell.set_style(Style::default().fg(Color::Rgb(
                            (tr as f64 * brightness) as u8,
                            (tg as f64 * brightness) as u8,
                            (tb as f64 * brightness) as u8,
                        )));
                    }

                    // Secondary cell (one row above): complementary bar from the top,
                    // dimmer - bridges the gap to the previous row so transitions are
                    // smooth instead of abrupt steps.
                    if let Some(top_sym) = FILL_TOP[fill_level] {
                        let row_above = row0 - 1;
                        if row_above >= 0 && (row_above as u16) < draw_h {
                            let cy   = field_top + row_above as u16;
                            let cell = &mut buf[(cx, cy)];
                            cell.set_symbol(top_sym);
                            let dim = 0.55;
                            cell.set_style(Style::default().fg(Color::Rgb(
                                (tr as f64 * brightness * dim) as u8,
                                (tg as f64 * brightness * dim) as u8,
                                (tb as f64 * brightness * dim) as u8,
                            )));
                        }
                    }
                }
            }
        }

        // ── brick head - flat front edge, tapered trailing edge ───────────
        let moving_right = ball.vel_x >= 0.0;
        let brick_chars: [&str; 3] = if moving_right { ["▒", "▓", "█"] } else { ["█", "▓", "▒"] };

        let bx_col  = ball.x.round() as u16;
        let row_off = (ball.y_frac * (draw_h as f64 - 1.0)).round() as u16;
        let by      = field_top + row_off.min(draw_h.saturating_sub(1));

        let norm_rtt = if shared_scale > 0.0 { (ball.last_rtt / shared_scale).clamp(0.0, 1.0) } else { 0.0 };
        let (hr, hg, hb) = args.theme.gradient_color(norm_rtt);
        let blend = 0.65f64;
        let cr = (hr as f64 * blend + tr as f64 * (1.0 - blend)) as u8;
        let cg = (hg as f64 * blend + tg as f64 * (1.0 - blend)) as u8;
        let cb = (hb as f64 * blend + tb as f64 * (1.0 - blend)) as u8;

        for dx in 0..BRICK_W {
            let bx = pong_area.x + left_margin
                + (bx_col + dx).min(field_w.saturating_sub(left_margin + BRICK_W));
            let cell = &mut buf[(bx, by)];
            if ball.drop_flash > 0 {
                cell.set_symbol(if dx == BRICK_W / 2 { "×" } else { "▓" });
                cell.set_style(Style::default().fg(Color::Rgb(220, 50, 50)).add_modifier(Modifier::BOLD));
            } else {
                cell.set_symbol(brick_chars[dx as usize]);
                cell.set_style(Style::default().fg(Color::Rgb(cr, cg, cb)).add_modifier(Modifier::BOLD));
            }
        }
    }

    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(frame, area, args, dialog, frozen, sort_mode, states.len(), "pong", tick, !log_fmt.is_empty(), 0, no_data, show_col_keys, show_headers);
}
