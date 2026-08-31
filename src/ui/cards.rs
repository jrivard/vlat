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

//! Cards grid view.
//!
//! One bordered panel per target, laid out in a grid that fills the terminal.
//! Each panel shows the current RTT, a recent-history sparkline, a range bar,
//! and as many additional stat pairs (avg, loss, jitter, p95, …) as fit in the
//! remaining rows.  Panels are capped at a maximum size so a typical fullscreen
//! console shows roughly three across; targets past the bottom edge are clipped.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use crate::cli::{Args, BaseStat, ExtraStat};
use crate::state::TargetState;
use super::ViewCtx;
use super::{
    accent_color,
    compute_no_data,
    fmt_count, fmt_cv, fmt_rtt, fmt_rtt_nodec,
    trim_range_bar,
    widgets,
    layout::{render_window_label, render_dialogs, truncate_line},
};

// Outer panel width is capped here so panels stay readable; on an ~80-col
// console this yields about three panels per row.
pub const MAX_PANEL_W: u16 = 34;
// Smallest outer panel width that still reads usefully.
pub const MIN_PANEL_W: u16 = 20;
// Panel height is content-fit (sized to the rows the card actually draws),
// clamped to this range so it never collapses to nothing or grows unbounded.
pub const MIN_PANEL_H: u16 = 5;
const MAX_PANEL_H: u16 = 13;
// Gap (in cells) between adjacent panels.
const HGAP: u16 = 1;
const VGAP: u16 = 0;

/// Discriminator / freeze-snapshot holder for the cards view.  The panels read
/// their history directly from each `TargetState`, so no extra buffers are kept.
#[derive(Clone)]
#[allow(dead_code)]
pub struct CardsState {
    pub n_targets: usize,
}

impl CardsState {
    pub fn new(n_targets: usize) -> Self {
        CardsState { n_targets }
    }
}

/// Decide the horizontal grid geometry for the available area.
/// Returns (n_cols, panel_w).  Panel *height* is content-fit (see `panel_height`).
fn grid_geometry(area_w: u16, n_targets: u16) -> (u16, u16) {
    // Fewest columns such that each panel is no wider than MAX_PANEL_W.
    let by_max = ((area_w + HGAP) as f32 / (MAX_PANEL_W + HGAP) as f32).ceil() as u16;
    // Most columns such that each panel is still at least MIN_PANEL_W.
    let by_min = ((area_w + HGAP) / (MIN_PANEL_W + HGAP)).max(1);
    let n_cols = by_max.clamp(1, by_min).min(n_targets.max(1));

    let panel_w = (((area_w + HGAP) / n_cols).saturating_sub(HGAP)).clamp(2, MAX_PANEL_W);
    (n_cols, panel_w)
}

/// Number of stat *cells* a card will draw given the active column selection:
/// visible base stats plus enabled numerical extras (recent/bar are their own rows).
fn stat_cell_count(args: &Args) -> usize {
    let base = [BaseStat::Avg, BaseStat::Range, BaseStat::Jitter, BaseStat::Drops]
        .iter().filter(|b| !args.hidden_base_stats.contains(b)).count();
    let extras = args.extra_stats.iter().filter(|e| matches!(e,
        ExtraStat::Mtr | ExtraStat::Std | ExtraStat::P01 | ExtraStat::P10 | ExtraStat::P50
        | ExtraStat::P95 | ExtraStat::P99 | ExtraStat::Cv | ExtraStat::Srtt | ExtraStat::Streak
    )).count();
    base + extras
}

/// Content-fit outer panel height for the given inner width and column selection:
/// border (2) + RTT row (1) + optional recent/range rows + stat rows, clamped.
fn panel_height(args: &Args, panel_w: u16) -> u16 {
    let inner_w = panel_w.saturating_sub(2) as usize;
    let per_row = if inner_w >= 18 { 2 } else { 1 };
    let stat_rows = stat_cell_count(args).div_ceil(per_row) as u16;
    let recent = if args.extra_stats.contains(&ExtraStat::Recent) { 1 } else { 0 };
    let bar    = if args.extra_stats.contains(&ExtraStat::Bar) && inner_w >= 4 { 1 } else { 0 };
    let content = 1 + recent + bar + stat_rows; // inner rows
    (content + 2).clamp(MIN_PANEL_H, MAX_PANEL_H)
}

/// Format a packet-loss percentage compactly.
fn fmt_loss(pct: f64) -> String {
    if pct <= 0.0 { "0%".to_string() }
    else if pct < 10.0 { format!("{:.1}%", pct) }
    else { format!("{:.0}%", pct) }
}

/// Build the ordered list of stat cells (label, value, value-style) shown below
/// the sparklines.  Mirrors the column system: visible base stats first (avg,
/// range, jitter, loss), then the enabled `extra_stats` in display order.  The
/// `Recent`/`Bar` extras are handled as dedicated rows by the caller, not here.
fn stat_cells(state: &TargetState, args: &Args, theme: &super::Theme) -> Vec<(&'static str, String, Style)> {
    let win   = args.is_window();
    let ascii = args.ascii;
    let val   = Style::default().fg(Color::Gray);
    let hide  = |b: BaseStat| args.hidden_base_stats.contains(&b);
    let mut v: Vec<(&'static str, String, Style)> = Vec::new();

    // ── Base stats (same order as the stats line) ─────────────────────────────
    if !hide(BaseStat::Avg) {
        let avg = if win { state.win_avg() } else { state.avg_latency() };
        v.push(("avg", fmt_rtt(avg), val));
    }
    if !hide(BaseStat::Range) {
        let (min, max) = if win { (state.win_min(), state.win_max()) }
                         else    { (state.life_min(), state.life_max()) };
        let arrow = if ascii { "-" } else { "\u{2194}" };
        v.push(("rng", format!("{}{}{}", fmt_rtt_nodec(min), arrow, fmt_rtt_nodec(max)), val));
    }
    if !hide(BaseStat::Jitter) {
        let jit = if win { state.win_jitter_avg() } else { state.avg_jitter() };
        v.push(("jit", fmt_rtt(jit), val));
    }
    if !hide(BaseStat::Drops) {
        let loss = if win { state.win_loss_pct() } else { state.life_loss_pct() };
        let loss_style = if loss > 0.0 { Style::default().fg(theme.drop_color) } else { val };
        v.push(("loss", fmt_loss(loss), loss_style));
    }

    // ── Optional extra stats, in the user's column order ──────────────────────
    for stat in &args.extra_stats {
        match stat {
            ExtraStat::Mtr => {
                let m = if win { state.win_mtr() } else { state.life_mtr() };
                v.push(("mtr", m.map(fmt_rtt).unwrap_or_else(|| "~".into()), val));
            }
            ExtraStat::Std => {
                let s = if win { state.win_stddev() } else { state.life_stddev() };
                v.push(("std", fmt_rtt(s), val));
            }
            ExtraStat::P01 => v.push(("p01", fmt_rtt(if win { state.win_p01() } else { state.life_p01() }), val)),
            ExtraStat::P10 => v.push(("p10", fmt_rtt(if win { state.win_p10() } else { state.life_p10() }), val)),
            ExtraStat::P50 => v.push(("p50", fmt_rtt(if win { state.win_median() } else { state.life_median() }), val)),
            ExtraStat::P95 => v.push(("p95", fmt_rtt(if win { state.win_p95() } else { state.life_p95() }), val)),
            ExtraStat::P99 => v.push(("p99", fmt_rtt(if win { state.win_p99() } else { state.life_p99() }), val)),
            ExtraStat::Cv => {
                let cv = if win { state.win_cv() } else { state.life_cv() };
                v.push(("cv", fmt_cv(cv), val));
            }
            ExtraStat::Srtt => v.push(("srtt", if state.srtt > 0.0 { fmt_rtt(state.srtt) } else { "~".into() }, val)),
            ExtraStat::Streak => v.push(("strk", fmt_count(state.cur_drop_streak as u64), val)),
            // Recent / Bar are dedicated rows; identity / meta variants never appear here.
            _ => {}
        }
    }

    v
}

/// Render a single target panel into `rect`.
fn render_panel(
    frame:        &mut Frame,
    rect:         Rect,
    state:        &TargetState,
    slot:         usize,
    args:         &Args,
    shared_scale: f64,
) {
    if rect.width < 4 || rect.height < 3 { return; }

    let (cr, cg, cb)   = args.theme.target_color(slot);
    let target_color   = Color::Rgb(cr, cg, cb);
    let border_color   = accent_color(state, args, target_color);
    let label_style    = Style::default().fg(target_color).add_modifier(Modifier::BOLD);
    let lbl_dim        = Style::default().fg(Color::DarkGray);

    // Title: truncated label in the top border.
    let title_budget = rect.width.saturating_sub(4) as usize;
    let title: String = state.label.chars().take(title_budget).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if args.ascii { BorderType::Plain } else { BorderType::Rounded })
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(format!(" {} ", title), label_style));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    if inner.width == 0 || inner.height == 0 { return; }
    let inner_w = inner.width as usize;
    let inner_h = inner.height;

    // Running cursor over inner rows; each section claims rows top-down and the
    // next section starts wherever the previous one left off (so toggling the
    // recent/range rows collapses the layout instead of leaving gaps).
    let mut row: u16 = 0;
    macro_rules! row_rect {
        () => {{ let r = Rect::new(inner.x, inner.y + row, inner.width, 1); row += 1; r }};
    }

    // ── Current RTT + trend arrow (always shown) ──────────────────────────────
    {
        let is_drop = state.history.iter().rev().find(|h| !h.is_pending())
            .map(|h| h.is_drop()).unwrap_or(false);
        let (rtt_str, rtt_style) = if state.waiting || state.resolve_error.is_some() {
            ("…".to_string(), Style::default().fg(Color::DarkGray))
        } else if is_drop {
            ("DROP".to_string(), Style::default().fg(args.theme.drop_color).add_modifier(Modifier::BOLD))
        } else if state.last_rtt > 0.0 {
            (fmt_rtt(state.last_rtt), Style::default().fg(accent_color(state, args, target_color)).add_modifier(Modifier::BOLD))
        } else {
            ("~".to_string(), Style::default().fg(Color::DarkGray))
        };
        let mut spans = vec![Span::styled(rtt_str, rtt_style)];
        spans.push(Span::raw("  "));
        spans.push(widgets::trend_spark_span(state.mtr_trend(args.graph_interval), args.ascii, &args.theme));
        frame.render_widget(Paragraph::new(truncate_line(Line::from(spans), inner_w)), row_rect!());
    }

    // ── Recent-history sparkline (gated on the `recent` column) ────────────────
    if args.extra_stats.contains(&ExtraStat::Recent) && row < inner_h {
        let spans = widgets::build_target_sparkline_spans(state, args, inner_w, shared_scale, false);
        frame.render_widget(Paragraph::new(truncate_line(Line::from(spans), inner_w)), row_rect!());
    }

    // ── Range bar (gated on the `bar` column) ─────────────────────────────────
    if args.extra_stats.contains(&ExtraStat::Bar) && row < inner_h && inner_w >= 4 {
        let bar = trim_range_bar(widgets::build_range_bar_spans(
            state, args.ascii, shared_scale, &args.theme, inner_w, false,
        ));
        frame.render_widget(Paragraph::new(truncate_line(Line::from(bar), inner_w)), row_rect!());
    }

    // ── Stat pairs, as many as fit ────────────────────────────────────────────
    if row < inner_h {
        let stat_rows = (inner_h - row) as usize;
        let per_row   = if inner_w >= 18 { 2 } else { 1 };
        let cell_w    = inner_w / per_row;
        let cells     = stat_cells(state, args, &args.theme);

        for chunk in cells.chunks(per_row).take(stat_rows) {
            let mut spans: Vec<Span> = Vec::new();
            for (lbl, value, vstyle) in chunk {
                let body_w   = cell_w.saturating_sub(1); // leave a column between cells
                let prefix   = format!("{} ", lbl);
                let plen     = prefix.chars().count();
                let val_room = body_w.saturating_sub(plen);
                let val_str: String = value.chars().take(val_room).collect();
                let used     = plen + val_str.chars().count();
                spans.push(Span::styled(prefix, lbl_dim));
                spans.push(Span::styled(val_str, *vstyle));
                let pad = (cell_w).saturating_sub(used);
                if pad > 0 { spans.push(Span::raw(" ".repeat(pad))); }
            }
            frame.render_widget(Paragraph::new(truncate_line(Line::from(spans), inner_w)), row_rect!());
        }
    }
}

// ── public draw entry point ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn draw_cards(
    frame:             &mut Frame,
    states:            &[TargetState],
    _cards:           &mut CardsState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, tick, dialog, sort_order, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    {
        let (min_w, min_h) = super::min_size("cards", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            super::layout::draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    // Black background.
    frame.render_widget(Block::default().style(Style::default().bg(Color::Black)), area);

    let n = states.len();
    if n == 0 { return; }

    let (n_cols, panel_w) = grid_geometry(area.width, n as u16);
    let panel_h = panel_height(args, panel_w);
    if panel_w < 4 || panel_h < 3 { return; }

    let n_rows = ((area.height + VGAP) / (panel_h + VGAP)).max(1);
    let capacity = (n_cols * n_rows) as usize;

    for (disp_idx, &slot) in sort_order.iter().enumerate().take(n.min(capacity)) {
        if slot >= states.len() { continue; }
        let col = disp_idx as u16 % n_cols;
        let row = disp_idx as u16 / n_cols;
        let px  = area.x + col * (panel_w + HGAP);
        let py  = area.y + row * (panel_h + VGAP);
        if px + panel_w > area.x + area.width || py + panel_h > area.y + area.height { continue; }
        let rect = Rect::new(px, py, panel_w, panel_h);
        render_panel(frame, rect, &states[slot], slot, args, shared_scale);
    }

    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed);
    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(frame, area, args, dialog, frozen, sort_mode, states.len(), "cards", tick, !log_fmt.is_empty(), 0, no_data, show_col_keys, show_headers);
}
