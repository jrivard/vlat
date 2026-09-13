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

//! Scatter plot view.
//!
//! Each target is plotted as a labelled dot at (X metric, Y metric).  The
//! metric on each axis is chosen at runtime via the axis-picker dialog ('a'),
//! from the set defined by [`AxisMetric`]; the X axis can additionally be
//! switched to a log1p scale from the same dialog. A short fading trail behind
//! each dot shows its last few sampled positions, giving a sense of motion
//! between frames.

use std::collections::VecDeque;
use std::time::Duration;
use crate::time::Instant;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::Block,
};
use crate::state::TargetState;
use super::ViewCtx;
use super::{
    compute_no_data, lerp_rgb,
    DialogMode,
    dialogs::draw_axis_picker_dialog,
    layout::{draw_too_small, render_dialogs, render_window_label},
};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Columns reserved for the Y-axis label strip + border char (e.g. "100%│").
pub const Y_AXIS_W: u16 = 5;
/// Rows reserved for the X-axis rule line + the tick-label row below it.
pub const X_AXIS_H: u16 = 2;

// ── Axis definitions ────────────────────────────────────────────────────────
// Any of these can be plotted on either axis; the picker dialog ('a') lets the
// user swap them at runtime.  Add a new metric here and it's automatically
// available on both axes - nothing else needs to change. The same enum backs
// the worm view's single-metric picker (see `worm.rs`), where the chosen
// metric drives worm speed/length instead of screen position.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AxisMetric {
    Avg,
    Jitter,
    StdDev,
    Median,
    P95,
    P99,
    Loss,
    Cv,
    Mtr,
}

impl AxisMetric {
    pub const ALL: [AxisMetric; 9] = [
        AxisMetric::Avg, AxisMetric::Jitter, AxisMetric::StdDev, AxisMetric::Median,
        AxisMetric::P95, AxisMetric::P99, AxisMetric::Loss, AxisMetric::Cv, AxisMetric::Mtr,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AxisMetric::Avg    => "avg RTT",
            AxisMetric::Jitter => "jitter",
            AxisMetric::StdDev => "stddev",
            AxisMetric::Median => "median",
            AxisMetric::P95    => "p95",
            AxisMetric::P99    => "p99",
            AxisMetric::Loss   => "loss",
            AxisMetric::Cv     => "cv",
            AxisMetric::Mtr    => "mtr",
        }
    }

    pub fn unit(self) -> &'static str {
        if self.is_percent() { "%" } else { "ms" }
    }

    pub fn is_percent(self) -> bool {
        matches!(self, AxisMetric::Loss | AxisMetric::Cv)
    }

    pub fn value(self, state: &TargetState, is_window: bool) -> f64 {
        match self {
            AxisMetric::Avg    => if is_window { state.win_avg() }        else { state.avg_latency() },
            AxisMetric::Jitter => if is_window { state.win_jitter_avg() } else { state.avg_jitter() },
            AxisMetric::StdDev => if is_window { state.win_stddev() }     else { state.life_stddev() },
            AxisMetric::Median => if is_window { state.win_median() }     else { state.life_median() },
            AxisMetric::P95    => if is_window { state.win_p95() }        else { state.life_p95() },
            AxisMetric::P99    => if is_window { state.win_p99() }        else { state.life_p99() },
            AxisMetric::Loss   => if is_window { state.win_loss_pct() }   else { state.life_loss_pct() },
            AxisMetric::Cv     => if is_window { state.win_cv() }         else { state.life_cv() },
            AxisMetric::Mtr    => (if is_window { state.win_mtr() } else { state.life_mtr() }).unwrap_or(0.0),
        }
    }
}

// ── State ─────────────────────────────────────────────────────────────────────

/// How many trailing samples to keep per target for the motion trail.
const TRAIL_LEN: usize = 4;
/// Minimum spacing between trail samples - keeps the trail readable instead of
/// jittering every frame.
const TRAIL_SAMPLE_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct ScatterState {
    pub x_axis: AxisMetric,
    pub y_axis: AxisMetric,
    /// Log1p-scale the X axis instead of linear. Toggled from the axis picker.
    pub log_x:  bool,
    /// Per-target ring buffer of recent (x, y) samples, oldest first, used to
    /// draw a short fading motion trail behind each dot.
    history:     Vec<VecDeque<(f64, f64)>>,
    last_sample: Instant,
}

impl ScatterState {
    pub fn new() -> Self {
        Self {
            x_axis:      AxisMetric::Avg,
            y_axis:      AxisMetric::Loss,
            log_x:       false,
            history:     Vec::new(),
            // Backdate so the first step() call samples immediately instead of waiting
            // a full interval.
            last_sample: Instant::now().checked_sub(TRAIL_SAMPLE_INTERVAL).unwrap_or_else(Instant::now),
        }
    }

    /// Like `new()`, but starting on the given axes instead of the avg/loss
    /// default - used to seed the view from `--scatter-x` / `--scatter-y`.
    pub fn with_axes(x_axis: AxisMetric, y_axis: AxisMetric) -> Self {
        Self { x_axis, y_axis, ..Self::new() }
    }

    pub fn set_x_axis(&mut self, m: AxisMetric) {
        if self.x_axis != m { self.x_axis = m; self.history.clear(); }
    }

    pub fn set_y_axis(&mut self, m: AxisMetric) {
        if self.y_axis != m { self.y_axis = m; self.history.clear(); }
    }

    pub fn toggle_log_x(&mut self) { self.log_x = !self.log_x; }

    /// Samples the current axis values for each active target, at most every
    /// `TRAIL_SAMPLE_INTERVAL`. Called from the fast ticker while the scatter
    /// view is active.
    pub fn step(&mut self, states: &[TargetState], is_window: bool) {
        if self.history.len() != states.len() { self.history.resize(states.len(), VecDeque::new()); }
        if self.last_sample.elapsed() < TRAIL_SAMPLE_INTERVAL { return; }
        self.last_sample = Instant::now();
        for (i, state) in states.iter().enumerate() {
            if state.waiting || scatter_is_down(state, is_window) { continue; }
            let x = self.x_axis.value(state, is_window);
            let y = self.y_axis.value(state, is_window);
            let h = &mut self.history[i];
            h.push_back((x, y));
            if h.len() > TRAIL_LEN { h.pop_front(); }
        }
    }
}

/// True when a target has never received a successful ping in the relevant
/// window (all recent/lifetime probes were drops) - its RTT is undefined, so
/// it can't be placed meaningfully on either axis. Mirrors the "all_dropping"
/// check used by the radar/worm views. This is independent of which metrics
/// are currently plotted - a target with no samples has no meaningful value
/// for any of them.
fn scatter_is_down(state: &TargetState, is_window: bool) -> bool {
    if is_window {
        state.window.is_empty() && state.win_drops > 0
    } else {
        state.lifetime_rtts.is_empty() && state.drops > 0
    }
}

// ── Scale helpers ─────────────────────────────────────────────────────────────

/// Percent-axis ceiling: smallest "nice" value that accommodates `max_pct`.
fn pct_scale_max(max_pct: f64) -> f64 {
    if      max_pct <=  5.0  {  5.0 }
    else if max_pct <= 10.0  { 10.0 }
    else if max_pct <= 25.0  { 25.0 }
    else if max_pct <= 50.0  { 50.0 }
    else                     { 100.0 }
}

/// Axis ceiling appropriate to the metric's unit: "nice" percent buckets for
/// %-based metrics, "nice" ms buckets (shared with the graph/bars views) for
/// everything else. Also used by the radar view to scale its single picked
/// metric against the reference rings.
pub(crate) fn axis_scale_max(max_val: f64, metric: AxisMetric) -> f64 {
    if metric.is_percent() { pct_scale_max(max_val) } else { crate::state::scale_bucket(max_val).max(10.0) }
}

/// Five evenly-spaced ticks from 0 to `max`, for a linear axis.
fn axis_ticks(max: f64) -> [f64; 5] {
    [0.0, max * 0.25, max * 0.5, max * 0.75, max]
}

/// Five ticks evenly spaced in log1p-space from 0 to `max`, matching the dot
/// placement formula in [`norm_log`] so tick marks line up with the data.
fn axis_ticks_log(max: f64) -> [f64; 5] {
    if max <= 0.0 { return [0.0; 5]; }
    let l = (max + 1.0).ln();
    [0.0, 0.25, 0.5, 0.75, 1.0].map(|f| (l * f).exp() - 1.0)
}

/// Normalized [0,1] position of `v` on a log1p scale capped at `max`.
fn norm_log(v: f64, max: f64) -> f64 {
    if max <= 0.0 { return 0.0; }
    ((v.max(0.0) + 1.0).ln() / (max + 1.0).ln()).clamp(0.0, 1.0)
}

/// Tick-label value; 0 → "0" (not "~"), otherwise no decimals.
fn fmt_axis_tick(v: f64) -> String {
    if v <= 0.0 { "0".to_string() } else { format!("{:.0}", v) }
}

// ── Draw ──────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_scatter(
    frame:             &mut Frame,
    states:            &[TargetState],
    ss:                &ScatterState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, log_fmt, tick, dialog, sort_order, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    {
        let (min_w, min_h) = super::min_size(
            "scatter", states.len(), show_col_keys, show_headers,
            col_widths, mode_labels, args.column_vis.mode,
        );
        if area.width < min_w || area.height < min_h {
            draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    // Black background for the whole view.
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    // ── Vertical layout: [col-keys?] [n × header rows?] [plot area] ──────────
    let rows_per_target = if show_headers { 1u16 } else { 0 };
    let n = states.len() as u16;

    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    for _ in 0..(n * rows_per_target) {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset = if show_col_keys { 2usize } else { 0 };
    let plot_idx        = col_keys_offset + (n * rows_per_target) as usize;

    let col_key_areas = if show_col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);

    // ── Plot area ─────────────────────────────────────────────────────────────
    let plot_area = chunks[plot_idx];
    if plot_area.width <= Y_AXIS_W || plot_area.height <= X_AXIS_H + 1 { return; }

    // Horizontal split: Y-axis strip | inner area.
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(Y_AXIS_W), Constraint::Min(1)])
        .split(plot_area);
    let (y_strip, inner) = (h_chunks[0], h_chunks[1]);

    // Vertical split of inner: data canvas | x-rule | x-labels.
    let data_h = inner.height.saturating_sub(X_AXIS_H);
    if data_h == 0 { return; }
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(data_h), Constraint::Length(1), Constraint::Length(1)])
        .split(inner);
    let (data_area, rule_area, xlabel_area) = (v_chunks[0], v_chunks[1], v_chunks[2]);

    // ── Compute data ranges ───────────────────────────────────────────────────
    // Targets with no successful pings at all in the relevant window have no
    // meaningful value on either axis; they're set aside as "down" and drawn
    // along the bottom edge instead (see `down_slots` below).
    let is_win = args.is_window();
    let mut active: Vec<(usize, f64, f64)> = Vec::new();
    let mut down_slots: Vec<usize> = Vec::new();
    for &s in sort_order {
        if states[s].waiting { continue; }
        if scatter_is_down(&states[s], is_win) {
            down_slots.push(s);
        } else {
            active.push((s, ss.x_axis.value(&states[s], is_win), ss.y_axis.value(&states[s], is_win)));
        }
    }

    let max_x = active.iter().map(|(_, x, _)| *x).fold(0.0f64, f64::max);
    let max_y = active.iter().map(|(_, _, y)| *y).fold(0.0f64, f64::max);
    let x_range = axis_scale_max(max_x, ss.x_axis);
    let y_range = axis_scale_max(max_y, ss.y_axis);

    // ── Draw axes and dots (via direct buffer access) ─────────────────────────
    let (vert_ch, horiz_ch, corner_ch, tick_y_ch, tick_x_ch) = if args.ascii {
        ("|", "-", "+", "+", "+")
    } else {
        ("│", "─", "└", "┤", "┬")
    };
    let dot_ch   = if args.ascii { "o" } else { "●" };
    let trail_ch = if args.ascii { "." } else { "\u{00b7}" };
    // Half-bubble for "down" targets: flat edge resting on the bottom row.
    let bubble_ch = if args.ascii { "n" } else { "◠" };
    // Leader-line connector for labels nudged off the dot's own row.
    let leader_ch = if args.ascii { ":" } else { "┊" };

    let axis_col  = y_strip.x + y_strip.width.saturating_sub(1);
    let data_h_f  = data_h.saturating_sub(1) as f64;
    let data_w_f  = data_area.width.saturating_sub(1) as f64;

    let map_x = |x_val: f64| -> f64 {
        if ss.log_x { norm_log(x_val, x_range) } else { (x_val / x_range).clamp(0.0, 1.0) }
    };

    {
        let buf = frame.buffer_mut();

        // Y-axis vertical line: top of data area to (not including) the corner.
        for row in 0..data_h {
            let cell = &mut buf[(axis_col, plot_area.y + row)];
            cell.set_symbol(vert_ch);
            cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));
        }
        // Corner at the axis junction.
        {
            let cell = &mut buf[(axis_col, rule_area.y)];
            cell.set_symbol(corner_ch);
            cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));
        }

        // X-axis horizontal rule (inner width, not overlapping the corner).
        for col in 0..inner.width {
            let sc = inner.x + col;
            let cell = &mut buf[(sc, rule_area.y)];
            if cell.symbol() != corner_ch {
                cell.set_symbol(horiz_ch);
                cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));
            }
        }

        // Y-axis tick labels: right-aligned into y_strip, tick mark on axis line.
        for &tv in &axis_ticks(y_range) {
            let norm = (tv / y_range).clamp(0.0, 1.0);
            // Low value at bottom → inverted row (high norm = top of canvas).
            let row_in_data = ((1.0 - norm) * data_h_f).round() as u16;
            let sr = plot_area.y + row_in_data;

            // Tick mark on the axis border.
            let cell = &mut buf[(axis_col, sr)];
            cell.set_symbol(tick_y_ch);
            cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));

            // Right-aligned label before the axis border.
            let label = format!("{}{}", fmt_axis_tick(tv), ss.y_axis.unit());
            let label_end = axis_col;
            let label_start = label_end.saturating_sub(label.len() as u16);
            for (i, ch) in label.chars().enumerate() {
                let sc = label_start + i as u16;
                if sc >= label_end { break; }
                let cell = &mut buf[(sc, sr)];
                cell.set_symbol(&ch.to_string());
                cell.set_style(Style::default().fg(Color::Gray).bg(Color::Black));
            }
        }

        // X-axis tick marks + labels.
        let x_tick_vals = if ss.log_x { axis_ticks_log(x_range) } else { axis_ticks(x_range) };
        for &tv in &x_tick_vals {
            if data_area.width == 0 { break; }
            let norm = map_x(tv);
            let col_in_data = (norm * data_w_f).round() as u16;
            let sc = data_area.x + col_in_data;
            if sc >= rule_area.x + rule_area.width { continue; }

            // Tick mark on the rule (don't overwrite the corner).
            {
                let cell = &mut buf[(sc, rule_area.y)];
                if cell.symbol() != corner_ch {
                    cell.set_symbol(tick_x_ch);
                    cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));
                }
            }

            // Label centred below the tick, clamped to bounds.
            let label = format!("{}{}", fmt_axis_tick(tv), ss.x_axis.unit());
            let lw = label.len() as u16;
            let label_start = sc.saturating_sub(lw / 2);
            for (i, ch) in label.chars().enumerate() {
                let c = label_start + i as u16;
                if c < xlabel_area.x || c >= xlabel_area.x + xlabel_area.width { continue; }
                let cell = &mut buf[(c, xlabel_area.y)];
                if cell.symbol() == " " {
                    cell.set_symbol(&ch.to_string());
                    cell.set_style(Style::default().fg(Color::Gray).bg(Color::Black));
                }
            }
        }

        // Axis title in the label row (right edge, after tick labels).
        let log_suffix = if ss.log_x { " [log]" } else { "" };
        let x_title = format!("{} ({}){} \u{2192}", ss.x_axis.label(), ss.x_axis.unit(), log_suffix);
        if xlabel_area.width as usize > x_title.len() + 2 {
            let ts = xlabel_area.x + xlabel_area.width - x_title.len() as u16;
            for (i, ch) in x_title.chars().enumerate() {
                let c = ts + i as u16;
                if c >= xlabel_area.x + xlabel_area.width { break; }
                let cell = &mut buf[(c, xlabel_area.y)];
                if cell.symbol() == " " {
                    cell.set_symbol(&ch.to_string());
                    cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));
                }
            }
        }

        // ── Trails, dots and labels ────────────────────────────────────────────
        if data_area.width > 0 && data_area.height > 0 {
            // When there are down targets, keep the bottom row exclusively
            // theirs so a legitimate 0-value dot can't land on top of a
            // down-bubble and garble both labels.
            let reserve_bottom = !down_slots.is_empty() && data_h > 1;
            let plot_h_f   = if reserve_bottom { (data_h - 2) as f64 } else { data_h_f };
            let plot_max_row = if reserve_bottom { data_area.height - 2 } else { data_area.height - 1 };

            let map_cell = |x_val: f64, y_val: f64| -> (u16, u16) {
                let norm_x  = map_x(x_val);
                let norm_y  = (y_val / y_range).clamp(0.0, 1.0);
                let dot_col = (norm_x * data_w_f).round() as u16;
                // Low value at bottom → invert row.
                let dot_row = ((1.0 - norm_y) * plot_h_f).round() as u16;
                let sc = data_area.x + dot_col.min(data_area.width  - 1);
                let sr = data_area.y + dot_row.min(plot_max_row);
                (sc, sr)
            };

            // Motion trail: faint fading dots behind each target's current
            // position, oldest = dimmest. Drawn first so the main dot/label
            // pass below always wins on overlap.
            for &(slot, _, _) in &active {
                let Some(hist) = ss.history.get(slot) else { continue };
                let n_hist = hist.len();
                if n_hist == 0 { continue; }
                let (cr, cg, cb) = args.theme.target_color(slot);
                for (j, &(hx, hy)) in hist.iter().enumerate() {
                    let t = if n_hist > 1 { j as f64 / (n_hist - 1) as f64 } else { 1.0 };
                    let dim = 0.78 - 0.55 * t; // oldest ~0.78 toward black, newest ~0.23
                    let (tr, tg, tb) = lerp_rgb(cr, cg, cb, 0, 0, 0, dim);
                    let (sc, sr) = map_cell(hx, hy);
                    let cell = &mut buf[(sc, sr)];
                    if cell.symbol() == " " {
                        cell.set_symbol(trail_ch);
                        cell.set_style(Style::default().fg(Color::Rgb(tr, tg, tb)).bg(Color::Black));
                    }
                }
            }

            // Map each target to a screen cell, then nudge cells that collide
            // so overlapping values don't fully hide one another. Offsets are
            // chosen by arrival order in `sort_order`, which is stable
            // frame-to-frame, so the spread doesn't jitter/flicker.
            let mut points: Vec<(usize, f64, f64, u16, u16)> = active.iter().map(|&(slot, x_val, y_val)| {
                let (sc, sr) = map_cell(x_val, y_val);
                (slot, x_val, y_val, sc, sr)
            }).collect();

            const OVERLAP_NUDGES: [(i32, i32); 8] =
                [(0, 0), (1, 0), (-1, 0), (0, -1), (0, 1), (1, -1), (-1, 1), (1, 1)];
            let mut seen: std::collections::HashMap<(u16, u16), usize> = std::collections::HashMap::new();
            for p in &mut points {
                let base    = (p.3, p.4);
                let dup_idx = seen.entry(base).or_insert(0);
                let (dc, dr) = OVERLAP_NUDGES[(*dup_idx).min(OVERLAP_NUDGES.len() - 1)];
                *dup_idx += 1;
                p.3 = (base.0 as i32 + dc).clamp(data_area.x as i32, (data_area.x + data_area.width  - 1) as i32) as u16;
                p.4 = (base.1 as i32 + dr).clamp(data_area.y as i32, (data_area.y + plot_max_row) as i32) as u16;
            }

            // Label placement: try the dot's own row first. If the full label
            // (or even the short one) would collide with a neighbour there,
            // search a few rows above/below for room instead of silently
            // truncating - this is what keeps a cluster of same-row dots
            // (e.g. every target tied on loss%) from losing their labels
            // entirely. A dotted "leader line" is drawn at the dot's column
            // connecting it to an offset label so it's still obvious which
            // point owns which label.
            // Search generously outward from the dot's own row: scatter plots
            // are usually far taller than they are crowded horizontally (e.g.
            // a loss-axis cluster where every clean target sits at 0%, with
            // dozens of empty rows above going unused), so there's normally
            // plenty of vertical room to offset a label into.
            const LABEL_ROW_REACH: i32 = 12;
            let mut label_row_search: Vec<i32> = vec![0];
            for d in 1..=LABEL_ROW_REACH { label_row_search.push(-d); label_row_search.push(d); }
            let row_min = data_area.y;
            let row_max = data_area.y + plot_max_row;
            let right_edge = data_area.x + data_area.width;

            // Column intervals already spoken for on each row, keyed by row.
            // Seeded with every dot's own cell so labels never sit on top of
            // an unrelated dot.
            let mut row_occupied: std::collections::HashMap<u16, Vec<(u16, u16)>> = std::collections::HashMap::new();
            for &(_, _, _, sc, sr) in &points {
                row_occupied.entry(sr).or_default().push((sc, sc + 1));
            }
            let fits = |occ: &std::collections::HashMap<u16, Vec<(u16, u16)>>, row: u16, start: u16, end: u16| {
                occ.get(&row).is_none_or(|ivs| ivs.iter().all(|&(a, b)| end <= a || start >= b))
            };

            // Left-to-right so labels pack greedily without leapfrogging.
            let mut label_order: Vec<usize> = (0..points.len()).collect();
            label_order.sort_by_key(|&i| (points[i].3, points[i].4));

            // Resolved (row, text) per point; empty text = no label drawn.
            let mut placed: Vec<(u16, String)> = points.iter().map(|&(_, _, _, _, sr)| (sr, String::new())).collect();

            for &idx in &label_order {
                let (slot, _, _, sc, sr) = points[idx];
                let raw_label = &states[slot].label;
                // Strip the " (ip)" suffix vlat appends when a hostname resolves -
                // the in-plot label only has room for the short name, matching
                // the radar/pong/table views.
                let display = match raw_label.rfind(" (") {
                    Some(pos) if raw_label.ends_with(')') => &raw_label[..pos],
                    _ => raw_label.as_str(),
                };
                let label = format!(" {}", display);

                // Only the destination row's own text budget is checked here -
                // not every row the leader line passes through. Requiring the
                // whole vertical path to be clear too creates dead ends: one
                // wide label parked on an overflow row would then block every
                // other point's path past it, even when the row beyond is
                // wide open. The draw pass below never overwrites an occupied
                // cell, so a connector that happens to cross existing content
                // just leaves a small gap there instead of corrupting it.
                let mut chosen: Option<(u16, String)> = None;
                'search: for &dy in &label_row_search {
                    let row = sr as i32 + dy;
                    if row < row_min as i32 || row > row_max as i32 { continue; }
                    let row = row as u16;
                    let text_start = sc + 1;
                    let text_end   = (text_start + label.len() as u16).min(right_edge);
                    if text_end <= text_start { continue; }
                    let check_start = if dy == 0 { text_start } else { sc };
                    if fits(&row_occupied, row, check_start, text_end) {
                        chosen = Some((row, label.clone()));
                        break 'search;
                    }
                }

                if let Some((row, text)) = chosen {
                    let text_start = sc + 1;
                    let text_end   = (text_start + text.len() as u16).min(right_edge);
                    let reserve_start = if row == sr { text_start } else { sc };
                    row_occupied.entry(row).or_default().push((reserve_start, text_end));
                    placed[idx] = (row, text);
                }
            }

            // Dots and label text are drawn first, in their own pass, so they
            // always win; leader connectors are drawn in a second pass after
            // everything else is down, so a connector crossing someone else's
            // label just leaves a gap there instead of clobbering a letter.
            for &(slot, _, _, sc, sr) in &points {
                let (cr, cg, cb) = args.theme.target_color(slot);
                let color = Color::Rgb(cr, cg, cb);
                let cell = &mut buf[(sc, sr)];
                cell.set_symbol(dot_ch);
                cell.set_style(Style::default().fg(color).bg(Color::Black).add_modifier(Modifier::BOLD));
            }
            for (idx, &(slot, _, _, sc, _)) in points.iter().enumerate() {
                let (label_row, ref label) = placed[idx];
                if label.is_empty() { continue; }
                let (cr, cg, cb) = args.theme.target_color(slot);
                let color = Color::Rgb(cr, cg, cb);
                for (i, ch) in label.chars().enumerate() {
                    let c = sc + 1 + i as u16;
                    if c >= data_area.x + data_area.width { break; }
                    let cell = &mut buf[(c, label_row)];
                    if cell.symbol() == " " || cell.symbol() == trail_ch {
                        cell.set_symbol(&ch.to_string());
                        cell.set_style(Style::default().fg(color).bg(Color::Black));
                    }
                }
            }
            for (idx, &(slot, _, _, sc, sr)) in points.iter().enumerate() {
                let (label_row, ref label) = placed[idx];
                if label.is_empty() || label_row == sr { continue; }
                let (cr, cg, cb) = args.theme.target_color(slot);
                let (lr, lg, lb) = lerp_rgb(cr, cg, cb, 0, 0, 0, 0.45);
                let (lo, hi) = if label_row > sr { (sr + 1, label_row) } else { (label_row, sr - 1) };
                for row in lo..=hi {
                    let cell = &mut buf[(sc, row)];
                    if cell.symbol() == " " || cell.symbol() == trail_ch {
                        cell.set_symbol(leader_ch);
                        cell.set_style(Style::default().fg(Color::Rgb(lr, lg, lb)).bg(Color::Black));
                    }
                }
            }
        }

        // ── Down targets: half-bubbles along the bottom edge ──────────────────
        // These have no successful pings, so no axis value is meaningful -
        // rather than faking a position, they get their own row spread evenly
        // across the width, colored to match their normal target color.
        if !down_slots.is_empty() && data_area.width > 0 && data_area.height > 0 {
            let bottom_row = data_area.y + data_area.height - 1;
            let n = down_slots.len() as u16;
            for (i, &slot) in down_slots.iter().enumerate() {
                let i = i as u16;
                let col_offset = if n <= 1 { 0 } else {
                    (i * (data_area.width - 1)) / n
                };
                let sc = data_area.x + col_offset.min(data_area.width - 1);
                let sr = bottom_row;

                let (cr, cg, cb) = args.theme.target_color(slot);
                let color = Color::Rgb(cr, cg, cb);
                let state = &states[slot];

                {
                    let cell = &mut buf[(sc, sr)];
                    cell.set_symbol(bubble_ch);
                    cell.set_style(Style::default().fg(color).bg(Color::Black).add_modifier(Modifier::BOLD));
                }

                let full_label  = format!(" {} (down)", state.label);
                let short_label = format!(" {}", state.label);
                let space_right = (data_area.x + data_area.width).saturating_sub(sc + 1);
                let label: &str = if space_right as usize >= full_label.len() {
                    &full_label
                } else if space_right as usize >= short_label.len() {
                    &short_label
                } else {
                    ""
                };
                for (i, ch) in label.chars().enumerate() {
                    let c = sc + 1 + i as u16;
                    if c >= data_area.x + data_area.width { break; }
                    let cell = &mut buf[(c, sr)];
                    if cell.symbol() == " " {
                        cell.set_symbol(&ch.to_string());
                        cell.set_style(Style::default().fg(color).bg(Color::Black));
                    }
                }
            }
        }

        // Waiting-for-data placeholder.
        if active.is_empty() && down_slots.is_empty() && data_area.width > 0 && data_area.height > 0 {
            let msg = "waiting for data\u{2026}";
            let msg_len = msg.chars().count() as u16;
            let sc = data_area.x + data_area.width.saturating_sub(msg_len) / 2;
            let sr = data_area.y + data_area.height / 2;
            for (i, ch) in msg.chars().enumerate() {
                let c = sc + i as u16;
                if c >= data_area.x + data_area.width { break; }
                let cell = &mut buf[(c, sr)];
                cell.set_symbol(&ch.to_string());
                cell.set_style(Style::default().fg(Color::DarkGray).bg(Color::Black));
            }
        }
    }

    // Y-axis title in y_strip top-left corner.
    let y_title = format!("{} {}", ss.y_axis.label(), ss.y_axis.unit());
    if y_strip.width as usize >= y_title.len() && y_strip.height > 0 {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::Paragraph;
        let line = Line::from(Span::styled(
            y_title,
            Style::default().fg(Color::DarkGray).bg(Color::Black),
        ));
        frame.render_widget(Paragraph::new(line), ratatui::layout::Rect {
            x: y_strip.x,
            y: y_strip.y,
            width: y_strip.width.saturating_sub(1),
            height: 1,
        });
    }

    // ── Dialog overlay ────────────────────────────────────────────────────────
    // The axis picker needs live ScatterState (x_axis/y_axis/log_x), which the
    // shared render_dialogs() helper doesn't carry - draw it directly here and
    // fall through to the shared path for every other dialog.
    if let DialogMode::AxisPicker { cursor, field } = dialog {
        draw_axis_picker_dialog(frame, area, args.ascii, &args.theme, *cursor, *field, ss.x_axis, ss.y_axis, ss.log_x);
    } else {
        let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
        render_dialogs(
            frame, area, args, dialog, frozen, sort_mode,
            states.len(), "scatter", tick, !log_fmt.is_empty(),
            0u32, no_data, show_col_keys, show_headers,
        );
    }

    if frozen {
        super::dialogs::draw_frozen_notice(frame, area, args.ascii, &args.theme, tick);
    }
}
