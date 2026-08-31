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

use std::collections::VecDeque;
use std::f64::consts::TAU;
use crate::time::Instant;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use crate::cli::Args;
use crate::state::TargetState;
use super::{AxisMetric, ViewCtx};
use super::{
    compute_no_data,
    dialogs::{DialogMode, draw_metric_picker_dialog},
    layout::{loading_message, render_window_label, render_dialogs},
    scatter::axis_scale_max,
};

// ── constants ─────────────────────────────────────────────────────────────────

// Terminal cell height is ~2× cell width in pixels; correct for this when
// mapping visual (round-circle) coordinates to terminal (col, row) cells.
const ASPECT: f64 = 2.0;

// One full sweep every 16 seconds.
const SWEEP_SPEED: f64 = TAU / 16.0;

// Phosphor trail spans 60° behind the sweep line.
const TRAIL_WIDTH: f64 = TAU / 6.0;

// Leading edge zone: bright green line, first 10° of trail.
const LEAD_WIDTH: f64 = TAU / 36.0;

// Blips fade to invisible after 3 full rotations (72 s at default speed).
const BLIP_FADE_SECS: f64 = 72.0;

const MAX_BLIPS: usize = 8;

const REF_RADII: [f64; 4] = [0.25, 0.50, 0.75, 1.00];

// Reference circle thickness in visual units (not cells).
const REF_THICKNESS: f64 = 0.9;

// Colors as (r, g, b) tuples for easy lerping.
const SWEEP_BRIGHT: (u8, u8, u8) = (30, 240, 90);
const SWEEP_MID:    (u8, u8, u8) = (0, 60, 20);
const SWEEP_FADE:   (u8, u8, u8) = (0, 12, 4);
const REF_COL:      (u8, u8, u8) = (15, 45, 18);

// ── types ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Blip {
    radius_norm: f64,
    created_at:  Instant,
    is_drop:     bool,
}

#[derive(Clone)]
pub struct RadarState {
    pub angle:     f64,               // current sweep angle, 0 = north, clockwise
    blips:         Vec<VecDeque<Blip>>,
    pub target_angles: Vec<f64>,
    last_tick:     Instant,
    /// Metric that drives blip distance from center (farther = higher).
    /// Selected via the in-app metric picker ('a') or --radar-metric;
    /// defaults to avg RTT. Selection applies live - see `set_metric`.
    pub metric:       AxisMetric,
    /// "Nice" ceiling for `metric`, recomputed each `step()` from the
    /// current spread of target values - the same value the reference-ring
    /// labels are drawn against, so the rings stay meaningful for whichever
    /// metric is selected.
    pub metric_scale: f64,
}

impl RadarState {
    pub fn new(n: usize) -> Self {
        Self::with_metric(n, AxisMetric::Avg)
    }

    /// Like `new()`, but starting on the given metric instead of the avg RTT
    /// default - used to seed the view from `--radar-metric`.
    pub fn with_metric(n: usize, metric: AxisMetric) -> Self {
        let n = n.max(1);
        RadarState {
            angle:         0.0,
            blips:         (0..n).map(|_| VecDeque::new()).collect(),
            target_angles: (0..n).map(|i| (i as f64) * TAU / n as f64).collect(),
            last_tick:     Instant::now(),
            metric,
            metric_scale:  0.0,
        }
    }

    pub fn set_metric(&mut self, m: AxisMetric) { self.metric = m; }

    pub fn step(&mut self, states: &[TargetState]) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_tick).as_secs_f64().min(0.5);
        self.last_tick = now;

        let n = states.len().max(1);
        if self.target_angles.len() != n {
            self.target_angles = (0..n).map(|i| (i as f64) * TAU / n as f64).collect();
        }
        while self.blips.len() < n { self.blips.push(VecDeque::new()); }
        self.blips.truncate(n);

        let old_angle = self.angle;
        let delta     = elapsed * SWEEP_SPEED;
        self.angle    = (old_angle + delta).rem_euclid(TAU);
        let new_angle = self.angle;

        let metric   = self.metric;
        let raw_max  = states.iter().filter(|s| !s.waiting)
            .map(|s| metric.value(s, true)).fold(0.0f64, f64::max);
        self.metric_scale = axis_scale_max(raw_max, metric);
        let metric_scale  = self.metric_scale;

        for (i, state) in states.iter().enumerate() {
            if state.waiting { continue; }
            let ta = self.target_angles[i];

            // Did the sweep cross this target's angle during this step?
            let crossed = if delta >= TAU {
                true
            } else if old_angle < new_angle {
                ta >= old_angle && ta < new_angle
            } else {
                // wrapped around 0/2π
                ta >= old_angle || ta < new_angle
            };
            if !crossed { continue; }

            let all_dropping = state.window.is_empty() && state.win_drops > 0;
            let is_drop      = state.last_was_drop || all_dropping;
            let radius_norm  = if is_drop || metric_scale <= 0.0 {
                1.0_f64
            } else {
                (metric.value(state, true) / metric_scale).clamp(0.05, 0.95)
            };

            self.blips[i].push_front(Blip { radius_norm, created_at: now, is_drop });
            while self.blips[i].len() > MAX_BLIPS { self.blips[i].pop_back(); }
        }
    }
}

/// Reference-ring label for a scaled metric value: RTT formatting for
/// ms-based metrics, a plain percentage for loss/cv.
fn fmt_ring_label(v: f64, metric: AxisMetric) -> String {
    if metric.is_percent() { format!("{:.0}%", v) } else { crate::ui::fmt_rtt(v) }
}

// ── coordinate helpers ────────────────────────────────────────────────────────

// Maximum visual radius that fits in the given cell area.
// x_cells = r_visual, y_cells = r_visual / ASPECT.
fn r_visual(w: u16, h: u16) -> f64 {
    (w as f64 / 2.0).min(h as f64) * 0.80
}

fn lerp3(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let f = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t.clamp(0.0, 1.0)).round() as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

// Convert (radius_norm, angle_from_north_cw) → terminal (col, row) offsets from center.
fn polar_to_cell(rv: f64, radius_norm: f64, angle: f64) -> (f64, f64) {
    let east  =  radius_norm * rv * angle.sin();
    let north =  radius_norm * rv * angle.cos();
    (east, north / ASPECT)  // col_offset, row_offset (row increases downward → subtract north)
}

// ── buffer rendering ──────────────────────────────────────────────────────────

fn render_radar_field(
    buf:           &mut ratatui::buffer::Buffer,
    area:          Rect,
    sweep_angle:   f64,
    blips:         &[VecDeque<Blip>],
    target_angles: &[f64],
    args:          &Args,
) {
    if area.width < 4 || area.height < 3 { return; }

    let cx = area.x as f64 + (area.width  as f64 - 1.0) / 2.0;
    let cy = area.y as f64 + (area.height as f64 - 1.0) / 2.0;
    let rv = r_visual(area.width, area.height);

    // ── Pass 1: sweep trail + reference circles ───────────────────────────────
    for row in area.y..area.y + area.height {
        for col in area.x..area.x + area.width {
            let dx    = col as f64 - cx;
            let dy    = row as f64 - cy;
            let yn    = -dy * ASPECT;               // y-north in visual units
            let vdist = (dx * dx + yn * yn).sqrt();

            if vdist > rv + 0.5 { continue; }

            let nr         = vdist / rv;
            let cell_angle = f64::atan2(dx, yn).rem_euclid(TAU);
            let behind     = (sweep_angle - cell_angle).rem_euclid(TAU);

            // Sweep trail (takes priority over reference circles)
            if behind < TRAIL_WIDTH && nr <= 1.01 {
                let t  = 1.0 - behind / TRAIL_WIDTH;
                let t2 = t * t;
                let cell = &mut buf[(col, row)];

                if behind < LEAD_WIDTH {
                    // Bright leading edge: green dot character
                    let lead_t  = 1.0 - behind / LEAD_WIDTH;
                    let lead_t2 = lead_t * lead_t;
                    let (r, g, b) = lerp3(SWEEP_MID, SWEEP_BRIGHT, lead_t2);
                    cell.set_symbol("·");
                    cell.set_style(Style::default()
                        .fg(Color::Rgb(r, g, b))
                        .bg(Color::Black));
                } else {
                    // Fading trail: dim green background only
                    let (r, g, b) = lerp3(SWEEP_FADE, SWEEP_MID, t2 * 0.6);
                    cell.set_symbol(" ");
                    cell.set_style(Style::default().bg(Color::Rgb(r, g, b)));
                }
                continue;
            }

            // Reference circles
            for &ref_r in &REF_RADII {
                if (vdist - ref_r * rv).abs() < REF_THICKNESS / 2.0 {
                    let cell = &mut buf[(col, row)];
                    cell.set_symbol("·");
                    cell.set_style(Style::default()
                        .fg(Color::Rgb(REF_COL.0, REF_COL.1, REF_COL.2))
                        .bg(Color::Black));
                    break;
                }
            }
        }
    }

    // ── Pass 2: blips (oldest first so newest renders on top) ────────────────
    for (i, target_blips) in blips.iter().enumerate() {
        if i >= target_angles.len() { break; }
        let ta         = target_angles[i];
        let (cr, cg, cb) = args.theme.target_color(i);

        for blip in target_blips.iter().rev() {
            let age  = blip.created_at.elapsed().as_secs_f64();
            let fade = (1.0 - age / BLIP_FADE_SECS).max(0.0);
            if fade < 0.03 { continue; }

            let (col_off, row_off) = polar_to_cell(rv, blip.radius_norm, ta);
            let bcol = (cx + col_off).round() as i32;
            let brow = (cy - row_off).round() as i32;

            if bcol < area.x as i32 || bcol >= (area.x + area.width)  as i32 { continue; }
            if brow < area.y as i32 || brow >= (area.y + area.height) as i32 { continue; }

            let f2  = fade * fade;
            let (r, g, b) = if blip.is_drop {
                ((210.0 * f2) as u8, (30.0 * f2) as u8, (30.0 * f2) as u8)
            } else {
                ((cr as f64 * f2) as u8, (cg as f64 * f2) as u8, (cb as f64 * f2) as u8)
            };

            let cell = &mut buf[(bcol as u16, brow as u16)];
            cell.set_symbol("●");
            cell.set_style(Style::default()
                .fg(Color::Rgb(r, g, b))
                .bg(Color::Black));
        }
    }
}

// ── public draw entry point ───────────────────────────────────────────────────

pub fn draw_radar(
    frame:        &mut Frame,
    states:       &[TargetState],
    radar:        &RadarState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, log_fmt, tick, dialog, sort_order, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    let n    = states.len() as u16;
    {
        let (min_w, min_h) = super::min_size("radar", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            super::layout::draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let rows_per_target: u16 = if !show_headers { 0 } else { 1 };

    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    for _ in 0..(n * rows_per_target) {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));      // radar area

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset = if show_col_keys { 2usize } else { 0 };
    let header_rows     = (n * rows_per_target) as usize;
    let radar_chunk_idx = col_keys_offset + header_rows;

    // ── black background ──────────────────────────────────────────────────────
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let col_key_areas = if show_col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    // ── radar field ───────────────────────────────────────────────────────────
    let radar_area = chunks[radar_chunk_idx];

    {
        let buf = frame.buffer_mut();
        let any_calibrating = states.iter()
            .any(|s| s.calibrating.map(|(_, u)| u > Instant::now()).unwrap_or(false));
        if !any_calibrating {
            render_radar_field(buf, radar_area, radar.angle, &radar.blips, &radar.target_angles, args);
        }
    }

    // ── target labels just outside the radar circle ───────────────────────────
    if radar_area.width >= 8 && radar_area.height >= 4 {
        let cx = radar_area.x as f64 + (radar_area.width  as f64 - 1.0) / 2.0;
        let cy = radar_area.y as f64 + (radar_area.height as f64 - 1.0) / 2.0;
        let rv = r_visual(radar_area.width, radar_area.height);

        for (i, &slot) in sort_order.iter().enumerate() {
            if slot >= states.len() || i >= radar.target_angles.len() { break; }
            let ta    = radar.target_angles[slot];
            let label = &states[slot].label;
            // Strip " (ip)" suffix if present
            let display = if let Some(pos) = label.rfind(" (") {
                if label.ends_with(')') { &label[..pos] } else { label.as_str() }
            } else {
                label.as_str()
            };
            let label_len = display.chars().count() as u16;

            let (col_off, row_off) = polar_to_cell(rv * 1.11, 1.0, ta);
            let anchor_col = (cx + col_off).round() as i32;
            let anchor_row = (cy - row_off).round() as i32;

            // Horizontal alignment based on east/west position
            let col_start = if ta.sin() > 0.15 {
                anchor_col          // east: left-align
            } else if ta.sin() < -0.15 {
                anchor_col - label_len as i32  // west: right-align
            } else {
                anchor_col - label_len as i32 / 2  // north/south: center
            };

            // Bounds check
            let col_start = col_start.clamp(radar_area.x as i32,
                (radar_area.x + radar_area.width) as i32 - label_len as i32);
            let row_clamped = anchor_row.clamp(
                radar_area.y as i32, (radar_area.y + radar_area.height - 1) as i32);
            if col_start < 0 { continue; }

            let (cr, cg, cb) = args.theme.target_color(slot);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    display,
                    Style::default().fg(Color::Rgb(cr, cg, cb)).add_modifier(Modifier::BOLD),
                )),
                Rect::new(col_start as u16, row_clamped as u16, label_len, 1),
            );
        }

        // ── scale labels on reference circles (east of north, small arc) ─────
        if radar.metric_scale > 0.0 {
            let scale_angle = TAU / 18.0;   // 20° from north
            for &ref_r in &REF_RADII {
                let v     = ref_r * radar.metric_scale;
                let label = fmt_ring_label(v, radar.metric);
                let llen  = label.len() as u16;
                let (col_off, row_off) = polar_to_cell(rv * ref_r + 1.2, 1.0, scale_angle);
                let lcol = (cx + col_off).round() as i32;
                let lrow = (cy - row_off).round() as i32;
                if lcol < radar_area.x as i32 || lrow < radar_area.y as i32 { continue; }
                if lcol + llen as i32 > (radar_area.x + radar_area.width) as i32 { continue; }
                if lrow >= (radar_area.y + radar_area.height) as i32 { continue; }
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        label,
                        Style::default().fg(Color::Rgb(REF_COL.0, REF_COL.1, REF_COL.2)),
                    )),
                    Rect::new(lcol as u16, lrow as u16, llen, 1),
                );
            }
        }
    }

    // ── calibrating overlay (same as worm) ────────────────────────────────────
    {
        let now = Instant::now();
        if let Some((start, until)) = states.iter()
            .filter_map(|s| s.calibrating)
            .max_by_key(|&(_, u)| u)
        {
            if until > now {
                let br         = 140u8;
                let total_ms   = until.duration_since(start).as_millis().max(1) as f64;
                let elapsed_ms = now.duration_since(start).as_millis() as f64;
                let progress   = (elapsed_ms / total_ms).clamp(0.0, 1.0);
                let secs_left  = until.duration_since(now).as_secs() + 1;

                const BAR_W: usize = 16;
                let filled = (progress * BAR_W as f64).round() as usize;
                let bar: String = "█".repeat(filled) + &"░".repeat(BAR_W - filled);

                let text = format!("{}  {}  {}s", loading_message(), bar, secs_left);
                let tw   = text.chars().count() as u16;
                let ox   = radar_area.x + (radar_area.width.saturating_sub(tw)) / 2;
                let oy   = radar_area.y + radar_area.height / 2;
                let rect = Rect::new(ox, oy, tw.min(radar_area.width), 1);
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        text,
                        Style::default()
                            .fg(Color::Rgb(br, br, br))
                            .add_modifier(Modifier::ITALIC),
                    ))),
                    rect,
                );
            }
        }
    }

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);

    // ── Dialog overlay ────────────────────────────────────────────────────────
    // The metric picker needs live RadarState (metric), which the shared
    // render_dialogs() helper doesn't carry - draw it directly here and fall
    // through to the shared path for every other dialog.
    if let DialogMode::MetricPicker { cursor } = dialog {
        draw_metric_picker_dialog(frame, area, args.ascii, &args.theme, *cursor, "radar");
    } else {
        let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
        render_dialogs(frame, area, args, dialog, frozen, sort_mode, states.len(), "radar", tick, !log_fmt.is_empty(), 0, no_data, show_col_keys, show_headers);
    }

    if frozen {
        super::dialogs::draw_frozen_notice(frame, area, args.ascii, &args.theme, tick);
    }
}
