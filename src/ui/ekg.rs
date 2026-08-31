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

//! EKG / hospital-monitor screensaver.
//!
//! N horizontal bands (one per target) stacked vertically, each showing a
//! scrolling RTT trace.  RTT maps to vertical height within the band.
//! Packet drops break the trace and stamp a red × marker.
//!
//! In Unicode mode each terminal cell is rendered as a braille character,
//! giving 2× horizontal and 4× vertical sub-cell resolution compared to
//! one-character-per-sample ASCII mode.

use std::collections::VecDeque;
use crate::time::Instant;
use crate::constants::GRAPH_ANIM_SECS;
use crate::types::Sample;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Paragraph},
};
use crate::state::TargetState;
use super::ViewCtx;
use super::{
    compute_no_data,
    layout::{render_window_label, render_dialogs},
};

// ── braille bit layout ────────────────────────────────────────────────────────
//
//  Sub-row │ left col │ right col
//    0 (top)│  0x01    │  0x08
//    1      │  0x02    │  0x10
//    2      │  0x04    │  0x20
//    3 (bot)│  0x40    │  0x80
//
const BL: [u8; 4] = [0x01, 0x02, 0x04, 0x40]; // left braille column, sub-rows 0-3
const BR: [u8; 4] = [0x08, 0x10, 0x20, 0x80]; // right braille column, sub-rows 0-3

// ── state ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct EkgState {
    /// Per-target ring buffer: Some(rtt_ms) or None (drop / no data).
    /// Raw millisecond values - normalized to [0,1] at render time so rescale animates smoothly.
    pub traces:    Vec<VecDeque<Option<f64>>>,
    pub n_targets: usize,
    prev_sent:     Vec<u64>,  // last total_sent seen per slot
    prev_drops:    Vec<u64>,  // last drops seen per slot
    backfilled:    bool,
}

impl EkgState {
    pub fn new(n_targets: usize) -> Self {
        EkgState {
            traces:    (0..n_targets).map(|_| VecDeque::new()).collect(),
            n_targets,
            prev_sent:  vec![0u64; n_targets],
            prev_drops: vec![0u64; n_targets],
            backfilled: false,
        }
    }

    /// Populate traces from existing graph_history so the view isn't blank on entry.
    fn backfill(&mut self, states: &[TargetState], max_w: usize) {
        let n = self.n_targets.min(states.len());
        for (slot, state) in states.iter().enumerate().take(n) {
            if state.waiting { continue; }

            let hist  = &state.graph_history;
            let take  = hist.len().min(max_w);
            let start = hist.len().saturating_sub(take);
            for sample in &hist[start..] {
                let val = match sample {
                    Sample::Hit(rtt) => Some(*rtt),
                    _                => None,
                };
                self.traces[slot].push_back(val);
            }

            // Sync counters so the first live push doesn't double-count.
            self.prev_sent[slot]  = state.total_sent;
            self.prev_drops[slot] = state.drops as u64;
        }
    }

    /// Called each fast tick.  Pushes at most one new sample per target when
    /// `total_sent` has advanced (i.e. a new probe result arrived).
    /// Raw ms values are stored; normalization happens at render time so rescale animates.
    pub fn push(&mut self, states: &[TargetState], max_w: usize) {
        if !self.backfilled {
            self.backfill(states, max_w);
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

            let raw_ms = if is_drop || state.last_rtt <= 0.0
                            || state.last_rtt == f64::MAX
                            || state.last_rtt == f64::MIN
            {
                None
            } else {
                Some(state.last_rtt)
            };

            self.traces[slot].push_back(raw_ms);
            while self.traces[slot].len() > max_w.max(1) {
                self.traces[slot].pop_front();
            }
        }
    }
}

// ── rendering ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_ekg(
    frame:             &mut Frame,
    states:            &[TargetState],
    ekg:               &mut EkgState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, tick, dialog, sort_order, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    let n    = states.len() as u16;
    {
        let (min_w, min_h) = super::min_size("ekg", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            super::layout::draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let rows_per_target: u16  = if !show_headers { 0 } else { 1 };

    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    constraints.extend((0..(n * rows_per_target)).map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Min(1)); // ekg field

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset = if show_col_keys { 2usize } else { 0 };
    let header_rows   = (n * rows_per_target) as usize;
    let ekg_chunk_idx = col_keys_offset + header_rows;

    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let col_key_areas = if show_col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);

    let ekg_area = chunks[ekg_chunk_idx];
    let trace_w  = ekg_area.width as usize;

    if trace_w == 0 || ekg_area.height == 0 { return; }

    // Compute a per-frame lerped scale for smooth rescale animation.
    // Reuses the same graph_anim_start / scale_anim_old/new that trigger_scale_anim sets.
    let lerped_scale: f64 = {
        let anim = states.iter().find(|s| !s.waiting && s.graph_anim_start.is_some());
        match anim {
            Some(s) => {
                let t = (s.graph_anim_start.unwrap().elapsed().as_secs_f64() / GRAPH_ANIM_SECS).min(1.0);
                let eased = t * t * (3.0 - 2.0 * t);
                let old = if s.scale_anim_old > 0.0 { s.scale_anim_old } else { shared_scale };
                old + (shared_scale - old) * eased
            }
            None => shared_scale,
        }
    };
    let render_scale = if lerped_scale > 0.0 { lerped_scale } else { shared_scale.max(1.0) };

    // ── divide EKG area into N horizontal bands (stacked vertically) ──────────
    let n_targets = states.len();
    if n_targets == 0 { return; }

    // Equal-height bands; last band gets any remainder rows.
    let band_h_base = ekg_area.height / n_targets as u16;
    let remainder   = ekg_area.height % n_targets as u16;

    let buf = frame.buffer_mut();

    // Precompute per-band top offsets.
    let mut band_tops: Vec<u16> = Vec::with_capacity(n_targets);
    let mut cursor = 0u16;
    for i in 0..n_targets {
        band_tops.push(cursor);
        let ex = if (i as u16) < remainder { 1u16 } else { 0 };
        cursor += band_h_base + ex;
    }

    for (band_idx, &slot) in sort_order.iter().enumerate() {
        if band_idx >= n_targets { break; }

        let extra    = if (band_idx as u16) < remainder { 1u16 } else { 0 };
        let bh       = (band_h_base + extra).max(1);
        let band_top = ekg_area.y + band_tops[band_idx];

        if band_top >= ekg_area.y + ekg_area.height { break; }

        let is_last = band_idx == n_targets - 1;

        // Draw separator line at the bottom of every non-last band.
        if !is_last && bh >= 2 {
            let sep_y = band_top + bh - 1;
            for x in 0..trace_w {
                let cx = ekg_area.x + x as u16;
                let cell = &mut buf[(cx, sep_y)];
                cell.set_symbol("─");
                cell.set_style(Style::default().fg(Color::Rgb(28, 28, 28)));
            }
        }

        // Draw height: leave 1 row for the separator on non-last bands.
        let draw_h = if !is_last && bh >= 2 { bh - 1 } else { bh };
        if draw_h == 0 { continue; }

        let (tr, tg, tb) = args.theme.target_color(slot);
        let trace     = &ekg.traces[slot];
        let trace_len = trace.len();

        if args.ascii {
            // ── ASCII: 1 sample per terminal column ───────────────────────────
            // Show the newest `trace_w` samples (buffer may hold more).
            let display_len  = trace_len.min(trace_w);
            let trace_offset = trace_len - display_len;
            let empty_cols   = trace_w - display_len;
            let mut prev_y: Option<u16> = None;

            for x in 0..trace_w {
                let cx = ekg_area.x + x as u16;
                if x < empty_cols { prev_y = None; continue; }

                let sample_idx = trace_offset + (x - empty_cols);
                let brightness = 0.35 + 0.65 * (x as f64 / trace_w.max(1) as f64);

                match trace[sample_idx] {
                    None => {
                        let cy = band_top + draw_h / 2;
                        if cy < band_top + draw_h {
                            let cell = &mut buf[(cx, cy)];
                            cell.set_symbol("×");
                            cell.set_style(Style::default()
                                .fg(Color::Rgb(200, 50, 50))
                                .add_modifier(Modifier::BOLD));
                        }
                        prev_y = None;
                    }
                    Some(rtt_ms) => {
                        let norm   = (rtt_ms / render_scale).clamp(0.0, 1.0);
                        let raw_y  = ((1.0 - norm) * (draw_h as f64 - 1.0)).round() as u16;
                        let this_y = band_top + raw_y.min(draw_h - 1);
                        let fg = Color::Rgb(
                            (tr as f64 * brightness) as u8,
                            (tg as f64 * brightness) as u8,
                            (tb as f64 * brightness) as u8,
                        );
                        if let Some(py) = prev_y {
                            let (y_lo, y_hi) = if py <= this_y { (py, this_y) } else { (this_y, py) };
                            for y in y_lo..=y_hi {
                                let cell = &mut buf[(cx, y)];
                                if y == this_y { cell.set_symbol("·"); } else { cell.set_symbol("│"); }
                                cell.set_style(Style::default().fg(fg));
                            }
                        } else {
                            let cell = &mut buf[(cx, this_y)];
                            cell.set_symbol("·");
                            cell.set_style(Style::default().fg(fg));
                        }
                        prev_y = Some(this_y);
                    }
                }
            }

            // Bright cursor dot at newest sample.
            if let Some(&Some(rtt_ms)) = trace.back() {
                let norm   = (rtt_ms / render_scale).clamp(0.0, 1.0);
                let raw_y  = ((1.0 - norm) * (draw_h as f64 - 1.0)).round() as u16;
                let this_y = band_top + raw_y.min(draw_h - 1);
                let cx = ekg_area.x + trace_w.min(ekg_area.width as usize) as u16 - 1;
                if cx < ekg_area.x + ekg_area.width {
                    let cell = &mut buf[(cx, this_y)];
                    cell.set_symbol("●");
                    cell.set_style(Style::default()
                        .fg(Color::Rgb(
                            (tr as u16 + 100).min(255) as u8,
                            (tg as u16 + 100).min(255) as u8,
                            (tb as u16 + 100).min(255) as u8,
                        ))
                        .add_modifier(Modifier::BOLD));
                }
            }
        } else {
            // ── Braille: 2 samples per terminal column, 4 sub-rows per row ───
            //
            // Virtual columns: n_vcols = trace_w * 2 (one per braille half-column)
            // Virtual rows:    n_vrows = draw_h * 4  (one per braille dot row)
            //
            // Each terminal cell (cell_x, cell_y) covers virtual columns
            // [cell_x*2, cell_x*2+1] and virtual rows [cell_y*4 .. cell_y*4+3].
            // We accumulate braille bits per cell, then render in one pass.

            let n_vcols = trace_w * 2;
            let n_vrows = draw_h as usize * 4;

            let display_len  = trace_len.min(n_vcols);
            let trace_offset = trace_len - display_len;
            let empty_vcols  = n_vcols - display_len;

            let mut bits:   Vec<Vec<u8>>    = vec![vec![0u8;          draw_h as usize]; trace_w];
            let mut colors: Vec<Vec<Color>> = vec![vec![Color::Black; draw_h as usize]; trace_w];
            let mut drops:  Vec<(u16, u16)> = Vec::new();

            let mut prev_vy: Option<i32> = None;

            for v in 0..n_vcols {
                let cell_x      = v / 2;
                let braille_col = v % 2; // 0 = left half-column, 1 = right

                if v < empty_vcols { prev_vy = None; continue; }

                let sample_idx = trace_offset + (v - empty_vcols);
                let brightness = 0.35 + 0.65 * (v as f64 / n_vcols.max(1) as f64);
                let fg = Color::Rgb(
                    (tr as f64 * brightness) as u8,
                    (tg as f64 * brightness) as u8,
                    (tb as f64 * brightness) as u8,
                );

                match trace[sample_idx] {
                    None => {
                        drops.push((cell_x as u16, band_top + draw_h / 2));
                        prev_vy = None;
                    }
                    Some(rtt_ms) => {
                        let norm    = (rtt_ms / render_scale).clamp(0.0, 1.0);
                        let this_vy = ((1.0 - norm) * (n_vrows as f64 - 1.0)).round() as i32;
                        let this_vy = this_vy.clamp(0, n_vrows as i32 - 1);
                        let (vy_lo, vy_hi) = match prev_vy {
                            Some(p) => (p.min(this_vy), p.max(this_vy)),
                            None    => (this_vy, this_vy),
                        };
                        let col_bits = if braille_col == 0 { &BL } else { &BR };
                        for vy in vy_lo..=vy_hi {
                            let cy      = vy as usize / 4;
                            let sub_row = vy as usize % 4;
                            if cy < draw_h as usize {
                                bits[cell_x][cy]   |= col_bits[sub_row];
                                colors[cell_x][cy]  = fg;
                            }
                        }
                        prev_vy = Some(this_vy);
                    }
                }
            }

            // Render accumulated braille cells.
            for cell_x in 0..trace_w {
                for cell_y in 0..draw_h as usize {
                    if bits[cell_x][cell_y] == 0 { continue; }
                    let cx = ekg_area.x + cell_x as u16;
                    let cy = band_top + cell_y as u16;
                    if cx >= ekg_area.x + ekg_area.width { continue; }
                    let ch = char::from_u32(0x2800 | bits[cell_x][cell_y] as u32).unwrap_or(' ');
                    let cell = &mut buf[(cx, cy)];
                    cell.set_symbol(&ch.to_string());
                    cell.set_style(Style::default().fg(colors[cell_x][cell_y]));
                }
            }

            // Drop markers (overwrite braille at that cell).
            for (cx_off, cy) in drops {
                let cx = ekg_area.x + cx_off;
                if cx < ekg_area.x + ekg_area.width && cy < band_top + draw_h {
                    let cell = &mut buf[(cx, cy)];
                    cell.set_symbol("×");
                    cell.set_style(Style::default()
                        .fg(Color::Rgb(200, 50, 50))
                        .add_modifier(Modifier::BOLD));
                }
            }

            // Bright cursor dot at newest sample's terminal position.
            if let Some(&Some(rtt_ms)) = trace.back() {
                let norm    = (rtt_ms / render_scale).clamp(0.0, 1.0);
                let this_vy = ((1.0 - norm) * (n_vrows as f64 - 1.0)).round() as i32;
                let cell_y  = (this_vy.clamp(0, n_vrows as i32 - 1) as usize / 4) as u16;
                let cx = ekg_area.x + trace_w as u16 - 1;
                let cy = band_top + cell_y;
                if cx < ekg_area.x + ekg_area.width && cy < band_top + draw_h {
                    let cell = &mut buf[(cx, cy)];
                    cell.set_symbol("●");
                    cell.set_style(Style::default()
                        .fg(Color::Rgb(
                            (tr as u16 + 100).min(255) as u8,
                            (tg as u16 + 100).min(255) as u8,
                            (tb as u16 + 100).min(255) as u8,
                        ))
                        .add_modifier(Modifier::BOLD));
                }
            }
        }

        // Target label: dim, top-left of the band - omitted for single-target sessions.
        if n_targets > 1 {
            let label     = &states[slot].label;
            let label_fg  = Color::Rgb((tr / 2).max(40), (tg / 2).max(40), (tb / 2).max(40));
            let label_style = Style::default().fg(label_fg);
            for (col, ch) in label.chars().enumerate() {
                let cx = ekg_area.x + col as u16;
                if cx >= ekg_area.x + ekg_area.width { break; }
                let cell = &mut buf[(cx, band_top)];
                cell.set_symbol(&ch.to_string());
                cell.set_style(label_style);
            }
        }
    }

    // ── calibrating overlay ───────────────────────────────────────────────────
    {
        let now = Instant::now();
        if let Some((start, until)) = states.iter()
            .filter_map(|s| s.calibrating)
            .max_by_key(|&(_, u)| u)
        {
            if until > now {
                use ratatui::text::{Line, Span};
                use super::layout::loading_message;
                let total_ms   = until.duration_since(start).as_millis().max(1) as f64;
                let elapsed_ms = now.duration_since(start).as_millis() as f64;
                let progress   = (elapsed_ms / total_ms).clamp(0.0, 1.0);
                let secs_left  = until.duration_since(now).as_secs() + 1;
                const BAR_W: usize = 16;
                let filled = (progress * BAR_W as f64).round() as usize;
                let bar    = "█".repeat(filled) + &"░".repeat(BAR_W - filled);
                let text   = format!("{}  {}  {}s", loading_message(), bar, secs_left);
                let tw     = text.chars().count() as u16;
                let ox     = ekg_area.x + (ekg_area.width.saturating_sub(tw)) / 2;
                let oy     = ekg_area.y + ekg_area.height / 2;
                let rect   = ratatui::layout::Rect::new(ox, oy, tw.min(ekg_area.width), 1);
                let br = 140u8;
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        text,
                        Style::default().fg(Color::Rgb(br, br, br)).add_modifier(Modifier::ITALIC),
                    ))),
                    rect,
                );
            }
        }
    }

    // ── frozen / dialog overlay ───────────────────────────────────────────────
    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(frame, area, args, dialog, frozen, sort_mode, states.len(), "ekg", tick, !log_fmt.is_empty(), 0, no_data, show_col_keys, show_headers);
}
