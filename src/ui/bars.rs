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

//! Vertical bar chart screensaver.
//!
//! One column per target, filling terminal width.  Bar height = current RTT.
//! Two ghost columns (1-probe-ago, 2-probes-ago) fade left of each bar.
//! A horizontal avg-line overlay marks the rolling average.
//! Footer rows below the chart show the label (bold, target colour) and
//! current RTT + avg.  A Y-axis scale runs down the left edge.

use std::collections::VecDeque;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use crate::cli::Args;
use crate::state::TargetState;
use super::ViewCtx;
use crate::types::Sample;
use super::{
    accent_color,
    compute_no_data,
    fmt_rtt,
    layout::{render_window_label, render_dialogs},
};

// Width of the Y-axis scale strip on the left (4 chars for the label + 1 for │).
const SCALE_W: u16 = 5;
// Footer rows at the bottom of the body area.
const FOOTER_H: u16 = 2;
// Minimum viable column width per target.
const MIN_COL_W: u16 = 6;
// History depth (only 2-3 entries are used for ghosts, but keep more for robustness).
const KEEP: usize = 32;

// Braille fill-from-bottom: 1 row (⣀), 2 rows (⣤), 3 rows (⣶), 4 rows/full (⣿).
// Used for the fractional top cell of bars and ghost trails.
const BRAILLE_FRAC: [&str; 4] = ["⣀", "⣤", "⣶", "⣿"];

type Entry = Option<f64>; // None = drop

/// Per-target probe history used for the ghost-trail rendering.
#[derive(Clone)]
pub struct BarsState {
    pub history:   Vec<VecDeque<Entry>>,
    pub n_targets: usize,
    prev_sent:     Vec<u64>,
    prev_drops:    Vec<u64>,
    backfilled:    bool,
}

impl BarsState {
    pub fn new(n_targets: usize) -> Self {
        BarsState {
            history:    (0..n_targets).map(|_| VecDeque::new()).collect(),
            n_targets,
            prev_sent:  vec![0u64; n_targets],
            prev_drops: vec![0u64; n_targets],
            backfilled: false,
        }
    }

    fn backfill(&mut self, states: &[TargetState]) {
        let n = self.n_targets.min(states.len());
        for (slot, state) in states.iter().enumerate().take(n) {
            if state.waiting { continue; }
            let hist = &state.graph_history;
            let take = hist.len().min(KEEP);
            // push in chronological order (oldest first)
            for sample in hist.iter().rev().take(take).collect::<Vec<_>>().into_iter().rev() {
                let val = match sample { Sample::Hit(v) => Some(*v), _ => None };
                self.history[slot].push_back(val);
            }
            self.prev_sent[slot]  = state.total_sent;
            self.prev_drops[slot] = state.drops as u64;
        }
    }

    fn push(&mut self, states: &[TargetState]) {
        if !self.backfilled {
            self.backfill(states);
            self.backfilled = true;
        }
        let n = self.n_targets.min(states.len());
        for (slot, state) in states.iter().enumerate().take(n) {
            if state.waiting { continue; }
            if state.total_sent == self.prev_sent[slot] { continue; }
            self.prev_sent[slot] = state.total_sent;
            let cur_drops = state.drops as u64;
            let is_drop   = cur_drops > self.prev_drops[slot];
            if is_drop { self.prev_drops[slot] = cur_drops; }
            let entry = if is_drop || state.last_rtt <= 0.0 { None } else { Some(state.last_rtt) };
            self.history[slot].push_back(entry);
            while self.history[slot].len() > KEEP { self.history[slot].pop_front(); }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Map an RTT value to a fractional cell-height within the chart area.
fn rtt_to_h(rtt: f64, scale: f64, chart_h: u16) -> f64 {
    if scale <= 0.0 || rtt <= 0.0 { 0.0 }
    else { (rtt / scale * chart_h as f64).clamp(0.0, chart_h as f64) }
}

/// Multiply an RGB colour by a brightness factor in [0, 1].
fn dim(r: u8, g: u8, b: u8, f: f64) -> Color {
    Color::Rgb((r as f64 * f) as u8, (g as f64 * f) as u8, (b as f64 * f) as u8)
}

// ── buffer-only chart renderer ────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_bars_chart(
    buf:          &mut ratatui::buffer::Buffer,
    body:         Rect,
    states:       &[TargetState],
    bars:         &mut BarsState,
    shared_scale: f64,
    sort_order:   &[usize],
    args:         &Args,
    chart_h:      u16,
    col_w:        u16,
) {
    let n      = states.len();
    let ascii  = args.ascii;
    let clip_x = body.x + body.width;

    if n == 0 || chart_h == 0 || col_w < 2 { return; }

    // ── Y-axis vertical bar ────────────────────────────────────────────────────
    let axis_x  = body.x + SCALE_W - 1;
    let axis_ch = if ascii { "|" } else { "│" };
    let gray    = Style::default().fg(Color::DarkGray);
    for row in body.y..body.y + chart_h {
        let cell = &mut buf[(axis_x, row)];
        cell.set_symbol(axis_ch);
        cell.set_style(gray);
    }

    // ── Y-axis scale labels (right-justified, 4 chars wide) ───────────────────
    if shared_scale > 0.0 {
        let label_w = (SCALE_W - 1) as usize;
        let mut used: std::collections::HashSet<u16> = Default::default();
        for &frac in &[1.0f64, 0.75, 0.5, 0.25] {
            let rfb = ((chart_h as f64 * frac).round() as u16).min(chart_h.saturating_sub(1));
            let row = body.y + chart_h - 1 - rfb;
            if used.contains(&row) { continue; }
            used.insert(row);
            let ms    = shared_scale * frac;
            let label = fmt_rtt(ms);
            let label: String = label.chars().take(label_w).collect();
            let pad   = label_w.saturating_sub(label.len());
            // Write right-justified label
            for (i, ch) in label.chars().enumerate() {
                let cx = body.x + pad as u16 + i as u16;
                if cx >= axis_x { break; }
                let s = ch.to_string();
                let cell = &mut buf[(cx, row)];
                cell.set_symbol(&s);
                cell.set_style(gray);
            }
            // Replace the axis char at this row with a tick mark
            let tick = if ascii { "+" } else { "┤" };
            let cell = &mut buf[(axis_x, row)];
            cell.set_symbol(tick);
            cell.set_style(gray);
        }
    }

    // ── Per-target columns ─────────────────────────────────────────────────────
    bars.push(states);

    let (drop_r, drop_g, drop_b) = args.theme.drop_color_rgb();

    for (disp_idx, &slot) in sort_order.iter().enumerate().take(n) {
        if slot >= states.len() { break; }
        let state = &states[slot];
        let col_x = body.x + SCALE_W + disp_idx as u16 * col_w;
        if col_x >= clip_x { break; }
        let col_right = (col_x + col_w).min(clip_x);

        let (cr, cg, cb) = args.theme.target_color(slot);

        // Ghost count and bar width within the column.
        // Layout (left-to-right): [ghost_oldest][ghost_newer][bar...][gap]
        let n_ghosts: usize = if col_w >= 20 { 4 }
                              else if col_w >= 14 { 3 }
                              else if col_w >= 9  { 2 }
                              else if col_w >= 7  { 1 }
                              else { 0 };
        let bar_w = (col_w as usize).saturating_sub(n_ghosts + 1).max(2);
        let bar_x = col_x + n_ghosts as u16;

        let is_drop    = state.last_was_drop;
        let is_waiting = state.waiting || state.resolve_error.is_some();
        let cur_h   = if is_drop { 0.0 } else { rtt_to_h(state.last_rtt, shared_scale, chart_h) };
        let cur_full = cur_h.floor() as u16;
        let cur_frac = cur_h - cur_h.floor();

        let avg_rtt = if args.is_window() { state.win_avg() } else { state.avg_latency() };
        let avg_rfb = rtt_to_h(avg_rtt, shared_scale, chart_h).round() as u16;
        let p95_rtt = if col_w >= 12 {
            if args.is_window() { state.win_p95() } else { state.life_p95() }
        } else { 0.0 };
        let p95_rfb = if p95_rtt > 0.0 {
            rtt_to_h(p95_rtt, shared_scale, chart_h).round() as u16
        } else { u16::MAX };

        // Ghost probe entries: index 0 = 1-probe-ago, index 1 = 2-probes-ago.
        let hist = &bars.history[slot];
        let ghost_entries: Vec<Option<Entry>> = (1..=n_ghosts)
            .map(|age| hist.iter().rev().nth(age).copied())
            .collect();

        for row in body.y..body.y + chart_h {
            let rfb = body.y + chart_h - 1 - row; // rows_from_bottom (0 = baseline row)

            // ── Ghost columns ──────────────────────────────────────────────────
            // age_idx=0 → leftmost/oldest column, age_idx=n_ghosts-1 → innermost/newest ghost.
            for age_idx in 0..n_ghosts {
                // innermost (age_idx == n_ghosts-1) = 1-probe-ago (ghost_entries[0])
                // outermost (age_idx == 0)          = 2-probes-ago (ghost_entries[1])
                let ghost_age_slot = n_ghosts - 1 - age_idx; // index into ghost_entries
                let Some(Some(entry)) = ghost_entries.get(ghost_age_slot) else { continue };
                let ghost_x = col_x + age_idx as u16;
                if ghost_x >= col_right { continue; }

                let (ghost_h, ghost_drop) = match entry {
                    None    => (0.0f64, true),
                    Some(v) => (rtt_to_h(*v, shared_scale, chart_h), false),
                };
                let ghost_full = ghost_h.floor() as u16;
                let ghost_frac = ghost_h - ghost_h.floor();

                let in_ghost = if ghost_drop {
                    rfb == 0
                } else {
                    rfb < ghost_full || (rfb == ghost_full && ghost_frac > 0.1)
                };
                if !in_ghost { continue; }

                // Gradient: innermost (newest) = brightest, outermost (oldest) = dimmest.
                let alpha = if n_ghosts == 1 {
                    0.55_f64
                } else {
                    let t = age_idx as f64 / (n_ghosts - 1) as f64;
                    0.22_f64 + 0.38_f64 * t
                };
                let (sym, color) = if ghost_drop {
                    let s = if ascii { "x" } else { "⣿" };
                    let c = dim(drop_r, drop_g, drop_b, (alpha * 1.5_f64).min(1.0));
                    (s, c)
                } else {
                    let s = if ascii {
                        if age_idx == n_ghosts - 1 { ":" } else { "." }
                    } else if rfb < ghost_full {
                        "⣿"
                    } else {
                        // fractional top cell
                        let idx = ((ghost_frac * 4.0) as usize).clamp(0, 3);
                        BRAILLE_FRAC[idx]
                    };
                    (s, dim(cr, cg, cb, alpha))
                };
                let cell = &mut buf[(ghost_x, row)];
                cell.set_symbol(sym);
                cell.set_style(Style::default().fg(color));
            }

            // ── Main bar ───────────────────────────────────────────────────────
            // Drops: single-row indicator at the baseline (rfb==0) using the bar
            // char in drop_color, distinguishing them from tall RTT bars.
            let in_bar = if is_waiting || is_drop {
                rfb == 0
            } else {
                rfb < cur_full || (rfb == cur_full && cur_frac > 0.1)
            };
            if in_bar {
                let sym = if is_waiting || is_drop {
                    if ascii { "x" } else { "✗" }
                } else if rfb < cur_full {
                    if ascii { "#" } else { "⣿" }
                } else {
                    // fractional top cell
                    let idx = ((cur_frac * 4.0) as usize).clamp(0, 3);
                    if ascii { "#" } else { BRAILLE_FRAC[idx] }
                };
                let bar_color = if is_drop {
                    args.theme.drop_color
                } else {
                    Color::Rgb(cr, cg, cb)
                };
                for bx in bar_x..(bar_x + bar_w as u16).min(col_right) {
                    let cell = &mut buf[(bx, row)];
                    cell.set_symbol(sym);
                    cell.set_style(Style::default().fg(bar_color));
                }
            }

            // ── Avg line (rendered on top, spans ghost + bar zone) ─────────────
            if avg_rtt > 0.0 && rfb == avg_rfb {
                let avg_sym   = if ascii { "-" } else { "─" };
                let avg_color = dim(cr, cg, cb, 0.65);
                let avg_style = Style::default().fg(avg_color);
                for ax in col_x..(bar_x + bar_w as u16).min(col_right) {
                    let cell = &mut buf[(ax, row)];
                    cell.set_symbol(avg_sym);
                    cell.set_style(avg_style);
                }
            }
            // ── p95 reference line (wide columns only) ────────────────────────
            if p95_rtt > 0.0 && rfb == p95_rfb && rfb != avg_rfb {
                let p95_sym   = if ascii { "." } else { "╌" };
                let p95_color = dim(cr, cg, cb, 0.45);
                let p95_style = Style::default().fg(p95_color);
                for ax in col_x..(bar_x + bar_w as u16).min(col_right) {
                    let cell = &mut buf[(ax, row)];
                    cell.set_symbol(p95_sym);
                    cell.set_style(p95_style);
                }
            }
        }
    }

    // ── Top horizontal line (dotted, marks scale maximum; drawn last to stay on top) ──
    {
        let top_row = body.y;
        let top_sym = if ascii { "." } else { "╌" };
        for cx in (axis_x + 1)..clip_x {
            let cell = &mut buf[(cx, top_row)];
            cell.set_symbol(top_sym);
            cell.set_style(gray);
        }
    }
}

// ── public draw entry point ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_bars(
    frame:             &mut Frame,
    states:            &[TargetState],
    bars:              &mut BarsState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, tick, dialog, sort_order, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    let n    = states.len() as u16;
    {
        let (min_w, min_h) = super::min_size("bars", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
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
    constraints.push(Constraint::Min(1)); // body (chart + footer)

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset = if show_col_keys { 2usize } else { 0 };
    let header_rows     = (n * rows_per_target) as usize;
    let body_idx        = col_keys_offset + header_rows;
    let body            = chunks[body_idx];

    // Black background
    frame.render_widget(Block::default().style(Style::default().bg(Color::Black)), area);

    let col_key_areas = if show_col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    // Compute chart dimensions.  Bail if the body area is too small.
    let chart_h = body.height.saturating_sub(FOOTER_H);
    let bars_w  = body.width.saturating_sub(SCALE_W);
    if n == 0 || bars_w < MIN_COL_W { return; }
    let col_w = (bars_w / n).max(1);
    if col_w < 2 { return; }

    // Buffer pass: Y-axis, scale labels, bar columns, ghost trails, avg lines.
    {
        let buf = frame.buffer_mut();
        render_bars_chart(buf, body, states, bars, shared_scale, sort_order, args, chart_h, col_w);
    }

    // Widget pass: per-column footer (bold label + current RTT / avg line).
    if body.height > chart_h {
        let label_y = body.y + chart_h;
        let stats_y = label_y + 1;

        for (disp_idx, &slot) in sort_order.iter().enumerate().take(states.len()) {
            if slot >= states.len() { break; }
            let state = &states[slot];
            let col_x = body.x + SCALE_W + disp_idx as u16 * col_w;
            if col_x >= body.x + body.width { break; }
            let avail = col_w.min(body.x + body.width - col_x) as usize;

            let (cr, cg, cb) = args.theme.target_color(slot);
            let target_color = Color::Rgb(cr, cg, cb);
            let is_drop      = state.last_was_drop;
            let rect_col     = Rect::new(col_x, label_y, avail as u16, 1);

            // Label row: truncated hostname/label in bold target colour.
            if label_y < body.y + body.height {
                let label: String = state.label.chars().take(avail).collect();
                frame.render_widget(
                    Paragraph::new(Span::styled(label, Style::default().fg(target_color).add_modifier(Modifier::BOLD))),
                    rect_col,
                );
            }

            // Stats row: "RTT  a:AVG" with separate colours.
            if stats_y < body.y + body.height {
                let avg_rtt   = if args.is_window() { state.win_avg() } else { state.avg_latency() };
                let rtt_str   = if state.waiting { "...".to_string() }
                                else if is_drop  { "DROP".to_string() }
                                else if state.last_rtt > 0.0 { fmt_rtt(state.last_rtt) }
                                else { "~".to_string() };
                let avg_str   = format!(" a:{}", fmt_rtt(avg_rtt));
                let rtt_color = if is_drop { args.theme.drop_color }
                                else { accent_color(state, args, target_color) };

                let rtt_len = rtt_str.chars().count();
                let avg_len = avg_str.chars().count();

                let spans: Vec<Span> = if rtt_len + avg_len <= avail {
                    vec![
                        Span::styled(rtt_str, Style::default().fg(rtt_color)),
                        Span::styled(avg_str, Style::default().fg(Color::DarkGray)),
                    ]
                } else if rtt_len <= avail {
                    let short: String = avg_str.chars().take(avail - rtt_len).collect();
                    vec![
                        Span::styled(rtt_str, Style::default().fg(rtt_color)),
                        Span::styled(short,   Style::default().fg(Color::DarkGray)),
                    ]
                } else {
                    let short: String = rtt_str.chars().take(avail).collect();
                    vec![Span::styled(short, Style::default().fg(rtt_color))]
                };

                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(col_x, stats_y, avail as u16, 1),
                );
            }
        }
    }

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);
    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(frame, area, args, dialog, frozen, sort_mode, states.len(), "bars", tick, !log_fmt.is_empty(), 0, no_data, show_col_keys, show_headers);
}
