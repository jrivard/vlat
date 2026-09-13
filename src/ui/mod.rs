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

pub mod bars;
pub mod bubble;
pub mod dialogs;
pub mod ekg;
pub mod labels;
pub mod layout;
pub mod cards;
pub mod logo;
pub mod pong;
pub mod radar;
pub mod scatter;
pub mod worm;
pub mod theme;
pub mod widgets;

pub use bars::{draw_bars, BarsState};
pub use bubble::{draw_bubble, BubbleState};
pub use cards::{draw_cards, CardsState};
pub use scatter::{draw_scatter, ScatterState, AxisMetric};
pub use dialogs::{DialogMode, HelpSubMenu, HelpItem, AxisField, help_menu_items, HELP_VIEWS, HELP_SORTS, HELP_THEMES, VIEW_DISPLAY_ORDER, VIEW_PICKER_ORDER, explain_max_scroll, help_dialog_ideal_height};
pub use logo::LogoAnim;
pub use ekg::{draw_ekg, EkgState};
pub use layout::{draw_list_ui, draw_single_ui, draw_fullscreen_ui, draw_fullscreen_multi_ui, truncate_line, single_history_avail};
pub use pong::{draw_pong, pong_left_margin, PongState};
pub use radar::{draw_radar, RadarState};
pub use worm::{draw_worm, WormState};
pub use theme::Theme;
pub use crate::state::scale_bucket;

/// Fixed character width of the UP/DOWN status badge prepended to every stats line.
pub const STATUS_BADGE_W: usize = 3;

use ratatui::{Frame, layout::{Constraint, Direction, Layout, Rect}, style::{Color, Style}, text::{Line, Span}, widgets::Paragraph};
use crate::cli::{Args, BaseStat, ExtraStat, SortMode};
use crate::state::TargetState;
use crate::time::Instant;

/// Per-frame context shared by every full-frame view: CLI args, per-target
/// labels, column geometry, and UI chrome state.  Built once per frame in the
/// main loop; each draw function destructures just the fields it uses.
pub struct ViewCtx<'a> {
    pub args:              &'a Args,
    pub mode_labels:       &'a [String],
    pub col_widths:        &'a ColWidths,
    pub shared_scale:      f64,
    pub log_fmt:           &'a str,
    pub tick:              u64,
    pub dialog:            &'a DialogMode,
    pub sort_order:        &'a [usize],
    pub sort_arrows:       &'a [Option<(crate::time::Instant, bool)>],
    pub sort_mode:         &'a SortMode,
    pub sort_mode_changed: Option<crate::time::Instant>,
    pub frozen:            bool,
    pub show_headers:      bool,
    pub show_col_keys:     bool,
}

pub fn has_drops(state: &TargetState, args: &Args) -> bool {
    if args.is_window() { state.win_drops > 0 } else { state.drops > 0 }
}

pub fn show_dups_any(states: &[TargetState], args: &Args) -> bool {
    if args.is_window() { states.iter().any(|s| s.win_dups > 0) } else { states.iter().any(|s| s.dups > 0) }
}

pub fn accent_color(state: &TargetState, args: &Args, default: Color) -> Color {
    if state.waiting || state.resolve_error.is_some() {
        Color::DarkGray
    } else if has_drops(state, args) {
        args.theme.drop_color
    } else if state.warning_streak >= 3 || state.color_score > 2.0 || state.color_score > 1.0 {
        args.theme.rtt_warn
    } else if state.color_score < -1.0 {
        args.theme.rtt_good
    } else {
        default
    }
}

pub fn trim_range_bar(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut v: Vec<_> = spans.into_iter().skip(4).collect();
    v.truncate(v.len().saturating_sub(3));
    v
}

/// Overlay the ambient cycling logo in the top-right corner of a screensaver/fullscreen
/// view when there are enough header rows (≥ 3) and terminal width to spare.
/// Call this after any screensaver draw function - it is a no-op when conditions aren't met.
pub fn render_ambient_logo(
    f:           &mut Frame,
    states_len:  usize,
    show_headers: bool,
    logo:        &LogoAnim,
    args:        &Args,
) {
    if args.ascii { return; }
    let area = f.area();
    let rows_per_target: u16 = if !show_headers { 0 } else { 1 };
    let header_rows = states_len as u16 * rows_per_target;
    if header_rows < LogoAnim::HEIGHT || area.width < LogoAnim::WIDTH + 30 { return; }
    let logo_x    = area.x + area.width - LogoAnim::WIDTH;
    let logo_area = Rect::new(logo_x, area.y, LogoAnim::WIDTH, LogoAnim::HEIGHT);
    logo.render(f, logo_area, &args.theme);
}

/// Choose the best (addr_gap, col_gap) pair given a combined budget for both.
/// addr_gap is the space between the address column and the first stat (avg);
/// col_gap is the space between stat columns.
/// Tries ideal (3,2) first, falls back in priority order to (1,1) minimum.
fn pick_gaps(total: usize, cw: &ColWidths, show_drp: bool, show_dup: bool) -> (usize, usize) {
    for addr_gap in [3usize, 2, 1] {
        for col_gap in [2usize, 1] {
            if addr_gap + cw.stats_width_with_gap(col_gap, show_drp, show_dup) <= total {
                return (addr_gap, col_gap);
            }
        }
    }
    (1, 1)
}

/// Render the per-target header rows that appear at the top of every screensaver
/// mode (worm, radar, rain, …).
///
/// `chunks` is the full layout slice returned by `Layout::split`; header rows
/// are always the first `n * rows_per_target` entries.  `one_row` controls
/// whether each target gets one combined row or two separate rows.
pub fn render_screensaver_headers(
    frame:        &mut Frame,
    states:       &[TargetState],
    ctx:          &ViewCtx,
    chunks:       &[Rect],
    one_row:      bool,
    col_keys:     Option<(Rect, Rect)>,
) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, tick, sort_order, sort_arrows, show_headers, .. } = ctx;
    // Single-target: match draw_fullscreen_ui - one combined row with circles and inline sparkline.
    const BAR_MIN: usize = 15;
    const BAR_MAX: usize = 25;

    if states.len() == 1 {
        let slot  = sort_order[0];
        let state = &states[slot];
        let mode_label = &mode_labels[slot];
        let border_ch = if args.ascii { "|" } else { "\u{258c}" }; // ▌
        let show_drp = true;
        let show_dup = show_dups_any(std::slice::from_ref(state), args);
        let ip_changes_slot_w = ip_changes_slot_width(args.column_vis.resolve, state.ip_changes);
        let avail_w = if show_headers && !chunks.is_empty() { chunks[0].width as usize } else {
            col_keys.map(|(a, _)| a.width as usize).unwrap_or(80)
        };
        let content_w = avail_w.saturating_sub(1); // accent border
        let show_badge = args.column_vis.mode.unwrap_or(mode_label != "icmp");
        let global_mode: Option<&str> = if mode_label == "icmp" { Some("icmp") } else { None };
        let badge_pad_w = if show_badge { mode_label.len() } else { 0 };
        let badge_w     = if show_badge { badge_pad_w + 3 } else { 0 };
        let name_part_w = if col_widths.name_w > 0 { col_widths.name_w + 2 } else { 0 };
        let prefix_w_base = 2 + badge_w + name_part_w + col_widths.label + ip_changes_slot_w;
        let prefix_w      = prefix_w_base + 3;
        let ideal_stats_w = col_widths.stats_width_with_gap(2, show_drp, show_dup);
        let show_recent = args.extra_stats.contains(&crate::cli::ExtraStat::Recent);
        let show_bar    = args.extra_stats.contains(&crate::cli::ExtraStat::Bar);
        let extra = content_w.saturating_sub(prefix_w);
        let bar_reserve = if show_bar { 1 + BAR_MIN } else { 0 };
        let space_for_spark = extra.saturating_sub(ideal_stats_w + bar_reserve);
        let circles_w = if show_recent { let b = space_for_spark.saturating_sub(1); if b >= widgets::TARGET_SPARK_MIN { b.min(widgets::TARGET_SPARK_W) } else { 0 } } else { 0 };
        let spark_overhead = if circles_w > 0 { circles_w + 1 } else { 0 };
        let after_spark = extra.saturating_sub(spark_overhead);
        let bar_w = if !show_bar { 0 } else if after_spark > ideal_stats_w { (after_spark - ideal_stats_w).clamp(BAR_MIN, BAR_MAX) } else { BAR_MIN };
        let bar_overhead = if bar_w > 0 { 1 + bar_w } else { 0 };
        let stats_avail = after_spark.saturating_sub(bar_overhead);
        let effective_cw = col_widths.with_budget(stats_avail, show_drp, show_dup);
        let (addr_gap, gap) = pick_gaps(stats_avail + 3, &effective_cw, show_drp, show_dup);

        if let Some((hdr_area, rule_area)) = col_keys {
            let anim_state = if state.scale_anim.is_some() { Some(state) } else { None };
            let (inline_range_label, inline_range_spans) = if bar_w > 0 { widgets::inline_range_key(anim_state, shared_scale, bar_w, args.ascii, &args.theme) } else { (String::new(), vec![]) };
            let pfx = widgets::KeysPrefix {
                lead: 3, probe: badge_w, name: name_part_w, addr: col_widths.label,
                trailer: addr_gap, resolve: ip_changes_slot_w, status: STATUS_BADGE_W,
                pings_gap: if circles_w > 0 { 1 } else { 0 }, pings: circles_w,
                inline_range_w: if bar_w > 0 { 1 + bar_w } else { 0 }, inline_range_label, inline_range_spans,
            };
            let hdr = widgets::build_stats_keys_line(&pfx, &effective_cw, gap, true, show_drp, show_dup, args.ascii, &args.theme);
            widgets::render_col_key_rule(frame, hdr, hdr_area, rule_area, args.ascii, &args.theme);
        }

        if !show_headers { return; }

        let mut post_badge = widgets::build_current_rtt_spans(state, &effective_cw, &args.theme);
        if circles_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(widgets::build_target_sparkline_spans(state, args, circles_w, shared_scale));
        }
        if bar_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(trim_range_bar(widgets::build_range_bar_spans(state, args.ascii, shared_scale, &args.theme, bar_w, col_keys.is_some())));
        }
        let combined = widgets::build_combined_row_line(
            state, args, mode_label, &effective_cw, args.theme.hostname,
            shared_scale, tick, log_fmt,
            global_mode, badge_pad_w, ip_changes_slot_w, None, show_drp, show_dup,
            false, post_badge, false, true, gap, addr_gap,
        );
        if show_headers && !chunks.is_empty() {
            let ac = accent_color(state, args, Color::DarkGray);
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(chunks[0]);
            let bl = Line::from(Span::styled(border_ch, Style::default().fg(ac)));
            frame.render_widget(Paragraph::new(bl), horiz[0]);
            let mut spans = combined.spans;
            if spans.first().map(|sp| sp.content.as_ref()) == Some("  ") {
                let trend = state.mtr_trend(args.graph_interval);
                spans[0] = Span::raw(" ");
                spans.insert(0, widgets::trend_spark_span(trend, args.ascii, &args.theme));
            }
            let content_w_actual = horiz[1].width as usize;
            frame.render_widget(Paragraph::new(truncate_line(Line::from(spans), content_w_actual)), horiz[1]);
        }
        return;
    }

    let global_mode: Option<&str> = if mode_labels.iter().all(|m| m.as_str() == "icmp") {
        Some("icmp")
    } else {
        None
    };
    let show_badges       = mode_badge_visible(args.column_vis.mode, mode_labels);
    let badge_pad_w       = mode_labels.iter().map(|m| m.len()).max().unwrap_or(0);
    let max_ip_chg        = states.iter().map(|s| s.ip_changes).max().unwrap_or(0);
    let ip_changes_slot_w = ip_changes_slot_width(args.column_vis.resolve, max_ip_chg);
    let show_drp = true;
    let show_dup = show_dups_any(states, args);
    let terminal_w = col_keys.map(|(a, _)| a.width as usize)
        .or_else(|| chunks.first().map(|c| c.width as usize))
        .unwrap_or(80);
    let border_ch = if args.ascii { "|" } else { "\u{258c}" }; // ▌
    let name_part_w    = if col_widths.name_w > 0 { col_widths.name_w + 2 } else { 0 };
    let badge_overhead = if show_badges { badge_pad_w + 3 } else { 0 };
    let ideal_stats_w  = col_widths.stats_width_with_gap(2, show_drp, show_dup);
    // Budget after fixed prefix for this layout mode.
    let content_w_row = terminal_w.saturating_sub(1); // accent border
    let prefix_w_base = 2 + badge_overhead + name_part_w + col_widths.label + ip_changes_slot_w;
    let prefix_w      = prefix_w_base + 3;
    let extra = if one_row {
        content_w_row.saturating_sub(prefix_w)
    } else {
        content_w_row
    };
    let show_recent = args.extra_stats.contains(&crate::cli::ExtraStat::Recent);
    let show_bar    = args.extra_stats.contains(&crate::cli::ExtraStat::Bar);
    let bar_reserve = if show_bar { 1 + BAR_MIN } else { 0 };
    let space_for_spark = extra.saturating_sub(ideal_stats_w + bar_reserve);
    let circles_w = if show_recent { let b = space_for_spark.saturating_sub(1); if b >= widgets::TARGET_SPARK_MIN { b.min(widgets::TARGET_SPARK_W) } else { 0 } } else { 0 };
    let spark_overhead = if circles_w > 0 { 1 + circles_w } else { 0 };
    let after_spark = extra.saturating_sub(spark_overhead);
    let bar_w = if !show_bar { 0 } else if after_spark > ideal_stats_w { (after_spark - ideal_stats_w).clamp(BAR_MIN, BAR_MAX) } else { BAR_MIN };
    let bar_overhead = if bar_w > 0 { 1 + bar_w } else { 0 };
    let stats_avail = after_spark.saturating_sub(bar_overhead);
    let effective_cw = col_widths.with_budget(stats_avail, show_drp, show_dup);
    let (addr_gap, gap) = if one_row {
        pick_gaps(stats_avail + 3, &effective_cw, show_drp, show_dup)
    } else {
        let g = if effective_cw.stats_width_with_gap(2, show_drp, show_dup) <= stats_avail { 2 } else { 1 };
        (3, g)
    };

    if let Some((hdr_area, rule_area)) = col_keys {
        let anim_state = states.iter().find(|s| s.scale_anim.is_some());
        let (inline_range_label, inline_range_spans) = widgets::inline_range_key(anim_state, shared_scale, bar_w, args.ascii, &args.theme);
        let pfx = if one_row {
            widgets::KeysPrefix {
                lead: 3, probe: badge_overhead, name: name_part_w, addr: col_widths.label,
                trailer: addr_gap, resolve: ip_changes_slot_w, status: STATUS_BADGE_W,
                pings_gap: if circles_w > 0 { 1 } else { 0 }, pings: circles_w,
                inline_range_w: if bar_w > 0 { 1 + bar_w } else { 0 }, inline_range_label, inline_range_spans,
            }
        } else {
            widgets::KeysPrefix {
                lead: 0, probe: 0, name: 0, addr: 0,
                trailer: 0, resolve: 0, status: STATUS_BADGE_W,
                pings_gap: if circles_w > 0 { 1 } else { 0 }, pings: circles_w,
                inline_range_w: if bar_w > 0 { 1 + bar_w } else { 0 }, inline_range_label, inline_range_spans,
            }
        };
        let hdr = widgets::build_stats_keys_line(&pfx, &effective_cw, gap, true, show_drp, show_dup, args.ascii, &args.theme);
        widgets::render_col_key_rule(frame, hdr, hdr_area, rule_area, args.ascii, &args.theme);
    }

    if !show_headers { return; }

    for (row, &slot) in sort_order.iter().enumerate() {
        let state = &states[slot];
        let (cr, cg, cb) = args.theme.target_color(slot);
        let color = Color::Rgb(cr, cg, cb);

        let mut post_badge = widgets::build_current_rtt_spans(state, &effective_cw, &args.theme);
        if circles_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(widgets::build_target_sparkline_spans(state, args, circles_w, shared_scale));
        }
        if bar_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(trim_range_bar(widgets::build_range_bar_spans(state, args.ascii, shared_scale, &args.theme, bar_w, col_keys.is_some())));
        }

        if one_row {
            let sort_arrow = sort_arrows.get(slot).and_then(|&opt| opt).and_then(|(t, up)| {
                if t.elapsed().as_secs() < crate::constants::SORT_ARROW_SECS { Some(up) } else { None }
            });
            let line = widgets::build_combined_row_line(
                state, args, &mode_labels[slot], &effective_cw, color,
                shared_scale, tick, if row == 0 { log_fmt } else { "" },
                global_mode, badge_pad_w, ip_changes_slot_w, sort_arrow, show_drp, show_dup,
                false, post_badge, false, true, gap, addr_gap,
            );
            let ac = accent_color(state, args, Color::Rgb(cr, cg, cb));
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(chunks[row]);
            let bl = Line::from(Span::styled(border_ch, Style::default().fg(ac)));
            frame.render_widget(Paragraph::new(bl), horiz[0]);
            let mut spans = line.spans;
            if spans.first().map(|sp| sp.content.as_ref()) == Some("  ") {
                let trend = state.mtr_trend(args.graph_interval);
                spans[0] = Span::raw(" ");
                spans.insert(0, widgets::trend_spark_span(trend, args.ascii, &args.theme));
            }
            let content_w_actual = horiz[1].width as usize;
            frame.render_widget(Paragraph::new(truncate_line(Line::from(spans), content_w_actual)), horiz[1]);
        } else {
            let ac = accent_color(state, args, Color::Rgb(cr, cg, cb));

            // ── Header row ────────────────────────────────────────────────────────
            let hdr_horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(chunks[row * 2]);
            frame.render_widget(Paragraph::new(Line::from(Span::styled(border_ch, Style::default().fg(ac)))), hdr_horiz[0]);
            let header = widgets::build_header_line(
                state, args, true, &mode_labels[slot],
                if row == 0 { log_fmt } else { "" }, tick,
                Some(color), show_badges, badge_pad_w, hdr_horiz[1].width, None, false,
            );
            let mut hdr_spans = header.spans;
            if hdr_spans.first().map(|sp| sp.content.as_ref()) == Some("  ") {
                let trend = state.mtr_trend(args.graph_interval);
                hdr_spans[0] = Span::raw(" ");
                hdr_spans.insert(0, widgets::trend_spark_span(trend, args.ascii, &args.theme));
            }
            frame.render_widget(Paragraph::new(truncate_line(Line::from(hdr_spans), hdr_horiz[1].width as usize)), hdr_horiz[1]);

            // ── Stats row ─────────────────────────────────────────────────────────
            let stats_horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(chunks[row * 2 + 1]);
            frame.render_widget(Paragraph::new(Line::from(Span::styled(border_ch, Style::default().fg(ac)))), stats_horiz[0]);
            let stats_line = {
                let mut spans: Vec<Span<'static>> = Vec::new();
                spans.extend(widgets::build_status_badge_spans(state, tick, &args.theme, args.ascii));
                spans.extend(post_badge);
                let tail = widgets::build_stats_line(state, args.ascii, args.is_window(), shared_scale, &effective_cw, false, false, false, &args.theme, show_drp, show_dup, tick, gap, false, args.interval);
                spans.extend(tail.spans);
                Line::from(spans)
            };
            frame.render_widget(Paragraph::new(truncate_line(stats_line, stats_horiz[1].width as usize)), stats_horiz[1]);
        }
    }
}

/// Per-column width info for an RTT-scale stat, supporting optional decimal alignment.
/// When `frac_w > 0` the column is rendered with the decimal point pinned at a fixed
/// column (integer part right-aligned in `int_w`, fractional part left-aligned in
/// `frac_w`).  When `frac_w == 0` the value is right-aligned compactly in `compact`.
#[derive(Clone, Debug, PartialEq)]
pub struct RttColWidth {
    pub compact: usize,  // max fmt_rtt().len() across all targets
    pub int_w:   usize,  // max integer-part character count
    pub frac_w:  usize,  // max fractional-part width (includes '.'); 0 = compact mode
}

impl RttColWidth {
    /// Total rendered width under the current alignment mode.
    pub fn active_w(&self) -> usize {
        if self.frac_w > 0 { self.int_w + self.frac_w } else { self.compact }
    }
}

#[derive(Clone, PartialEq)]
pub struct ColWidths {
    pub name_w:        usize,  // max custom label width; 0 = no targets have custom labels (column hidden)
    pub label:         usize,  // address/host column width
    pub rtt:           RttColWidth,
    pub jitter:        RttColWidth,  // default column: mean jitter
    pub drp:           usize,  // width of "N/D" drop ratio string
    pub dup:           usize,  // width of formatted dup count
    pub range_compact: usize,  // max fmt_rtt_nodec() width for min/max values
    // Optional extra stat columns (None = not requested via --columns)
    pub mtr:    Option<RttColWidth>,
    pub std:    Option<RttColWidth>,
    pub p01:    Option<RttColWidth>,
    pub p10:    Option<RttColWidth>,
    pub p50:    Option<RttColWidth>,
    pub p95:    Option<RttColWidth>,
    pub p99:    Option<RttColWidth>,
    pub cv:     Option<usize>,      // max width of fmt_cv() string, e.g. "12.3%"
    pub srtt:   Option<RttColWidth>,
    pub streak: Option<usize>,      // max digit count of cur_drop_streak
    pub last:   Option<usize>,      // max width of fmt_last_up() string, e.g. "12m"
    pub status: Option<usize>,      // max width of probe_status_text(), e.g. "sent 31 probes in 30.3s"
    pub stat_order: Vec<ExtraStat>, // display order of active extra stat columns
    pub hidden_base_stats: Vec<BaseStat>, // base stats the user has hidden at runtime
}

impl ColWidths {
    /// Total character width of the stats section with a given inter-item gap size.
    /// gap=1 → single space between items; gap=2 → double space.
    /// Each item is: gap + sym(1) + value.  Range is: gap + sym(1) + min + "-"(1) + max.
    /// drp: always shown; gap + sym(1) + ratio(drp).
    /// dup: conditional; gap + sym(1) + count + " "(1) + pct(4).
    pub fn stats_width_with_gap(&self, gap: usize, show_drp: bool, show_dup: bool) -> usize {
        let r   = self.rtt.active_w();
        let j   = self.jitter.active_w();
        let rng = self.range_compact;
        let hide = |b: BaseStat| self.hidden_base_stats.contains(&b);
        let base = STATUS_BADGE_W + r
            + if hide(BaseStat::Avg)    { 0 } else { r   + gap + 1 }
            + if hide(BaseStat::Range)  { 0 } else { 2*rng + gap + 2 }
            + if hide(BaseStat::Jitter) { 0 } else { j   + gap + 1 };
        let extras = self.mtr.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.std.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.p01.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.p10.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.p50.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.p95.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.p99.as_ref().map_or(0,    |c| c.active_w() + gap + 1)
            + self.cv.map_or(0,              |w| w + gap + 1)
            + self.srtt.as_ref().map_or(0,   |c| c.active_w() + gap + 1)
            + self.streak.map_or(0,          |w| w + gap + 1)
            + self.last.map_or(0,            |w| w + gap + 1)
            + self.status.map_or(0,          |w| w + gap + 1);
        let show_drops = show_drp && !hide(BaseStat::Drops);
        base + extras
            + if show_drops { self.drp + gap + 1 } else { 0 }
            + if show_dup   { self.dup + gap + 6 } else { 0 }
    }

    /// Stats width with single-space gaps (used for decimal-alignment budget trimming).
    pub fn stats_width(&self, show_drp: bool, show_dup: bool) -> usize {
        self.stats_width_with_gap(1, show_drp, show_dup)
    }

    /// Return a clone with decimal alignment dropped on rightmost columns first
    /// until the stats section fits within `budget` characters.
    /// Strips extra stat columns in reverse display order, then jitter, then rtt.
    pub fn with_budget(&self, budget: usize, show_drp: bool, show_dup: bool) -> ColWidths {
        let mut cw = self.clone();
        loop {
            if cw.stats_width(show_drp, show_dup) <= budget { break; }
            // Strip optional extras right-to-left (reverse of display order), then base cols.
            let order = cw.stat_order.clone();
            let mut stripped = false;
            for stat in order.iter().rev() {
                match stat {
                    ExtraStat::Mtr  => if let Some(ref mut c) = cw.mtr  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::Std  => if let Some(ref mut c) = cw.std  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::P01  => if let Some(ref mut c) = cw.p01  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::P10  => if let Some(ref mut c) = cw.p10  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::P50  => if let Some(ref mut c) = cw.p50  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::P95  => if let Some(ref mut c) = cw.p95  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::P99  => if let Some(ref mut c) = cw.p99  { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    ExtraStat::Srtt => if let Some(ref mut c) = cw.srtt { if c.frac_w > 0 { c.frac_w = 0; stripped = true; break; } }
                    _ => {} // Cv and Streak have no frac_w; All/Default/None never appear
                }
            }
            if stripped { continue; }
            if cw.jitter.frac_w > 0 { cw.jitter.frac_w = 0; continue; }
            if cw.rtt.frac_w    > 0 { cw.rtt.frac_w    = 0; continue; }
            break;
        }
        cw
    }
}

fn rtt_split(s: &str) -> (usize, usize) {
    if let Some(dot) = s.find('.') { (dot, s.len() - dot) } else { (s.len(), 0) }
}

/// Width of the ip-changes (resolve counter) slot, honouring a forced visibility
/// override from --columns / the 'x' dialog. 0 = column absent. Minimum content
/// width of 4 keeps the "res" column-key label from colliding with "up".
pub fn ip_changes_slot_width(vis: Option<bool>, max_chg: u32) -> usize {
    let w = 2 + max_chg.to_string().len().max(2);
    match vis {
        Some(false) => 0,
        Some(true)  => w,
        None        => if max_chg > 1 { w } else { 0 },
    }
}

/// Effective visibility of the mode badge column given the forced override and
/// the automatic rule (badges shown when any target's probe type is not icmp).
pub fn mode_badge_visible(vis: Option<bool>, mode_labels: &[String]) -> bool {
    vis.unwrap_or_else(|| !mode_labels.iter().all(|m| m == "icmp"))
}

/// Returns a bitmask (one bit per dialog row index 0–22) where a set bit means
/// the column is enabled but not currently rendered because the terminal is too narrow.
/// Bits 0–4 (identity) are always 0 after the min_size check.
/// Bits 5–8 = base stats; 9–19 = extra numerical stats (18 = streak, 19 = last).
/// Bit 20 = recent sparkline (set when `show_recent && circles_w == 0`).
/// Bit 21 = bar (always 0; bar gets its own allocation before `stats_avail`).
/// Bit 22 = status (the probe/uptime summary text column - appended last in
/// `EXTRA_STAT_ALL` so it doesn't renumber 20/21 above).
///
/// Pass `effective_cw` (after `with_budget`) and the same `gap` and `show_*` flags
/// used when rendering, so the simulation matches the actual render path.
pub fn compute_space_hidden(
    cw:          &ColWidths,
    show_recent: bool,
    circles_w:   usize,
    stats_avail: usize,
    gap:         usize,
    show_drp:    bool,
    show_dup:    bool,
) -> u32 {
    let mut mask = 0u32;
    let hide_b = |b: &BaseStat| cw.hidden_base_stats.contains(b);
    // Simulate left-to-right accumulation in actual rendering order:
    // status_badge + current_rtt → avg → range → jitter → drops → dup → extras.
    let mut used = STATUS_BADGE_W + cw.rtt.active_w();
    macro_rules! check {
        ($bit:expr, $w:expr) => {{
            let w = $w;
            if used + w > stats_avail { mask |= 1 << $bit; }
            used += w;
        }};
    }
    if !hide_b(&BaseStat::Avg)    { check!(5u32, gap + 1 + cw.rtt.active_w()); }
    if !hide_b(&BaseStat::Range)  { check!(6u32, gap + 2 + 2 * cw.range_compact); }
    if !hide_b(&BaseStat::Jitter) { check!(7u32, gap + 1 + cw.jitter.active_w()); }
    if show_drp && !hide_b(&BaseStat::Drops) { check!(8u32, gap + 1 + cw.drp); }
    if show_dup { used += gap + 6 + cw.dup; } // dup has no dialog row but consumes space
    for stat in &cw.stat_order {
        let (bit, w_opt): (u32, Option<usize>) = match stat {
            ExtraStat::Mtr    => (9,  cw.mtr.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::Std    => (10, cw.std.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::P01    => (11, cw.p01.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::P10    => (12, cw.p10.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::P50    => (13, cw.p50.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::P95    => (14, cw.p95.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::P99    => (15, cw.p99.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::Cv     => (16, cw.cv.map(|w| gap + 1 + w)),
            ExtraStat::Srtt   => (17, cw.srtt.as_ref().map(|c| gap + 1 + c.active_w())),
            ExtraStat::Streak => (18, cw.streak.map(|w| gap + 1 + w)),
            ExtraStat::Last   => (19, cw.last.map(|w| gap + 1 + w)),
            ExtraStat::Status => (22, cw.status.map(|w| gap + 1 + w)),
            _ => continue,
        };
        if let Some(w) = w_opt {
            check!(bit, w);
        }
    }
    if show_recent && circles_w == 0 { mask |= 1 << 20; }
    mask
}

/// Returns a bitmask (one bit per dialog row index 0–20) where a set bit means
/// the column is enabled but currently has no data to display across all targets.
/// Bit 4 (resolve): set when the auto-show condition is not met (ip_changes <= 1 for all).
/// Bits 5–7 = avg/range/jitter (set when the window is empty).
/// Bit 8 = drops (set when no drops have occurred in the rolling window).
/// Bits 9–19 = extra numerical stats (stat-specific checks; 19 = last).
/// Bits 20–21 = recent/bar (set when no target has any history).
pub fn compute_no_data(states: &[TargetState], extras: &[ExtraStat], hidden_base: &[BaseStat]) -> u32 {
    if states.is_empty() { return 0; }
    let mut mask = 0u32;
    let want = |e: &ExtraStat| extras.contains(e);
    let hide_b = |b: &BaseStat| hidden_base.contains(b);

    let any_window   = states.iter().any(|s| !s.window.is_empty());
    let any_window2  = states.iter().any(|s| s.window.len() >= 2);
    let any_history  = states.iter().any(|s| !s.history.is_empty());
    let any_mtr      = states.iter().any(|s| s.win_mtr().is_some());
    let any_srtt     = states.iter().any(|s| s.srtt > 0.0);
    let any_cv       = states.iter().any(|s| s.win_cv() > 0.0);
    let any_last     = states.iter().any(|s| s.last_up.is_some());
    // Use > 1 to match the auto-show condition in ip_changes_slot_width / identity_column_states.
    let any_resolve  = states.iter().any(|s| s.ip_changes > 1);
    let any_drops    = states.iter().any(|s| s.win_drops > 0);

    // Bit 4: resolve auto-show condition not met (ip_changes <= 1 for all targets).
    // Shown regardless of the enabled state so the Ø appears on the auto-hidden row.
    if !any_resolve { mask |= 1 << 4; }

    if !hide_b(&BaseStat::Avg)    && !any_window  { mask |= 1 << 5; }
    if !hide_b(&BaseStat::Range)  && !any_window  { mask |= 1 << 6; }
    if !hide_b(&BaseStat::Jitter) && !any_window  { mask |= 1 << 7; }
    // Drops: "no data" means no drops in the rolling window, not just window-empty.
    if !hide_b(&BaseStat::Drops)  && !any_drops   { mask |= 1 << 8; }

    if want(&ExtraStat::Mtr)    && !any_mtr     { mask |= 1 <<  9; }
    if want(&ExtraStat::Std)    && !any_window2 { mask |= 1 << 10; }
    let any_life_rtts = states.iter().any(|s| !s.lifetime_rtts.is_empty());
    if want(&ExtraStat::P01)    && !any_window && !any_life_rtts { mask |= 1 << 11; }
    if want(&ExtraStat::P10)    && !any_window && !any_life_rtts { mask |= 1 << 12; }
    if want(&ExtraStat::P50)    && !any_window && !any_life_rtts { mask |= 1 << 13; }
    if want(&ExtraStat::P95)    && !any_window && !any_life_rtts { mask |= 1 << 14; }
    if want(&ExtraStat::P99)    && !any_window && !any_life_rtts { mask |= 1 << 15; }
    if want(&ExtraStat::Cv)     && !any_cv      { mask |= 1 << 16; }
    if want(&ExtraStat::Srtt)   && !any_srtt    { mask |= 1 << 17; }
    // streak (bit 18) always has data — "0" is a valid reading
    if want(&ExtraStat::Last)   && !any_last    { mask |= 1 << 19; }

    if want(&ExtraStat::Recent) && !any_history { mask |= 1 << 20; }
    if want(&ExtraStat::Bar)    && !any_history { mask |= 1 << 21; }

    mask
}

pub fn compute_col_widths(states: &[TargetState], stats_window: bool, extras: &[ExtraStat], hidden_base: &[BaseStat], prefer_v6: bool, vis: &crate::cli::ColumnVis, interval_ms: u64) -> ColWidths {
    let want = |e: &ExtraStat| extras.contains(e);
    let show_name = vis.name != Some(false);
    let show_addr = vis.addr != Some(false);
    let mut name_w  = 0usize;
    let mut label_w = 0usize;
    // rtt_c minimum of 4 ensures the "DROP" placeholder (4 chars) always fits.
    let (mut rtt_c, mut rtt_i, mut rtt_f) = (4usize, 1usize, 0usize);
    let (mut jit_c, mut jit_i, mut jit_f) = (3usize, 1usize, 0usize);
    let mut drp_w   = 3usize;  // minimum: "0/0"
    let mut dup_w   = 1usize;
    let mut range_c = 1usize;  // min/max shown without decimals
    let (mut mtr_c, mut mtr_i, mut mtr_f) = (3usize, 1usize, 0usize);
    let (mut std_c, mut std_i, mut std_f) = (3usize, 1usize, 0usize);
    let (mut p01_c, mut p01_i, mut p01_f) = (3usize, 1usize, 0usize);
    let (mut p10_c, mut p10_i, mut p10_f) = (3usize, 1usize, 0usize);
    let (mut p50_c, mut p50_i, mut p50_f) = (3usize, 1usize, 0usize);
    let (mut p95_c, mut p95_i, mut p95_f) = (3usize, 1usize, 0usize);
    let (mut p99_c, mut p99_i, mut p99_f) = (3usize, 1usize, 0usize);
    let (mut srt_c, mut srt_i, mut srt_f) = (3usize, 1usize, 0usize);
    let mut cv_w     = 4usize;   // min: "0.0%"
    let mut streak_w = 1usize;
    let mut last_w   = 1usize;   // min: "~" placeholder
    let mut status_w = 1usize;
    let now = Instant::now();
    for s in states {
        // name column: custom label, or hostname (non-IP host string), or blank for pure IP targets
        let name_len = if !show_name {
            0              // column hidden via --columns / 'x' dialog
        } else if s.custom_label {
            s.label.len()
        } else if s.host.parse::<std::net::IpAddr>().is_err() {
            s.host.len()   // it's a hostname, not a raw IP
        } else {
            0              // pure IP literal - no separate name
        };
        name_w = name_w.max(name_len);
        // address column: resolved IP; while resolving, reserve space for the placeholder
        if show_addr {
            let addr_len = s.current_ip.map(|ip| ip.to_string().len()).unwrap_or_else(|| {
                let ph_w = if s.resolving {
                    if prefer_v6 { "?:?:?:?:?:?:?:?".len() } else { "?.?.?.?".len() }
                } else { 0 };
                s.host.len().max(ph_w)
            });
            label_w = label_w.max(addr_len);
        }
        if s.waiting { continue; }
        // rtt column: current RTT and avg only (range uses range_compact)
        let rtt_only: [f64; 2] = if stats_window {
            [s.last_rtt, s.win_avg()]
        } else {
            [s.last_rtt, s.avg_latency()]
        };
        for val in rtt_only {
            let sv = fmt_rtt(val);
            rtt_c = rtt_c.max(sv.len());
            let (i, f) = rtt_split(&sv);
            rtt_i = rtt_i.max(i); rtt_f = rtt_f.max(f);
        }
        // range column: min/max without decimals
        let min_v = if stats_window { s.win_min() } else { s.life_min() };
        let max_v = if stats_window { s.win_max() } else { s.life_max() };
        range_c = range_c.max(fmt_rtt_nodec(min_v).len()).max(fmt_rtt_nodec(max_v).len());
        let mtr_val = if stats_window { s.win_mtr() } else { s.life_mtr() };
        if let Some(m) = mtr_val {
            let sv = fmt_rtt(m);
            mtr_c = mtr_c.max(sv.len());
            let (i, f) = rtt_split(&sv);
            mtr_i = mtr_i.max(i); mtr_f = mtr_f.max(f);
        }
        {
            let sv = if stats_window { fmt_rtt(s.win_stddev()) } else { fmt_rtt(s.life_stddev()) };
            std_c = std_c.max(sv.len());
            let (i, f) = rtt_split(&sv);
            std_i = std_i.max(i); std_f = std_f.max(f);
        }
        let drop_n = if stats_window { s.win_drops as u64 } else { s.drops as u64 };
        let drop_d = if stats_window { (s.win_drops as usize + s.window.len()) as u64 } else { s.total_sent };
        drp_w = drp_w.max(fmt_count(drop_n).len() + 1 + fmt_count(drop_d).len());
        dup_w = dup_w.max(fmt_count(if stats_window { s.win_dups  as u64 } else { s.dups  as u64 }).len());

        // Jitter: always a default column
        macro_rules! upd { ($sv:expr, $c:expr, $i:expr, $f:expr) => {{
            let sv = $sv; $c = $c.max(sv.len());
            let (ii, ff) = rtt_split(&sv); $i = $i.max(ii); $f = $f.max(ff);
        }}}
        {
            let v = if stats_window { s.win_jitter_avg() } else { s.avg_jitter() };
            upd!(fmt_rtt(v), jit_c, jit_i, jit_f);
        }

        // Optional extra stat column widths
        if want(&ExtraStat::Mtr) {
            let mtr_v = if stats_window { s.win_mtr() } else { s.life_mtr() };
            if let Some(m) = mtr_v { upd!(fmt_rtt(m), mtr_c, mtr_i, mtr_f); }
        }
        if want(&ExtraStat::Std) {
            let sv = if stats_window { fmt_rtt(s.win_stddev()) } else { fmt_rtt(s.life_stddev()) };
            upd!(sv, std_c, std_i, std_f);
        }
        if want(&ExtraStat::P01) {
            if stats_window && !s.window.is_empty() { upd!(fmt_rtt(s.win_p01()),    p01_c, p01_i, p01_f); }
            else if !stats_window && !s.lifetime_rtts.is_empty() { upd!(fmt_rtt(s.life_p01()),  p01_c, p01_i, p01_f); }
        }
        if want(&ExtraStat::P10) {
            if stats_window && !s.window.is_empty() { upd!(fmt_rtt(s.win_p10()),    p10_c, p10_i, p10_f); }
            else if !stats_window && !s.lifetime_rtts.is_empty() { upd!(fmt_rtt(s.life_p10()),  p10_c, p10_i, p10_f); }
        }
        if want(&ExtraStat::P50) {
            if stats_window && !s.window.is_empty() { upd!(fmt_rtt(s.win_median()), p50_c, p50_i, p50_f); }
            else if !stats_window && !s.lifetime_rtts.is_empty() { upd!(fmt_rtt(s.life_median()), p50_c, p50_i, p50_f); }
        }
        if want(&ExtraStat::P95) {
            if stats_window && !s.window.is_empty() { upd!(fmt_rtt(s.win_p95()),    p95_c, p95_i, p95_f); }
            else if !stats_window && !s.lifetime_rtts.is_empty() { upd!(fmt_rtt(s.life_p95()),  p95_c, p95_i, p95_f); }
        }
        if want(&ExtraStat::P99) {
            if stats_window && !s.window.is_empty() { upd!(fmt_rtt(s.win_p99()),    p99_c, p99_i, p99_f); }
            else if !stats_window && !s.lifetime_rtts.is_empty() { upd!(fmt_rtt(s.life_p99()),  p99_c, p99_i, p99_f); }
        }
        if want(&ExtraStat::Srtt) && s.srtt > 0.0 {
            upd!(fmt_rtt(s.srtt), srt_c, srt_i, srt_f);
        }
        if want(&ExtraStat::Cv) {
            let cv = if stats_window { s.win_cv() } else { s.life_cv() };
            cv_w = cv_w.max(fmt_cv(cv).len());
        }
        if want(&ExtraStat::Streak) {
            streak_w = streak_w.max(fmt_count(s.cur_drop_streak as u64).len());
        }
        if want(&ExtraStat::Last) {
            last_w = last_w.max(fmt_last_up(s.last_up, now).len());
        }
        if want(&ExtraStat::Status) {
            status_w = status_w.max(probe_status_text(s, now, interval_ms).chars().count());
        }
    }
    ColWidths {
        name_w,
        label:  label_w,
        rtt:    RttColWidth { compact: rtt_c, int_w: rtt_i, frac_w: rtt_f },
        jitter: RttColWidth { compact: jit_c, int_w: jit_i, frac_w: jit_f },
        drp:    drp_w,
        dup:    dup_w,
        range_compact: range_c,
        mtr:        if want(&ExtraStat::Mtr)    { Some(RttColWidth { compact: mtr_c, int_w: mtr_i, frac_w: mtr_f }) } else { None },
        std:        if want(&ExtraStat::Std)    { Some(RttColWidth { compact: std_c, int_w: std_i, frac_w: std_f }) } else { None },
        p01:        if want(&ExtraStat::P01)    { Some(RttColWidth { compact: p01_c, int_w: p01_i, frac_w: p01_f }) } else { None },
        p10:        if want(&ExtraStat::P10)    { Some(RttColWidth { compact: p10_c, int_w: p10_i, frac_w: p10_f }) } else { None },
        p50:        if want(&ExtraStat::P50)    { Some(RttColWidth { compact: p50_c, int_w: p50_i, frac_w: p50_f }) } else { None },
        p95:        if want(&ExtraStat::P95)    { Some(RttColWidth { compact: p95_c, int_w: p95_i, frac_w: p95_f }) } else { None },
        p99:        if want(&ExtraStat::P99)    { Some(RttColWidth { compact: p99_c, int_w: p99_i, frac_w: p99_f }) } else { None },
        cv:         if want(&ExtraStat::Cv)     { Some(cv_w) }     else { None },
        srtt:       if want(&ExtraStat::Srtt)   { Some(RttColWidth { compact: srt_c, int_w: srt_i, frac_w: srt_f }) } else { None },
        streak:     if want(&ExtraStat::Streak) { Some(streak_w) } else { None },
        last:       if want(&ExtraStat::Last)   { Some(last_w) }   else { None },
        status:     if want(&ExtraStat::Status) { Some(status_w) } else { None },
        stat_order: extras.to_vec(),
        hidden_base_stats: hidden_base.to_vec(),
    }
}

/// Returns the minimum terminal (width, height) needed to usefully render a given view.
/// `view` is one of "list", "single", "graph", "worm", "radar", "ekg".
/// Width: enough to show accent-border + trend + badge + name + addr + spinner + UP/DN badge + RTT.
/// Height: enough for all headers/col-keys plus a useful screensaver/graph area.
pub fn min_size(
    view: &str,
    n: usize,
    show_col_keys: bool,
    show_headers: bool,
    cw: &ColWidths,
    mode_labels: &[String],
    mode_vis: Option<bool>,
) -> (u16, u16) {
    let n16 = n as u16;
    let col_keys_h: u16 = if show_col_keys { 2 } else { 0 };
    // graph view always uses 1 row per target in its header; screensavers use 2 rows on narrow terminals
    let graph_rpt: u16 = if show_headers { 1 } else { 0 };
    let saver_rpt: u16 = if !show_headers { 0 } else if n <= 1 { 1 } else { 2 };

    let min_h: u16 = match view {
        "list"  => n16 + col_keys_h,
        "single" => 3, // blank spacer + combined name/address+status line + stats line (history rows self-adjust)
        "graph" => col_keys_h + n16 * graph_rpt + 6,
        "worm"  => col_keys_h + n16 * saver_rpt + 4,
        "radar" => col_keys_h + n16 * saver_rpt + 4,
        "pong"  => col_keys_h + n16 * saver_rpt + 4,
        "ekg"   => col_keys_h + n16 * saver_rpt + n16.max(2),
        "bars"    => col_keys_h + n16 * saver_rpt + 6,  // 4 chart rows + 2 footer rows
        "cards"   => cards::MIN_PANEL_H,              // grid clips; one panel tall is enough
        "scatter" => col_keys_h + n16 * saver_rpt + 6, // 2 axis rows + 4 data rows minimum
        _         => 4,
    };

    let badge_w = if !mode_badge_visible(mode_vis, mode_labels) { 0 }
                  else { mode_labels.iter().map(|m| m.len()).max().unwrap_or(0) + 3 };
    let name_part_w = if cw.name_w > 0 { cw.name_w + 2 } else { 0 };
    let base_min_w = (1 + 2 + badge_w + name_part_w + cw.label + 3 + STATUS_BADGE_W + cw.rtt.active_w()) as u16;
    // Bars needs at least SCALE_W + N*MIN_COL_W columns to be useful.
    let min_w = if view == "bars" {
        base_min_w.max(5 + n16 * 6)
    } else if view == "cards" {
        base_min_w.max(cards::MIN_PANEL_W)
    } else if view == "single" {
        // Name/address header and the up/down badge now share one bottom line
        // (border + arrow-slot + name + addr + gap + badge); the probe/uptime
        // text past that just truncates, so it isn't part of this floor.
        (1 + 2 + name_part_w + cw.label + 2 + STATUS_BADGE_W) as u16
    } else {
        base_min_w
    };

    (min_w, min_h)
}

fn col_widths_max(a: &ColWidths, b: &ColWidths) -> ColWidths {
    fn rmax(a: &RttColWidth, b: &RttColWidth) -> RttColWidth {
        RttColWidth { compact: a.compact.max(b.compact), int_w: a.int_w.max(b.int_w), frac_w: a.frac_w.max(b.frac_w) }
    }
    fn ormax(a: &Option<RttColWidth>, b: &Option<RttColWidth>) -> Option<RttColWidth> {
        match (a, b) { (Some(x), Some(y)) => Some(rmax(x, y)), (x, y) => x.as_ref().or(y.as_ref()).cloned() }
    }
    ColWidths {
        name_w:        a.name_w.max(b.name_w),
        label:         a.label.max(b.label),
        rtt:           rmax(&a.rtt, &b.rtt),
        jitter:        rmax(&a.jitter, &b.jitter),
        drp:           a.drp.max(b.drp),
        dup:           a.dup.max(b.dup),
        range_compact: a.range_compact.max(b.range_compact),
        mtr:           ormax(&a.mtr, &b.mtr),
        std:           ormax(&a.std, &b.std),
        p01:           ormax(&a.p01, &b.p01),
        p10:           ormax(&a.p10, &b.p10),
        p50:           ormax(&a.p50, &b.p50),
        p95:           ormax(&a.p95, &b.p95),
        p99:           ormax(&a.p99, &b.p99),
        cv:            match (a.cv, b.cv) { (Some(x), Some(y)) => Some(x.max(y)), (x, y) => x.or(y) },
        srtt:          ormax(&a.srtt, &b.srtt),
        streak:        match (a.streak, b.streak) { (Some(x), Some(y)) => Some(x.max(y)), (x, y) => x.or(y) },
        last:          match (a.last, b.last) { (Some(x), Some(y)) => Some(x.max(y)), (x, y) => x.or(y) },
        status:        match (a.status, b.status) { (Some(x), Some(y)) => Some(x.max(y)), (x, y) => x.or(y) },
        // Visibility/order reflect the user's current toggle state and shouldn't be
        // held back by the width hysteresis below - only numeric widths get that.
        stat_order:    b.stat_order.clone(),
        hidden_base_stats: b.hidden_base_stats.clone(),
    }
}

/// Wraps `compute_col_widths` with hysteresis: column widths grow immediately when
/// the fresh value requires more space, but only shrink after `SHRINK_AFTER` consecutive
/// frames that all fit within the current stable width.  This prevents the stats columns
/// from jumping every time an RTT value crosses a decimal-precision boundary.
pub struct ColWidthsStabilizer {
    stable:    Option<ColWidths>,
    countdown: u32,
}

const SHRINK_AFTER: u32 = 10;

impl ColWidthsStabilizer {
    pub fn new() -> Self { Self { stable: None, countdown: 0 } }

    pub fn apply(&mut self, fresh: ColWidths) -> ColWidths {
        let Some(ref stable) = self.stable else {
            self.stable = Some(fresh.clone());
            self.countdown = SHRINK_AFTER;
            return fresh;
        };
        let maxed = col_widths_max(stable, &fresh);
        if maxed != *stable {
            // fresh needs more space somewhere - expand immediately and reset countdown
            self.countdown = SHRINK_AFTER;
            self.stable = Some(maxed.clone());
            maxed
        } else {
            // fresh fits within current stable
            if self.countdown > 0 {
                self.countdown -= 1;
                stable.clone()
            } else {
                // countdown expired - shrink to fresh and reset
                self.countdown = SHRINK_AFTER;
                self.stable = Some(fresh.clone());
                fresh
            }
        }
    }
}

pub fn fmt_count(n: u64) -> String {
    if n >= 1_000_000_000 { format!("{}g", n / 1_000_000_000) }
    else if n >= 1_000_000 { format!("{}m", n / 1_000_000) }
    else if n >= 1_000     { format!("{}k", n / 1_000) }
    else                   { n.to_string() }
}

pub fn fmt_rtt(ms: f64) -> String {
    if ms <= 0.0 || ms == f64::MAX || ms == f64::MIN { "~".into() }
    else if ms < 10.0  { format!("{:.2}", ms) }
    else if ms < 100.0 { format!("{:.1}", ms) }
    else               { format!("{:.0}", ms) }
}

pub fn fmt_cv(cv: f64) -> String {
    if cv < 100.0 { format!("{:.1}%", cv) } else { format!("{:.0}%", cv) }
}

/// Compact "time since" string for the last successful response, e.g. "now", "5s", "12m", "3h", "2d".
/// None (no response yet) formats as "~", matching the other stat columns' placeholder.
pub fn fmt_last_up(last: Option<Instant>, now: Instant) -> String {
    match last {
        None => "~".into(),
        Some(t) => {
            let secs = now.saturating_duration_since(t).as_secs();
            if secs < 1             { "now".into() }
            else if secs < 60       { format!("{}s", secs) }
            else if secs < 3_600    { format!("{}m", secs / 60) }
            else if secs < 86_400   { format!("{}h", secs / 3_600) }
            else                    { format!("{}d", secs / 86_400) }
        }
    }
}

/// Human-friendly duration for the probe/uptime summary text: more precision for
/// small values, progressively coarser as the value grows, so a long-running
/// target never reports something silly like "waiting 47m12.0s". At each unit
/// tier, the smaller sub-unit is dropped once the larger unit's count reaches 4 -
/// by then the sub-unit is noise, not information.
fn format_elapsed(elapsed_secs: f64) -> String {
    let secs = elapsed_secs.max(0.0);
    if secs < 10.0 { return format!("{:.1}s", secs); } // sub-precision matters at this scale
    let secs = secs.round() as u64;
    if secs < 60 { return format!("{}s", secs); } // whole seconds - a decimal adds nothing here

    let (mins, secs) = (secs / 60, secs % 60);
    if mins < 60 { return if mins < 4 { format!("{}m{}s", mins, secs) } else { format!("{}m", mins) }; }

    let (hours, mins) = (mins / 60, mins % 60);
    if hours < 24 { return if hours < 4 { format!("{}h{}m", hours, mins) } else { format!("{}h", hours) }; }

    let (days, hours) = (hours / 24, hours % 24);
    if days < 4 { format!("{}d{}h", days, hours) } else { format!("{}d", days) }
}

/// Minimum age (seconds) before a "down"/"last drop" note is worth showing at all -
/// below this it's just noise flickering in and out on every probe, since the
/// event only just happened and the badge elsewhere already reflects it. Scales
/// with the probe interval (a slow poller shouldn't get a callout after what is,
/// for it, a single probe's worth of time) but never drops below 10s (a fast
/// poller doesn't need a callout for something that happened one eyeblink ago).
fn drop_note_threshold_secs(interval_ms: u64) -> f64 {
    (interval_ms as f64 / 1000.0 * 2.0).max(10.0)
}

/// Shared probe/uptime summary text - the single source of the text used by the
/// single-target view's status line and, as the opt-in `status` column, every
/// other view. Excludes the up/down badge itself - callers show that separately.
/// Kept terse (no verbs) since this is a column value, not a sentence; every
/// fragment is comma-separated.
///
/// Before any reply has ever come back: "no reply, N probes, <elapsed>" - there's
/// no last-contact reference point yet, so the phrasing doesn't pretend there is
/// one. Once at least one reply has arrived: "N probes, <elapsed>", plus one of:
/// - ", down <elapsed>" once currently down for at least `drop_note_threshold_secs`
/// - ", last drop <elapsed>" if currently up but has dropped before, again only
///   once that drop is at least `drop_note_threshold_secs` old
pub fn probe_status_text(state: &TargetState, now: Instant, interval_ms: u64) -> String {
    let n_probes = state.history.len();
    let plural = if n_probes == 1 { "" } else { "s" };
    let n_probes_str = fmt_count(n_probes as u64); // abbreviates long-running counts, e.g. "259k"
    let elapsed_secs = state.first_sent_at.map(|t| now.saturating_duration_since(t).as_secs_f64()).unwrap_or(0.0);

    let Some(last_up) = state.last_up else {
        return format!("no reply, {} probe{}, {}", n_probes_str, plural, format_elapsed(elapsed_secs));
    };

    let mut text = format!("{} probe{}, {}", n_probes_str, plural, format_elapsed(elapsed_secs));
    let threshold = drop_note_threshold_secs(interval_ms);
    if state.is_currently_down() {
        let since = now.saturating_duration_since(last_up).as_secs_f64();
        if since >= threshold {
            text.push_str(&format!(", down {}", format_elapsed(since)));
        }
    } else if state.drops > 0 {
        // Currently up, but it has dropped before at some point - worth a note even
        // though we're not down right now, once that drop has aged past the threshold.
        if let Some(t) = state.last_drop_at {
            let since = now.saturating_duration_since(t).as_secs_f64();
            if since >= threshold {
                text.push_str(&format!(", last drop {}", format_elapsed(since)));
            }
        }
    }
    text
}

pub fn fmt_rtt_nodec(ms: f64) -> String {
    if ms <= 0.0 || ms == f64::MAX || ms == f64::MIN { "~".into() }
    else { format!("{:.0}", ms) }
}

/// Returns the display scale to use for all charts.
/// If --max-range is set that value is used directly; otherwise auto-scaled from data
/// with a minimum of 100ms so the initial graph isn't cramped.
pub fn compute_scale(args: &Args, global_max: f64) -> f64 {
    if let Some(fixed) = args.max_range {
        fixed.max(1.0)
    } else {
        scale_bucket(if global_max == f64::MIN { 0.0 } else { global_max })
    }
}

pub fn lerp_rgb(r0: u8, g0: u8, b0: u8, r1: u8, g1: u8, b1: u8, t: f64) -> (u8, u8, u8) {
    let l = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    (l(r0, r1), l(g0, g1), l(b0, b1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TargetState;
    use std::time::Duration;

    // --- format_elapsed ---

    #[test]
    fn format_elapsed_sub_ten_seconds_keeps_a_decimal() {
        assert_eq!(format_elapsed(3.4), "3.4s");
        assert_eq!(format_elapsed(0.0), "0.0s");
        assert_eq!(format_elapsed(9.99), "10.0s"); // rounds up to the next tier's boundary text, still 1 decimal here
    }

    #[test]
    fn format_elapsed_ten_to_sixty_seconds_drops_the_decimal() {
        assert_eq!(format_elapsed(10.0), "10s");
        assert_eq!(format_elapsed(45.6), "46s");
        assert_eq!(format_elapsed(59.4), "59s"); // rounds within the seconds tier
    }

    #[test]
    fn format_elapsed_minutes_under_four_keep_seconds() {
        assert_eq!(format_elapsed(60.0), "1m0s");
        assert_eq!(format_elapsed(135.0), "2m15s");
        assert_eq!(format_elapsed(239.0), "3m59s");
    }

    #[test]
    fn format_elapsed_four_minutes_and_up_drops_seconds() {
        assert_eq!(format_elapsed(240.0), "4m");
        assert_eq!(format_elapsed(299.0), "4m");
        assert_eq!(format_elapsed(3599.0), "59m");
    }

    #[test]
    fn format_elapsed_hours_under_four_keep_minutes() {
        assert_eq!(format_elapsed(3600.0), "1h0m");
        assert_eq!(format_elapsed(3660.0), "1h1m");
        assert_eq!(format_elapsed(3600.0 * 3.5), "3h30m");
    }

    #[test]
    fn format_elapsed_four_hours_and_up_drops_minutes() {
        assert_eq!(format_elapsed(3600.0 * 4.0), "4h");
        assert_eq!(format_elapsed(3600.0 * 23.0), "23h");
    }

    #[test]
    fn format_elapsed_days_under_four_keep_hours() {
        assert_eq!(format_elapsed(86400.0), "1d0h");
        assert_eq!(format_elapsed(86400.0 + 3600.0 * 5.0), "1d5h");
    }

    #[test]
    fn format_elapsed_four_days_and_up_drops_hours() {
        assert_eq!(format_elapsed(86400.0 * 4.0), "4d");
        assert_eq!(format_elapsed(86400.0 * 10.0), "10d");
    }

    // --- drop_note_threshold_secs ---

    #[test]
    fn drop_note_threshold_scales_with_interval_but_floors_at_ten_seconds() {
        assert_eq!(drop_note_threshold_secs(1_000), 10.0);   // 1s interval: 2s < 10s floor
        assert_eq!(drop_note_threshold_secs(3_000), 10.0);   // 3s interval: 6s < 10s floor
        assert_eq!(drop_note_threshold_secs(8_000), 16.0);   // 8s interval: 16s > floor
        assert_eq!(drop_note_threshold_secs(30_000), 60.0);  // 30s interval: 60s > floor
    }

    // --- probe_status_text ---

    #[test]
    fn probe_status_text_abbreviates_large_probe_counts() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        for i in 0..1_500usize {
            s.record_sent(i);
            s.record_result(i, Ok(10.0), 0, false);
        }
        let text = probe_status_text(&s, Instant::now(), 1_000);
        assert!(text.starts_with("1k probes, "), "large counts should abbreviate like other columns: {text:?}");
    }

    #[test]
    fn probe_status_text_never_seen_up_says_no_reply() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Err(()), 0, false);
        let text = probe_status_text(&s, Instant::now(), 1_000);
        assert!(text.starts_with("no reply, 1 probe, "), "{text:?}");
        assert!(!text.contains("last seen") && !text.contains("down "), "{text:?}");
    }

    #[test]
    fn probe_status_text_down_note_suppressed_until_threshold_then_shown() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Ok(10.0), 0, false);
        s.record_sent(1);
        s.record_result(1, Err(()), 0, false); // now down, having been up before
        let last_up = s.last_up.unwrap();

        let just_happened = last_up + Duration::from_secs(1);
        let text = probe_status_text(&s, just_happened, 1_000);
        assert!(!text.contains("down "), "a 1s-old drop shouldn't get a callout yet: {text:?}");

        let aged = last_up + Duration::from_secs(30);
        let text = probe_status_text(&s, aged, 1_000);
        assert!(text.contains("down 30s"), "a 30s-old drop should show the note: {text:?}");
    }

    #[test]
    fn probe_status_text_omits_down_suffix_while_up() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Ok(10.0), 0, false);
        let text = probe_status_text(&s, Instant::now(), 1_000);
        assert!(!text.contains("down ") && !text.contains("last drop"), "{text:?}");
    }

    #[test]
    fn probe_status_text_last_drop_note_suppressed_until_threshold_then_shown() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Err(()), 0, false); // a drop in the past
        s.record_sent(1);
        s.record_result(1, Ok(10.0), 0, false); // back up now
        let last_drop = s.last_drop_at.unwrap();

        let just_happened = last_drop + Duration::from_secs(1);
        let text = probe_status_text(&s, just_happened, 1_000);
        assert!(!text.contains("last drop"), "a 1s-old drop shouldn't get a callout yet: {text:?}");

        let aged = last_drop + Duration::from_secs(30);
        let text = probe_status_text(&s, aged, 1_000);
        assert!(text.contains("last drop 30s"), "a 30s-old drop should show the note: {text:?}");
        assert!(!text.contains("down "), "{text:?}");
    }

    #[test]
    fn probe_status_text_no_last_drop_note_when_never_dropped() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Ok(10.0), 0, false);
        s.record_sent(1);
        s.record_result(1, Ok(11.0), 0, false);
        let text = probe_status_text(&s, Instant::now(), 1_000);
        assert!(!text.contains("last drop"), "no drops ever recorded, nothing to note: {text:?}");
    }

    // --- fmt_count ---

    #[test]
    fn count_zero() { assert_eq!(fmt_count(0), "0"); }

    #[test]
    fn count_below_kilo() { assert_eq!(fmt_count(999), "999"); }

    #[test]
    fn count_exactly_kilo() { assert_eq!(fmt_count(1_000), "1k"); }

    #[test]
    fn count_kilo_truncates_remainder() { assert_eq!(fmt_count(1_999), "1k"); }

    #[test]
    fn count_exactly_mega() { assert_eq!(fmt_count(1_000_000), "1m"); }

    #[test]
    fn count_mega_truncates() { assert_eq!(fmt_count(5_500_000), "5m"); }

    #[test]
    fn count_exactly_giga() { assert_eq!(fmt_count(1_000_000_000), "1g"); }

    #[test]
    fn count_large_giga() { assert_eq!(fmt_count(999_000_000_000), "999g"); }

    // --- fmt_rtt ---

    #[test]
    fn rtt_zero_is_placeholder() { assert_eq!(fmt_rtt(0.0), "~"); }

    #[test]
    fn rtt_negative_is_placeholder() { assert_eq!(fmt_rtt(-1.0), "~"); }

    #[test]
    fn rtt_f64_max_is_placeholder() { assert_eq!(fmt_rtt(f64::MAX), "~"); }

    #[test]
    fn rtt_f64_min_is_placeholder() { assert_eq!(fmt_rtt(f64::MIN), "~"); }

    #[test]
    fn rtt_below_ten_two_decimals() { assert_eq!(fmt_rtt(1.5), "1.50"); }

    #[test]
    fn rtt_small_two_decimals() { assert_eq!(fmt_rtt(9.99), "9.99"); }

    #[test]
    fn rtt_exactly_ten_one_decimal() { assert_eq!(fmt_rtt(10.0), "10.0"); }

    #[test]
    fn rtt_below_hundred_one_decimal() { assert_eq!(fmt_rtt(55.5), "55.5"); }

    #[test]
    fn rtt_exactly_hundred_no_decimal() { assert_eq!(fmt_rtt(100.0), "100"); }

    #[test]
    fn rtt_above_hundred_no_decimal() { assert_eq!(fmt_rtt(123.456), "123"); }

    #[test]
    fn rtt_large_no_decimal() { assert_eq!(fmt_rtt(1000.0), "1000"); }

    // --- lerp_rgb ---

    #[test]
    fn lerp_at_zero_returns_start_color() {
        assert_eq!(lerp_rgb(0, 0, 0, 255, 255, 255, 0.0), (0, 0, 0));
    }

    #[test]
    fn lerp_at_one_returns_end_color() {
        assert_eq!(lerp_rgb(0, 0, 0, 255, 255, 255, 1.0), (255, 255, 255));
    }

    #[test]
    fn lerp_at_half() {
        assert_eq!(lerp_rgb(0, 0, 0, 100, 100, 100, 0.5), (50, 50, 50));
    }

    #[test]
    fn lerp_mixed_channels() {
        // t=0 returns start
        assert_eq!(lerp_rgb(0, 210, 70, 210, 190, 0, 0.0), (0, 210, 70));
    }

    #[test]
    fn lerp_mixed_channels_at_one() {
        assert_eq!(lerp_rgb(0, 210, 70, 210, 190, 0, 1.0), (210, 190, 0));
    }

    // --- gradient_color (via Theme::colorful()) ---

    #[test]
    fn gradient_at_zero_is_green() {
        assert_eq!(Theme::colorful().gradient_color(0.0), (0, 210, 70));
    }

    #[test]
    fn gradient_at_half_is_yellow() {
        assert_eq!(Theme::colorful().gradient_color(0.5), (210, 190, 0));
    }

    #[test]
    fn gradient_at_one_is_red() {
        assert_eq!(Theme::colorful().gradient_color(1.0), (210, 40, 0));
    }

    #[test]
    fn gradient_at_quarter() {
        // norm=0.25 → first lerp at t=0.5
        // r = 0 + 210*0.5 = 105, g = 210 - 20*0.5 = 200, b = 70 - 70*0.5 = 35
        assert_eq!(Theme::colorful().gradient_color(0.25), (105, 200, 35));
    }

    #[test]
    fn gradient_at_three_quarters() {
        // norm=0.75 > 0.5 → second lerp at t=0.5
        // r = 210 + 0*0.5 = 210, g = 190 - 150*0.5 = 115, b = 0
        assert_eq!(Theme::colorful().gradient_color(0.75), (210, 115, 0));
    }
}
