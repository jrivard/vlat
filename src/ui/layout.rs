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

use std::sync::atomic::{AtomicU8, Ordering};
use crate::time::{Instant, SystemTime, UNIX_EPOCH};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use crate::cli::{Args, BaseStat, ExtraStat, SortMode};
use crate::constants::{BAR_ANIM_SECS, THEME_LABEL_SECS};
use crate::state::TargetState;
use super::ViewCtx;
use crate::types::Sample;
use super::{
    show_dups_any, accent_color, trim_range_bar, compute_no_data,
    dialogs::{
        DialogMode, draw_warning_dialog, draw_help_dialog,
        draw_filename_dialog, draw_explain_dialog, draw_frozen_notice,
        draw_freeze_notice_dialog, draw_sort_notice_dialog, draw_sort_picker_dialog, draw_theme_notice_dialog,
        draw_theme_picker_dialog, draw_view_notice_dialog, draw_view_picker_dialog, draw_save_defaults_dialog,
        draw_window_input_dialog, draw_stat_column_toggle_dialog,
    },
    Theme,
    widgets::{
        build_combined_row_line,
        build_stats_keys_line, render_col_key_rule, KeysPrefix,
        build_current_rtt_spans, build_header_line,
        build_range_bar_spans,
        build_status_badge_spans, build_stats_line,
        build_target_sparkline_spans, TARGET_SPARK_W, TARGET_SPARK_MIN, SINGLE_RECENT_MAX_W,
        inline_range_key,
        render_area_graph, render_area_graph_multi,
        trend_spark_span,
    },
};

/// Truncate a `Line`'s spans so the total visible character count ≤ `max_cols`.
const LOADING_MSGS: &[&str] = &[
    "gathering data",        // index 0 - weight 60, default
    "reticulating splines",  // index 1 - weight 20
    "herding packets",       // index 2 - weight 15
    "counting electrons",    // index 3 - weight 10
    "sensing the ether",   // index 4 - weight  7
    "bending spacetime",        // index 5 - weight  5
    "staring into the jitter", // index 6 - weight  3
];
const LOADING_WEIGHTS: &[u32] = &[60, 20, 15, 10, 7, 5, 3];

// 0 = not yet chosen; 1..=N = index+1 into LOADING_MSGS.
// Cleared only on user-triggered window reset via clear_loading_message().
static LOADING_MSG_IDX: AtomicU8 = AtomicU8::new(0);



pub fn loading_message() -> &'static str {
    let stored = LOADING_MSG_IDX.load(Ordering::Relaxed);
    if stored != 0 {
        return LOADING_MSGS[(stored - 1) as usize];
    }
    // Pick once using wall-clock nanos as entropy - called at most once per cycle.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let total: u32 = LOADING_WEIGHTS.iter().sum();
    let mut roll = nanos % total;
    let mut chosen = 0u8;
    for (i, &w) in LOADING_WEIGHTS.iter().enumerate() {
        if roll < w { chosen = i as u8; break; }
        roll -= w;
    }
    LOADING_MSG_IDX.store(chosen + 1, Ordering::Relaxed);
    LOADING_MSGS[chosen as usize]
}

pub fn truncate_line(line: Line<'static>, max_cols: usize) -> Line<'static> {
    if max_cols == 0 { return Line::from(""); }
    let mut remaining = max_cols;
    let mut out: Vec<Span<'static>> = Vec::new();
    for span in line.spans {
        if remaining == 0 { break; }
        let len = span.content.chars().count();
        if len <= remaining {
            remaining -= len;
            out.push(span);
        } else {
            let s: String = span.content.chars().take(remaining).collect();
            out.push(Span::styled(s, span.style));
            remaining = 0;
        }
    }
    Line::from(out)
}


fn fmt_window_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 { format!("{}m", m) } else { format!("{}m{}s", m, s) }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 { format!("{}h", h) } else { format!("{}h{}m", h, m) }
    }
}

pub fn render_window_label(f: &mut Frame, area: Rect, _states: &[TargetState], args: &Args, sort_mode: &SortMode, sort_mode_changed: Option<Instant>) {
    let theme = &args.theme;
    let label_text = if args.window == 0 {
        "lifetime".to_string()
    } else {
        format!("{} window", fmt_window_secs(args.window))
    };
    if label_text.is_empty() { return; }

    let label_style = Style::default().fg(theme.c(theme.uptime));

    // Sort indicator: "  ⇅a" for name, "  ⇅▲" for mtr, empty for unsorted.
    let sort_tag: &str = match sort_mode {
        SortMode::None   => "",
        SortMode::Name   => "  \u{21c5}a",                        // ⇅a
        SortMode::Mtr    => "  \u{21c5}\u{25b2}",                 // ⇅▲
        SortMode::Avg    => "  \u{21c5}\u{00f8}",                 // ⇅ø
        SortMode::Loss   => "  \u{21c5}%",                        // ⇅%
        SortMode::Std    => "  \u{21c5}\u{03c3}",                 // ⇅σ
        SortMode::Jitter => "  \u{21c5}j",                        // ⇅j
        SortMode::Streak => "  \u{21c5}#",                        // ⇅#
        SortMode::P50    => "  \u{21c5}\u{00bd}",                 // ⇅½
        SortMode::P95    => "  \u{21c5}\u{2089}\u{2085}",         // ⇅₉₅
        SortMode::P99    => "  \u{21c5}\u{2089}\u{2089}",         // ⇅₉₉
        SortMode::P01    => "  \u{21c5}\u{2080}\u{2081}",         // ⇅₀₁
        SortMode::P10    => "  \u{21c5}\u{2081}\u{2080}",         // ⇅₁₀
        SortMode::Cv     => "  \u{21c5}cv",                       // ⇅cv
        SortMode::Srtt   => "  \u{21c5}\u{03c4}",                 // ⇅τ
        SortMode::Last   => "  \u{21c5}\u{2191}",                 // ⇅↑
    };

    // Highlight briefly after a mode change, then fade to the same grey as the window span.
    let recently_changed = sort_mode_changed
        .map(|t| t.elapsed().as_secs() < crate::constants::SORT_ARROW_SECS)
        .unwrap_or(false);
    let sort_style = if recently_changed {
        Style::default().fg(theme.xaxis_now).add_modifier(Modifier::BOLD)
    } else {
        label_style
    };

    let tag_chars = sort_tag.chars().count();
    let w: u16 = (tag_chars + label_text.len()) as u16;

    let y = area.y + area.height.saturating_sub(1);
    let x = if args.window == 0 {
        area.x + 1
    } else {
        area.x + area.width.saturating_sub(w + 1)
    };
    let rect = Rect::new(x, y, w.min(area.width), 1);

    if tag_chars > 0 {
        let line = Line::from(vec![
            Span::styled(label_text, label_style),
            Span::styled(sort_tag, sort_style),
        ]);
        f.render_widget(Paragraph::new(line), rect);
    } else {
        f.render_widget(
            Paragraph::new(Span::styled(label_text, label_style)),
            rect,
        );
    }
}

/// Briefly shows the active theme name in the fullscreen footer after a theme change.
fn render_theme_label(f: &mut Frame, area: Rect, theme: &Theme, theme_changed: Option<Instant>) {
    let t = match theme_changed {
        Some(t) => t,
        None => return,
    };
    if t.elapsed().as_secs() >= THEME_LABEL_SECS {
        return;
    }
    let label = format!("theme: {}", theme.name);
    let w = label.len() as u16;
    // Position to the left of the typical window-label area (~14 chars from the right edge).
    let gap: u16 = 14;
    let x = area.x + area.width.saturating_sub(w + gap);
    let y = area.y + area.height.saturating_sub(1);
    let max_w = w.min(area.width.saturating_sub(gap));
    if max_w == 0 { return; }
    f.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default().fg(theme.xaxis_now).add_modifier(Modifier::BOLD),
        )),
        Rect::new(x, y, max_w, 1),
    );
}

/// Helper to render floating dialogs (warning, help, explain, filename, freeze notice).
#[allow(clippy::too_many_arguments)]
pub fn render_dialogs(
    f: &mut Frame,
    area: Rect,
    args: &Args,
    dialog: &DialogMode,
    frozen: bool,
    sort_mode: &SortMode,
    target_count: usize,
    current_view: &str,
    tick: u64,
    is_logging: bool,
    space_hidden: u32,
    no_data: u32,
    show_col_keys: bool,
    show_headers: bool,
) {
    match dialog {
        DialogMode::None => {}
        DialogMode::Warning { message, dismiss_at } => {
            let secs_left = dismiss_at.saturating_duration_since(crate::time::Instant::now()).as_secs();
            draw_warning_dialog(f, area, message, secs_left, args.ascii, &args.theme);
        }
        DialogMode::Help { page, scroll, dismiss_at, logo, cursor, sub_menu, collapsed } => {
            let secs_left = dismiss_at.saturating_duration_since(crate::time::Instant::now()).as_secs();
            let sort_name = if target_count <= 1 { "n/a" } else { sort_mode.as_str() };
            let extra_cols_on = !args.extra_stats.is_empty();
            draw_help_dialog(f, area, *page, *scroll, args.ascii, &args.theme, secs_left, sort_name, frozen, current_view, logo, args.window, *cursor, sub_menu.as_ref(), is_logging, show_col_keys, show_headers, extra_cols_on, collapsed, target_count);
        }
        DialogMode::Explain { scroll } => {
            draw_explain_dialog(f, area, *scroll, args.ascii, &args.theme);
        }
        DialogMode::FilenameInput { format, input } => {
            draw_filename_dialog(f, area, format, input, &args.theme);
        }
        DialogMode::FreezeNotice { dismiss_at, now_frozen } => {
            let secs_left = dismiss_at.saturating_duration_since(crate::time::Instant::now()).as_secs();
            draw_freeze_notice_dialog(f, area, args.ascii, &args.theme, *now_frozen, secs_left);
        }
        DialogMode::SortNotice { dismiss_at, sort_mode: mode } => {
            let secs_left = dismiss_at.saturating_duration_since(crate::time::Instant::now()).as_secs();
            draw_sort_notice_dialog(f, area, args.ascii, &args.theme, mode, secs_left);
        }
        DialogMode::ThemeNotice { dismiss_at, theme_name } => {
            let secs_left = dismiss_at.saturating_duration_since(crate::time::Instant::now()).as_secs();
            draw_theme_notice_dialog(f, area, args.ascii, &args.theme, theme_name, secs_left);
        }
        DialogMode::ViewNotice { dismiss_at, view_name, view_desc } => {
            let secs_left = dismiss_at.saturating_duration_since(crate::time::Instant::now()).as_secs();
            draw_view_notice_dialog(f, area, args.ascii, &args.theme, view_name, view_desc, secs_left);
        }
        DialogMode::WindowInput { input } => {
            draw_window_input_dialog(f, area, input, args.window, &args.theme);
        }
        DialogMode::SaveDefaults { view_name, theme_name, sort_name, config_path,
                                   save_view, save_theme, save_sort,
                                   save_keys, save_window, save_cols,
                                   cursor, keys_current, window_current, cols_delta,
                                   file_view, file_theme, file_sort,
                                   file_keys, file_window, file_cols, .. } => {
            draw_save_defaults_dialog(f, area, *view_name, theme_name, sort_name, config_path,
                                      *save_view, *save_theme, *save_sort,
                                      *save_keys, *save_window, *save_cols,
                                      *cursor, *keys_current, *window_current, cols_delta,
                                      file_view.as_deref(), file_theme.as_deref(), file_sort.as_deref(),
                                      file_keys.as_deref(), file_window.as_deref(), file_cols.as_deref(),
                                      args.ascii, &args.theme);
        }
        DialogMode::StatColumnToggle { cursor, identity } => {
            draw_stat_column_toggle_dialog(f, area, *cursor, identity, &args.extra_stats, &args.hidden_base_stats, space_hidden, no_data, args.ascii, &args.theme);
        }
        DialogMode::SortPicker { cursor, .. } => {
            draw_sort_picker_dialog(f, area, args.ascii, &args.theme, *cursor, args.reverse_sort);
        }
        DialogMode::ThemePicker { cursor, .. } => {
            draw_theme_picker_dialog(f, area, args.ascii, &args.theme, *cursor);
        }
        DialogMode::ViewPicker { cursor, .. } => {
            draw_view_picker_dialog(f, area, args.ascii, &args.theme, *cursor, target_count);
        }
        // The scatter view draws this one itself (it needs live ScatterState,
        // which this shared helper doesn't have access to) - unreachable from
        // every other view, kept only for exhaustiveness.
        DialogMode::AxisPicker { .. } => {}
        // Likewise the worm/radar metric picker needs live WormState /
        // RadarState and draws itself - unreachable from every other view,
        // kept only for exhaustiveness.
        DialogMode::MetricPicker { .. } => {}
    }
    if frozen { draw_frozen_notice(f, area, args.ascii, &args.theme, tick); }
}



/// Full-screen "terminal too small" replacement overlay — btop-style.
/// Fills the frame with the background colour and shows a centered box with
/// the current vs. needed dimensions, highlighting whichever axis is too small.
pub fn draw_too_small(f: &mut Frame, curr_w: u16, curr_h: u16, min_w: u16, min_h: u16, theme: &super::Theme) {
    use ratatui::widgets::{Block, Clear};
    let area = f.area();

    // Clear / fill background
    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(Style::default().bg(Color::Black)), area);

    let warn  = Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD);
    let ok    = Style::default().fg(theme.rtt_good);
    let plain = Style::default().fg(Color::Gray);
    let title = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);

    let w_ok = curr_w >= min_w;
    let h_ok = curr_h >= min_h;

    // Build the lines of the centered message
    let lines: Vec<Line> = vec![
        Line::from(Span::styled("Terminal size too small", title)),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Current:  ", plain),
            Span::styled(format!("{}", curr_w), if w_ok { ok } else { warn }),
            Span::styled(" × ", plain),
            Span::styled(format!("{}", curr_h), if h_ok { ok } else { warn }),
        ]),
        Line::from(vec![
            Span::styled("  Needed:   ", plain),
            Span::styled(format!("{}", min_w), if w_ok { ok } else { warn }),
            Span::styled(" × ", plain),
            Span::styled(format!("{}", min_h), if h_ok { ok } else { warn }),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Resize the terminal to continue.", plain)),
        Line::from(vec![
            Span::styled("  h", title),
            Span::styled(" help   ", plain),
            Span::styled("q", title),
            Span::styled(" quit", plain),
        ]),
    ];

    // Find the widest line (in characters) to center the block
    let box_w: u16 = lines.iter().map(|l| {
        l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>()
    }).max().unwrap_or(40) as u16 + 4;

    let box_h: u16 = lines.len() as u16 + 2;

    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let msg_area = Rect::new(x, y, box_w.min(area.width), box_h.min(area.height));

    let para = Paragraph::new(lines)
        .block(Block::bordered().border_style(Style::default().fg(Color::DarkGray)))
        .style(Style::default().bg(Color::Black));
    f.render_widget(para, msg_area);
}

fn dim_color(color: Color, factor: f64) -> Color {
    let (r, g, b) = super::theme::color_to_rgb(color);
    Color::Rgb(
        (r as f64 * factor).min(255.0) as u8,
        (g as f64 * factor).min(255.0) as u8,
        (b as f64 * factor).min(255.0) as u8,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_single_target_history(
    f:            &mut Frame,
    chunks:       &[Rect],
    offset:       usize,
    n_hist:       usize,
    since:        usize,
    max_bar_w:    usize,
    state:        &TargetState,
    args:         &Args,
    shared_scale: f64,
    ac:           Color,
) {
    let border_ch = if args.ascii { "|" } else { "\u{258c}" };

    // `since` (the first successful reply's index) keeps a long pre-success drop
    // backlog out of the block: with `n_hist` now reserved at its final size (see
    // draw_single_ui), taking the last `n_hist` non-pending samples unrestricted
    // would reach back past the first success into that backlog to pad itself out.
    let mut recent: Vec<(Sample, u8)> = state.history.iter().enumerate()
        .rev()
        .filter(|(i, s)| *i >= since && !s.is_pending())
        .take(n_hist)
        .map(|(i, s)| (s.clone(), state.circle_history.get(i).copied().unwrap_or(1)))
        .collect();
    recent.reverse(); // oldest at index 0

    for row in 0..n_hist {
        let chunk = chunks[offset + row];
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(chunk);

        if row >= recent.len() { continue; } // blank space until history fills in

        // Oldest rows fade out; newest row is full brightness.
        let brightness = 0.35 + 0.65 * ((row + 1) as f64 / recent.len() as f64);

        f.render_widget(
            Paragraph::new(Line::from(
                Span::styled(border_ch, Style::default().fg(dim_color(ac, brightness)))
            )),
            horiz[0],
        );

        let content_w = horiz[1].width as usize;
        const RTT_W: usize = 5;
        const GAP:   usize = 1;
        if content_w <= RTT_W + GAP { continue; }
        let bar_w = (content_w - RTT_W - GAP).min(max_bar_w);

        let (ref sample, tier) = recent[row];
        let effective_scale = if state.scale_anim.is_some() && state.scale_anim_old > 0.0 {
            let elapsed = state.bar_anim_start.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            let frac = (elapsed / BAR_ANIM_SECS).clamp(0.0, 1.0);
            state.scale_anim_old + (shared_scale - state.scale_anim_old) * frac
        } else {
            shared_scale
        };
        let (rtt_label, base_color, fill_ratio): (String, Color, f64) = match sample {
            Sample::Hit(rtt) => {
                let color = match tier {
                    0   => args.theme.rtt_good,
                    2   => args.theme.rtt_warn,
                    255 => args.theme.drop_color,
                    _   => args.theme.rtt_normal,
                };
                let ratio = if effective_scale > 0.0 { rtt / effective_scale } else { 0.0 };
                (format!("{:>5}", super::fmt_rtt(*rtt)), color, ratio.clamp(0.0, 1.0))
            }
            Sample::Drop => {
                let sym = if args.ascii { "x" } else { "\u{d7}" };
                (format!("{:^5}", sym), args.theme.drop_color, 0.0)
            }
            Sample::Pending => continue,
        };

        let label_col    = dim_color(base_color, brightness);
        let bar_col      = dim_color(base_color, brightness * 0.8);
        let is_animating = state.scale_anim.is_some() && state.scale_anim_old > 0.0;

        // Avg RTT for the ⋮ marker — non-zero only for Hit samples.
        let avg_rtt: f64 = if matches!(sample, Sample::Hit(_)) {
            // Use window avg when in window mode; fall back to lifetime avg when window is
            // empty (e.g. early in session or after a window-size change).
            let r = if args.is_window() { state.win_avg() } else { 0.0 };
            if r > 0.0 { r } else { state.avg_latency() }
        } else {
            0.0
        };

        // Avg marker position in character units (full bar_w, used for non-animation rendering).
        let avg_char: Option<usize> = if avg_rtt > 0.0 && effective_scale > 0.0 && bar_w > 0 {
            let pos = ((avg_rtt / effective_scale) * bar_w as f64).round() as usize;
            Some(pos.min(bar_w - 1))
        } else {
            None
        };

        let mut spans = vec![
            Span::styled(rtt_label, Style::default().fg(label_col)),
            Span::raw(" "),
        ];

        if args.ascii {
            let fill_n = (fill_ratio * bar_w as f64).round() as usize;
            if is_animating {
                // Border arrows at the outermost columns; fill renders in inner_w columns.
                let (left_arrow, right_arrow) = if state.scale_anim_up { (">", ">") } else { ("<", "<") };
                spans.push(Span::styled(left_arrow, Style::default().fg(label_col)));
                let inner_w = bar_w.saturating_sub(2);
                if inner_w > 0 {
                    let fill_i = (fill_ratio * inner_w as f64).round() as usize;
                    let ap_i: Option<usize> = if avg_rtt > 0.0 && effective_scale > 0.0 {
                        let pos = ((avg_rtt / effective_scale) * inner_w as f64).round() as usize;
                        Some(pos.min(inner_w - 1))
                    } else { None };
                    if let Some(ap) = ap_i {
                        let good_col = dim_color(args.theme.rtt_good, brightness * 0.8);
                        let warn_col = dim_color(args.theme.rtt_warn, brightness * 0.8);
                        let avg_col  = dim_color(Color::White, brightness);
                        if fill_i <= ap {
                            spans.push(Span::styled("-".repeat(fill_i),          Style::default().fg(good_col)));
                            spans.push(Span::raw(" ".repeat(ap - fill_i)));
                            spans.push(Span::styled("|",                          Style::default().fg(avg_col)));
                            spans.push(Span::raw(" ".repeat(inner_w - ap - 1)));
                        } else {
                            spans.push(Span::styled("-".repeat(ap),               Style::default().fg(good_col)));
                            spans.push(Span::styled("|",                          Style::default().fg(avg_col)));
                            spans.push(Span::styled("-".repeat(fill_i - ap - 1), Style::default().fg(warn_col)));
                            spans.push(Span::raw(" ".repeat(inner_w - fill_i)));
                        }
                    } else {
                        spans.push(Span::styled("-".repeat(fill_i), Style::default().fg(bar_col)));
                        spans.push(Span::raw(" ".repeat(inner_w - fill_i)));
                    }
                }
                spans.push(Span::styled(right_arrow, Style::default().fg(label_col)));
            } else if let Some(ap) = avg_char {
                let good_col = dim_color(args.theme.rtt_good, brightness * 0.8);
                let warn_col = dim_color(args.theme.rtt_warn, brightness * 0.8);
                let avg_col  = dim_color(Color::White, brightness);
                if fill_n <= ap {
                    spans.push(Span::styled("-".repeat(fill_n),          Style::default().fg(good_col)));
                    spans.push(Span::raw(" ".repeat(ap - fill_n)));
                    spans.push(Span::styled("|",                          Style::default().fg(avg_col)));
                    spans.push(Span::raw(" ".repeat(bar_w - ap - 1)));
                } else {
                    spans.push(Span::styled("-".repeat(ap),               Style::default().fg(good_col)));
                    spans.push(Span::styled("|",                          Style::default().fg(avg_col)));
                    spans.push(Span::styled("-".repeat(fill_n - ap - 1), Style::default().fg(warn_col)));
                    spans.push(Span::raw(" ".repeat(bar_w - fill_n)));
                }
            } else {
                spans.push(Span::styled("-".repeat(fill_n), Style::default().fg(bar_col)));
                spans.push(Span::raw(" ".repeat(bar_w - fill_n)));
            }
        } else {
            let sub          = (fill_ratio * bar_w as f64 * 2.0) as usize;
            let full         = (sub / 2).min(bar_w);
            let half         = sub % 2 == 1 && full < bar_w;
            let empty        = bar_w - full - if half { 1 } else { 0 };
            if is_animating {
                // Border arrows at the outermost columns; fill renders in inner_w columns.
                let (left_arrow, right_arrow) = if state.scale_anim_up {
                    ("\u{25b6}", "\u{25b6}")
                } else {
                    ("\u{25c0}", "\u{25c0}")
                };
                spans.push(Span::styled(left_arrow, Style::default().fg(label_col)));
                let inner_w = bar_w.saturating_sub(2);
                if inner_w > 0 {
                    let sub_i   = (fill_ratio * inner_w as f64 * 2.0) as usize;
                    let full_i  = (sub_i / 2).min(inner_w);
                    let half_i  = sub_i % 2 == 1 && full_i < inner_w;
                    let empty_i = inner_w - full_i - if half_i { 1 } else { 0 };
                    let ap_i: Option<usize> = if avg_rtt > 0.0 && effective_scale > 0.0 {
                        let pos = ((avg_rtt / effective_scale) * inner_w as f64).round() as usize;
                        Some(pos.min(inner_w - 1))
                    } else { None };
                    if let Some(ap) = ap_i {
                        let good_col  = dim_color(args.theme.rtt_good, brightness * 0.8);
                        let warn_col  = dim_color(args.theme.rtt_warn, brightness * 0.8);
                        let avg_col   = dim_color(Color::White, brightness);
                        let below_avg = full_i <= ap;
                        let green_full = if below_avg { full_i } else { ap };
                        let green_half = half_i && full_i < ap;
                        spans.push(Span::styled("\u{2836}".repeat(green_full), Style::default().fg(good_col)));
                        if green_half {
                            spans.push(Span::styled("\u{2806}", Style::default().fg(good_col)));
                        }
                        if below_avg {
                            let gap = ap - green_full - if green_half { 1 } else { 0 };
                            spans.push(Span::raw(" ".repeat(gap)));
                        }
                        spans.push(Span::styled("\u{22ee}", Style::default().fg(avg_col)));
                        if below_avg {
                            spans.push(Span::raw(" ".repeat(inner_w - ap - 1)));
                        } else {
                            let orange_full = full_i.saturating_sub(ap + 1);
                            spans.push(Span::styled("\u{2836}".repeat(orange_full), Style::default().fg(warn_col)));
                            if half_i {
                                spans.push(Span::styled("\u{2806}", Style::default().fg(warn_col)));
                            }
                            spans.push(Span::raw(" ".repeat(empty_i)));
                        }
                    } else {
                        spans.push(Span::styled("\u{2836}".repeat(full_i), Style::default().fg(bar_col)));
                        if half_i {
                            spans.push(Span::styled("\u{2806}", Style::default().fg(bar_col)));
                        }
                        spans.push(Span::raw(" ".repeat(empty_i)));
                    }
                }
                spans.push(Span::styled(right_arrow, Style::default().fg(label_col)));
            } else if let Some(ap) = avg_char {
                let good_col  = dim_color(args.theme.rtt_good, brightness * 0.8);
                let warn_col  = dim_color(args.theme.rtt_warn, brightness * 0.8);
                let avg_col   = dim_color(Color::White, brightness);
                let below_avg = full <= ap;
                let green_full = if below_avg { full } else { ap };
                // Half char lands before the marker only when fill doesn't reach marker position.
                let green_half = half && full < ap;
                spans.push(Span::styled("\u{2836}".repeat(green_full), Style::default().fg(good_col)));
                if green_half {
                    spans.push(Span::styled("\u{2806}", Style::default().fg(good_col)));
                }
                if below_avg {
                    let gap = ap - green_full - if green_half { 1 } else { 0 };
                    spans.push(Span::raw(" ".repeat(gap)));
                }
                spans.push(Span::styled("\u{22ee}", Style::default().fg(avg_col)));
                if below_avg {
                    spans.push(Span::raw(" ".repeat(bar_w - ap - 1)));
                } else {
                    let orange_full = full.saturating_sub(ap + 1);
                    spans.push(Span::styled("\u{2836}".repeat(orange_full), Style::default().fg(warn_col)));
                    if half {
                        spans.push(Span::styled("\u{2806}", Style::default().fg(warn_col)));
                    }
                    spans.push(Span::raw(" ".repeat(empty)));
                }
            } else {
                spans.push(Span::styled("\u{2836}".repeat(full), Style::default().fg(bar_col)));
                if half {
                    spans.push(Span::styled("\u{2806}", Style::default().fg(bar_col)));
                }
                spans.push(Span::raw(" ".repeat(empty)));
            }
        }

        let line = truncate_line(Line::from(spans), content_w);
        f.render_widget(Paragraph::new(line), horiz[1]);
    }
}

pub fn draw_list_ui(f: &mut Frame, states: &[TargetState], ctx: &ViewCtx) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, dialog, tick, frozen, sort_order, sort_arrows, sort_mode, show_col_keys, .. } = ctx;
    let area = f.area();
    let n = states.len();
    {
        let (min_w, min_h) = super::min_size("list", n, show_col_keys, false, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            draw_too_small(f, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let col_keys_h: u16 = if show_col_keys { 2 } else { 0 };

    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    constraints.extend((0..n).map(|_| Constraint::Length(1)));
    constraints.push(Constraint::Min(0));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let global_mode: Option<&str> = if mode_labels.iter().all(|m| m.as_str() == "icmp") {
        Some("icmp")
    } else {
        None
    };
    let show_badges = super::mode_badge_visible(args.column_vis.mode, mode_labels);
    let badge_pad_w = mode_labels.iter().map(|m| m.len()).max().unwrap_or(0);
    let max_ip_chg  = states.iter().map(|s| s.ip_changes).max().unwrap_or(0);
    let ip_changes_slot_w = super::ip_changes_slot_width(args.column_vis.resolve, max_ip_chg);
    let show_drp = true;
    let show_dup = show_dups_any(states, args);
    let name_part = if col_widths.name_w > 0 { col_widths.name_w + 2 } else { 0 };
    let terminal_w = area.width as usize;
    let border_ch = if args.ascii { "|" } else { "\u{258c}" }; // ▌
    let content_w = terminal_w.saturating_sub(1); // accent border
    let base_overhead = 2 + badge_pad_w + 2 + name_part + col_widths.label + 3 + ip_changes_slot_w + 3;
    let ideal_stats_w = col_widths.stats_width_with_gap(2, show_drp, show_dup);
    const BAR_MIN: usize = 15;
    const BAR_MAX: usize = 25;
    let show_recent = args.extra_stats.contains(&ExtraStat::Recent);
    let show_bar    = args.extra_stats.contains(&ExtraStat::Bar);
    let extra = content_w.saturating_sub(base_overhead);
    let bar_reserve = if show_bar { 1 + BAR_MIN } else { 0 };
    let space_for_spark = extra.saturating_sub(ideal_stats_w + bar_reserve);
    let circles_w = if show_recent { let b = space_for_spark.saturating_sub(1); if b >= TARGET_SPARK_MIN { b.min(TARGET_SPARK_W) } else { 0 } } else { 0 };
    let spark_overhead = if circles_w > 0 { 1 + circles_w } else { 0 };
    let after_spark = extra.saturating_sub(spark_overhead);
    let bar_w = if !show_bar { 0 } else if after_spark > ideal_stats_w { (after_spark - ideal_stats_w).clamp(BAR_MIN, BAR_MAX) } else { BAR_MIN };
    let bar_overhead = if bar_w > 0 { 1 + bar_w } else { 0 };
    let stats_avail = after_spark.saturating_sub(bar_overhead);
    let effective_cw = col_widths.with_budget(stats_avail, show_drp, show_dup);
    let (addr_gap, gap) = super::pick_gaps(stats_avail + 3, &effective_cw, show_drp, show_dup);
    let space_hidden = super::compute_space_hidden(&effective_cw, show_recent, circles_w, stats_avail, gap, show_drp, show_dup);

    if show_col_keys {
        let badge_part = if show_badges { badge_pad_w + 3 } else { 0 };
        let anim_state = states.iter().find(|s| s.scale_anim.is_some());
        let (inline_range_label, inline_range_spans) = if bar_w > 0 { inline_range_key(anim_state, shared_scale, bar_w, args.ascii, &args.theme) } else { (String::new(), vec![]) };
        let pfx = KeysPrefix {
            lead: 3, probe: badge_part, name: name_part, addr: col_widths.label,
            trailer: addr_gap, resolve: ip_changes_slot_w, status: super::STATUS_BADGE_W,
            pings_gap: if circles_w > 0 { 1 } else { 0 }, pings: circles_w,
            inline_range_w: if bar_w > 0 { 1 + bar_w } else { 0 }, inline_range_label, inline_range_spans,
        };
        let hdr = build_stats_keys_line(&pfx, &effective_cw, gap, true, show_drp, show_dup, args.ascii, &args.theme);
        render_col_key_rule(f, hdr, chunks[0], chunks[1], args.ascii, &args.theme);
    }

    for (display_i, &slot) in sort_order.iter().enumerate() {
        let state = &states[slot];
        let sort_arrow = sort_arrows.get(slot).and_then(|&opt| opt).and_then(|(t, up)| {
            if t.elapsed().as_secs() < crate::constants::SORT_ARROW_SECS { Some(up) } else { None }
        });
        let ac = accent_color(state, args, Color::DarkGray);
        let color = if n > 1 {
            let (cr, cg, cb) = args.theme.target_color(slot);
            Color::Rgb(cr, cg, cb)
        } else {
            args.theme.hostname
        };

        let mut post_badge: Vec<Span<'static>> = build_current_rtt_spans(state, &effective_cw, &args.theme);
        if circles_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(build_target_sparkline_spans(state, args, circles_w, shared_scale));
        }
        if bar_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(trim_range_bar(build_range_bar_spans(state, args.ascii, shared_scale, &args.theme, bar_w, show_col_keys)));
        }
        let line = build_combined_row_line(
            state, args,
            &mode_labels[slot],
            &effective_cw,
            color,
            shared_scale,
            tick,
            if display_i == 0 { log_fmt } else { "" },
            global_mode,
            badge_pad_w,
            ip_changes_slot_w,
            sort_arrow,
            show_drp,
            show_dup,
            false,
            post_badge,
            false,
            true,
            gap,
            addr_gap,
        );

        let row_chunk = chunks[col_keys_h as usize + display_i];
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(row_chunk);
        let border_area  = horiz[0];
        let content_area = horiz[1];
        let w = content_area.width as usize;

        let bl = Line::from(Span::styled(border_ch, Style::default().fg(ac)));
        f.render_widget(Paragraph::new(bl), border_area);

        // Layout: [trend][ ][badge?][label][UP/xX][recent×28][ ][rtt+...]  [spinner][↻n]
        let mut spans = line.spans;
        if spans.first().map(|sp| sp.content.as_ref()) == Some("  ") {
            let trend = state.mtr_trend(args.graph_interval);
            spans[0] = Span::raw(" ");
            spans.insert(0, trend_spark_span(trend, args.ascii, &args.theme));
        }
        let line = truncate_line(Line::from(spans), w);
        f.render_widget(Paragraph::new(line), content_area);
    }

    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(f, area, args, dialog, frozen, sort_mode, states.len(), "list", tick, !log_fmt.is_empty(), space_hidden, no_data, show_col_keys, false);
}

/// One item in the single-target view's stat row(s); packed by `compute_single_layout`
/// into as many rows as needed and rendered with full-word labels ("jitter ", not "j").
#[derive(Clone)]
enum StatItem { Avg, Range, Jitter, Drops, Dup, Extra(ExtraStat) }

impl StatItem {
    fn base(&self) -> Option<BaseStat> {
        match self {
            StatItem::Avg    => Some(BaseStat::Avg),
            StatItem::Range  => Some(BaseStat::Range),
            StatItem::Jitter => Some(BaseStat::Jitter),
            StatItem::Drops  => Some(BaseStat::Drops),
            _ => None,
        }
    }
}

/// Layout metrics for the single-target view that don't depend on `--history-rows`
/// itself. Shared between `draw_single_ui` (to render) and `single_history_avail`
/// (to cap the Up/Down history-rows adjustment at what the current terminal can
/// actually display).
struct SingleLayout {
    effective_cw: super::ColWidths,
    gap: usize,
    content_w: usize,
    show_drp: bool,
    show_dup: bool,
    recent_w: usize,
    recent_row_h: u16,
    stat_pages: Vec<Vec<StatItem>>,
    /// Rows available for scrolling history once the name row, recent bar, stat
    /// row(s), and status line have each claimed their space.
    avail: usize,
}

fn compute_single_layout(area: Rect, states: &[TargetState], args: &Args, col_widths: &super::ColWidths) -> SingleLayout {
    const INDENT_W: usize = 2;
    const FIXED_ROWS: usize = 1; // combined name/address + status line (stats/recent-bar rows counted separately)

    let show_drp   = true;
    let show_dup   = show_dups_any(states, args);
    let content_w  = (area.width as usize).saturating_sub(1); // accent border (name/address, history)
    let content_w2 = (area.width as usize).saturating_sub(INDENT_W); // 2-space indent (status, recent bar, stats)
    let show_recent = args.extra_stats.contains(&ExtraStat::Recent);

    // The stats line gets the full row width; compensate for the status-badge + rtt
    // slot it doesn't render itself (the avg column reuses cw.rtt's width, so that
    // width has to be settled before the status line below can borrow it).
    let stats_budget = content_w2 + super::STATUS_BADGE_W + col_widths.rtt.active_w();
    let effective_cw = col_widths.with_budget(stats_budget, show_drp, show_dup);
    let gap = if effective_cw.stats_width_with_gap(2, show_drp, show_dup) <= content_w2 { 2 } else { 1 };

    // Pack the enabled stat columns into as many rows as needed to show all of them
    // (in display order) instead of truncating whatever doesn't fit on one line.
    let item_w = |item: &StatItem| -> usize {
        let (label_w, val_w): (usize, usize) = match item {
            StatItem::Avg    => (4, effective_cw.rtt.active_w()),             // "avg "
            StatItem::Range  => (6, 2 * effective_cw.range_compact + 1),      // "range " min-max
            StatItem::Jitter => (7, effective_cw.jitter.active_w()),          // "jitter "
            StatItem::Drops  => (5, effective_cw.drp),                       // "loss "
            StatItem::Dup    => (4, effective_cw.dup + 1 + 4),               // "dup " count " " pct
            StatItem::Extra(ExtraStat::Mtr)    => (4, effective_cw.mtr.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::Std)    => (4, effective_cw.std.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::P01)    => (4, effective_cw.p01.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::P10)    => (4, effective_cw.p10.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::P50)    => (4, effective_cw.p50.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::P95)    => (4, effective_cw.p95.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::P99)    => (4, effective_cw.p99.as_ref().map_or(0,  |c| c.active_w())),
            StatItem::Extra(ExtraStat::Cv)     => (3, effective_cw.cv.unwrap_or(0)),
            StatItem::Extra(ExtraStat::Srtt)   => (5, effective_cw.srtt.as_ref().map_or(0, |c| c.active_w())),
            StatItem::Extra(ExtraStat::Streak) => (7, effective_cw.streak.unwrap_or(0)),
            StatItem::Extra(ExtraStat::Last)   => (5, effective_cw.last.unwrap_or(0)),
            StatItem::Extra(ExtraStat::Status) => (7, effective_cw.status.unwrap_or(0)), // "status "
            StatItem::Extra(_) => (0, 0),
        };
        if label_w == 0 && val_w == 0 { 0 } else { gap + label_w + val_w }
    };
    let hide = |b: &BaseStat| effective_cw.hidden_base_stats.contains(b);
    let mut stat_items: Vec<StatItem> = Vec::new();
    if !hide(&BaseStat::Avg)    { stat_items.push(StatItem::Avg); }
    if !hide(&BaseStat::Range)  { stat_items.push(StatItem::Range); }
    if !hide(&BaseStat::Jitter) { stat_items.push(StatItem::Jitter); }
    if show_drp && !hide(&BaseStat::Drops) { stat_items.push(StatItem::Drops); }
    if show_dup { stat_items.push(StatItem::Dup); }
    for stat in &effective_cw.stat_order {
        stat_items.push(StatItem::Extra(stat.clone()));
    }
    let mut stat_pages: Vec<Vec<StatItem>> = Vec::new();
    {
        let mut cur: Vec<StatItem> = Vec::new();
        let mut cur_w = 0usize;
        for item in stat_items {
            let w = item_w(&item);
            if !cur.is_empty() && cur_w + w > content_w2 {
                stat_pages.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur_w += w;
            cur.push(item);
        }
        stat_pages.push(cur); // always at least one row, even when empty
    }
    let n_stats_rows = stat_pages.len();

    // The recent-trend bar gets its own row, capped at SINGLE_RECENT_MAX_W rather
    // than stretching to fill a wide terminal - see that constant's doc comment.
    let recent_w = if show_recent && content_w2 >= TARGET_SPARK_MIN { content_w2.min(SINGLE_RECENT_MAX_W) } else { 0 };
    let recent_row_h: u16 = if recent_w > 0 { 1 } else { 0 };

    let avail = area.height.saturating_sub(FIXED_ROWS as u16)
        .saturating_sub(recent_row_h)
        .saturating_sub(n_stats_rows as u16) as usize;

    SingleLayout { effective_cw, gap, content_w, show_drp, show_dup, recent_w, recent_row_h, stat_pages, avail }
}

/// Max `--history-rows` the single-target view can actually display given the
/// current terminal size - used to cap the Up/Down history-rows key adjustment so
/// it stops at what fits on screen instead of an arbitrary constant.
pub fn single_history_avail(area: Rect, states: &[TargetState], args: &Args, col_widths: &super::ColWidths) -> usize {
    compute_single_layout(area, states, args, col_widths).avail
}

/// Detailed single-target view. Only ever invoked with exactly one target.
///
/// Layout: a name/address row, a scrolling per-return history (height set by
/// `--history-rows`), a large recent-trend bar on its own row, a line of whichever
/// stat columns are enabled (labelled with words rather than the single-char
/// glyphs used elsewhere, since there's no column-key legend row here to
/// define them), and a compact status line (up/down badge, total probes sent,
/// elapsed time) - the same style used for the pre-first-reply waiting line.
/// The data lines share a 2-space indent.
/// The column-key legend used by `draw_list_ui` is not shown here - there is only
/// ever one target, so there is nothing to disambiguate.
pub fn draw_single_ui(f: &mut Frame, states: &[TargetState], ctx: &ViewCtx) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, dialog, tick, frozen, sort_order, sort_mode, .. } = ctx;
    let area = f.area();
    let n = states.len();
    {
        let (min_w, min_h) = super::min_size("single", n, false, false, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            draw_too_small(f, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let slot       = sort_order[0];
    let state      = &states[slot];
    let mode_label = &mode_labels[slot];
    let ac         = accent_color(state, args, Color::DarkGray);
    let border_ch  = if args.ascii { "|" } else { "\u{258c}" }; // ▌
    const INDENT_W: usize = 2;

    let SingleLayout { effective_cw, gap, content_w, show_drp, show_dup, recent_w, recent_row_h, stat_pages, avail } =
        compute_single_layout(area, states, args, col_widths);

    // Until the first successful reply comes back, skip the full row layout
    // (history/bar/stats/status) entirely and show one line: name/address, a
    // spinner (or DOWN once a probe has timed out), the probe counter, and the
    // total elapsed wait time. This avoids reserving a tall block of blank
    // history rows before there's anything real to show in them.
    if state.last_up.is_none() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(chunks[0]);
        f.render_widget(Paragraph::new(Line::from(Span::styled(border_ch, Style::default().fg(ac)))), horiz[0]);

        let show_mode_badge = super::mode_badge_visible(args.column_vis.mode, std::slice::from_ref(mode_label));
        let badge_pad_w = if show_mode_badge { mode_label.len() } else { 0 };
        let mut line = build_header_line(
            state, args, false, mode_label, log_fmt, tick,
            None, show_mode_badge, badge_pad_w, horiz[1].width, None, false,
        );

        if state.current_ip.is_some() {
            let is_down = state.is_currently_down();
            line.spans.push(Span::raw("  "));
            if is_down {
                line.spans.push(Span::styled(
                    "DOWN",
                    Style::default().fg(args.theme.drop_color).add_modifier(Modifier::BOLD),
                ));
            } else {
                let spin_tick = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| (d.as_millis() / 100) as usize)
                    .unwrap_or(tick as usize);
                let frame_str = if args.ascii {
                    ["|", "/", "-", "\\"][spin_tick % 4].to_string()
                } else {
                    // 3-dot blob tracing the 12-position perimeter of the 4×4 braille grid CW -
                    // matches the "waiting for first probe result" spinner used elsewhere.
                    const FRAMES: &[&str] = &[
                        "\u{2809}\u{2801}", "\u{2808}\u{2809}", "\u{2800}\u{2819}", "\u{2800}\u{2838}",
                        "\u{2800}\u{28b0}", "\u{2800}\u{28e0}", "\u{2880}\u{28c0}", "\u{28c0}\u{2840}",
                        "\u{28c4}\u{2800}", "\u{2846}\u{2800}", "\u{2807}\u{2800}", "\u{280b}\u{2800}",
                    ];
                    FRAMES[spin_tick % FRAMES.len()].to_string()
                };
                line.spans.push(Span::styled(frame_str, Style::default().fg(Color::Gray)));
            }

            line.spans.push(Span::raw("  "));
            line.spans.push(Span::raw(super::probe_status_text(state, Instant::now(), args.interval)));
        }

        let w = horiz[1].width as usize;
        f.render_widget(Paragraph::new(truncate_line(line, w)), horiz[1]);

        let space_hidden = super::compute_space_hidden(&effective_cw, false, 0, usize::MAX, gap, show_drp, show_dup);
        let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
        render_dialogs(f, area, args, dialog, frozen, sort_mode, states.len(), "single", tick, !log_fmt.is_empty(), space_hidden, no_data, false, false);
        return;
    }

    // Reserve the history block at its eventual full size (bounded by the terminal
    // and --history-rows) the instant nominal view is entered, rather than growing
    // it one row at a time as replies arrive. Growing it used to push the recent-
    // bar/stats/status block down the screen a little further on every reply until
    // the history block filled up, which read as the terminal scrolling. Reserving
    // the final size up front means that block paints once, in its final spot, and
    // stays there; `render_single_target_history` already leaves not-yet-filled
    // rows blank (see its `row >= recent.len()` check) so the real rows it does
    // have just fill in from the top, immediately above the pinned block below,
    // with no repositioning of anything already on screen.
    let n_hist: usize = (args.history_rows as usize).min(avail);

    // A trend bar/jitter/std built from a single sample is meaningless, so its
    // content doesn't appear until the second reply - but the rows below still
    // reserve their eventual space (via recent_row_h/stat_pages, computed above
    // for the final layout) so the status line never has to jump when that
    // content does appear; only its rendering is gated on `reveal_extra` below.
    let filled_hist = match state.first_success_idx {
        Some(start) => state.history[start..].iter().filter(|s| !s.is_pending()).count(),
        None => 0, // unreachable here - last_up.is_some() (checked above) implies this is Some too
    };
    let reveal_extra = filled_hist >= 2;
    let n_stats_rows = stat_pages.len();

    let mut constraints: Vec<Constraint> = Vec::new();
    for _ in 0..n_hist {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));         // name/address + status badge + probe summary, combined
    for _ in 0..n_stats_rows {
        constraints.push(Constraint::Length(1));     // remaining enabled stat columns (as many rows as needed)
    }
    constraints.push(Constraint::Length(recent_row_h)); // large recent-trend bar, own row (closes out the block)
    constraints.push(Constraint::Min(0));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let indent_row = |chunk: Rect| -> Rect {
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(INDENT_W as u16), Constraint::Min(0)])
            .split(chunk);
        horiz[1]
    };

    if n_hist > 0 {
        const HIST_LINE_MAX_W: usize = 80; // border + RTT label + gap + bar
        let hist_content_w = content_w.min(HIST_LINE_MAX_W - 1);
        let hist_max_bar_w = hist_content_w.saturating_sub(6); // 5 RTT label + 1 gap
        let since = state.first_success_idx.unwrap_or(0);
        render_single_target_history(f, &chunks, 0, n_hist, since, hist_max_bar_w, state, args, shared_scale, ac);
    }

    // ── Line: name/address · up/down status badge · probe summary ──────────────
    // Leads the block below the history rows - border, header, badge, probe/uptime
    // text - the same shape as the pre-first-reply waiting line, whether or not a
    // reply has ever come back.
    {
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(chunks[n_hist]);
        f.render_widget(Paragraph::new(Line::from(Span::styled(border_ch, Style::default().fg(ac)))), horiz[0]);

        let show_mode_badge = super::mode_badge_visible(args.column_vis.mode, std::slice::from_ref(mode_label));
        let badge_pad_w = if show_mode_badge { mode_label.len() } else { 0 };
        let mut line = build_header_line(
            state, args, false, mode_label, log_fmt, tick,
            None, show_mode_badge, badge_pad_w, horiz[1].width, None, false,
        );

        // Currently down reads as the same bold "DOWN" word used by the pre-first-reply
        // line, rather than the compact "XX"/"✗✗" badge used elsewhere - it reads better
        // paired with the probe-status text than the 3-char badge does.
        line.spans.push(Span::raw("  "));
        if state.is_currently_down() {
            line.spans.push(Span::styled("DOWN", Style::default().fg(args.theme.drop_color).add_modifier(Modifier::BOLD)));
        } else {
            line.spans.extend(build_status_badge_spans(state, tick, &args.theme, args.ascii));
        }
        line.spans.push(Span::raw("  "));
        line.spans.push(Span::raw(super::probe_status_text(state, Instant::now(), args.interval)));

        let w = horiz[1].width as usize;
        f.render_widget(Paragraph::new(truncate_line(line, w)), horiz[1]);
    }

    // ── Lines: remaining enabled statistic columns (wrapped across rows as needed) ──
    // Same reasoning as the recent-trend bar below: the rows are always reserved,
    // content is gated on `reveal_extra`.
    if reveal_extra {
        for (i, page) in stat_pages.iter().enumerate() {
            let page_present_bases: Vec<BaseStat> = page.iter().filter_map(StatItem::base).collect();
            let mut page_hidden = vec![BaseStat::Avg, BaseStat::Range, BaseStat::Jitter, BaseStat::Drops];
            page_hidden.retain(|b| !page_present_bases.contains(b));
            let page_stat_order: Vec<ExtraStat> = page.iter()
                .filter_map(|it| if let StatItem::Extra(e) = it { Some(e.clone()) } else { None })
                .collect();
            let page_show_drp = page.iter().any(|it| matches!(it, StatItem::Drops));
            let page_show_dup = page.iter().any(|it| matches!(it, StatItem::Dup));

            let mut page_cw = effective_cw.clone();
            page_cw.hidden_base_stats = page_hidden;
            page_cw.stat_order = page_stat_order;

            // build_stats_line always leads with a `gap`-wide separator (meant to follow
            // the rtt column it doesn't render here); fold that into the indent so the
            // rendered text lines up flush with the status line's 2-space indent.
            let stats_indent = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(INDENT_W.saturating_sub(gap) as u16), Constraint::Min(0)])
                .split(chunks[1 + n_hist + i]);
            let content_area = stats_indent[1];

            let line = build_stats_line(
                state, args.ascii, args.is_window(), shared_scale, &page_cw,
                false, false, false, &args.theme, page_show_drp, page_show_dup, tick, gap, true,
                args.interval,
            );
            let w = content_area.width as usize;
            f.render_widget(Paragraph::new(truncate_line(line, w)), content_area);
        }
    }

    // ── Line: large recent-trend bar (own row, closes out the block) ──────────
    // The row is reserved as soon as it's enabled (folded into `avail` above)
    // regardless of `reveal_extra`; only its content waits for the second reply.
    if recent_w > 0 && reveal_extra {
        let content_area = indent_row(chunks[n_hist + 1 + n_stats_rows]);
        let spans = build_target_sparkline_spans(state, args, recent_w, shared_scale);
        let w = content_area.width as usize;
        f.render_widget(Paragraph::new(truncate_line(Line::from(spans), w)), content_area);
    }

    // Stats now wrap across as many rows as needed rather than being cut off, so
    // nothing is width-hidden here; pass an unbounded budget to compute_space_hidden.
    let space_hidden = super::compute_space_hidden(&effective_cw, false, 0, usize::MAX, gap, show_drp, show_dup);
    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(f, area, args, dialog, frozen, sort_mode, states.len(), "single", tick, !log_fmt.is_empty(), space_hidden, no_data, false, false);
}

/// Helper to render common fullscreen overlays (dialogs, window span, theme labels, freeze notice).
#[allow(clippy::too_many_arguments)]
fn render_fullscreen_overlays(
    f: &mut Frame,
    area: Rect,
    states: &[TargetState],
    args: &Args,
    dialog: &DialogMode,
    frozen: bool,
    sort_mode: &SortMode,
    sort_mode_changed: Option<Instant>,
    _show_legend: bool,
    tick: u64,
    is_logging: bool,
    space_hidden: u32,
    show_col_keys: bool,
    show_headers: bool,
) {
    // Window span - bottom-right corner overlay (suppressed in fullscreen when showing lifetime stats)
    if args.is_window() {
        render_window_label(f, area, states, args, sort_mode, sort_mode_changed);
    }
    render_theme_label(f, area, &args.theme, args.theme_changed);

    let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
    render_dialogs(f, area, args, dialog, frozen, sort_mode, states.len(), "graph", tick, is_logging, space_hidden, no_data, show_col_keys, show_headers);
}

pub fn draw_fullscreen_ui(f: &mut Frame, s: &TargetState, mode_label: &str, ctx: &ViewCtx) {
    let &ViewCtx { args, col_widths, shared_scale, log_fmt, dialog, tick, frozen, sort_mode, show_headers, show_col_keys, .. } = ctx;
    let area = f.area();
    {
        let ml = vec![mode_label.to_string()];
        let (min_w, min_h) = super::min_size("graph", 1, show_col_keys, show_headers, col_widths, &ml, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            draw_too_small(f, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let hdr_h  = if show_headers  { 1u16 } else { 0 };
    let keys_h = if show_col_keys { 2u16 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(keys_h.min(1)),  // col-key names (0 or 1)
            Constraint::Length(if show_col_keys { 1 } else { 0 }), // col-key rule (0 or 1)
            Constraint::Length(hdr_h),           // combined header+stats
            Constraint::Min(4),                  // graph
            Constraint::Length(1),               // x-axis time labels
            Constraint::Length(1),               // range bar
        ])
        .split(area);
    // chunks: [0]=key_names [1]=key_rule [2]=hdr [3]=graph [4]=x_axis [5]=range_bar

    let show_drp = true;
    let show_dup = show_dups_any(std::slice::from_ref(s), args);
    let ip_changes_slot_w = super::ip_changes_slot_width(args.column_vis.resolve, s.ip_changes);
    let avail_w = area.width as usize;
    let border_ch = if args.ascii { "|" } else { "\u{258c}" }; // ▌
    let content_w = avail_w.saturating_sub(1); // accent border
    let global_mode: Option<&str> = if mode_label == "icmp" { Some("icmp") } else { None };
    let show_badge = args.column_vis.mode.unwrap_or(mode_label != "icmp");
    let badge_pad_w = if show_badge { mode_label.len() } else { 0 };
    let badge_w = if show_badge { badge_pad_w + 3 } else { 0 };
    let name_part_w = if col_widths.name_w > 0 { col_widths.name_w + 2 } else { 0 };
    let prefix_w_base = 2 + badge_w + name_part_w + col_widths.label + 3 + ip_changes_slot_w;
    let ideal_stats_w = col_widths.stats_width_with_gap(2, show_drp, show_dup);
    const BAR_MIN: usize = 15;
    const BAR_MAX: usize = 25;
    let show_recent = args.extra_stats.contains(&ExtraStat::Recent);
    let show_bar    = args.extra_stats.contains(&ExtraStat::Bar);
    let extra = content_w.saturating_sub(prefix_w_base);
    let bar_reserve = if show_bar { 1 + BAR_MIN } else { 0 };
    let space_for_spark = extra.saturating_sub(ideal_stats_w + bar_reserve);
    let circles_w = if show_recent { let b = space_for_spark.saturating_sub(1); if b >= TARGET_SPARK_MIN { b.min(TARGET_SPARK_W) } else { 0 } } else { 0 };
    let spark_overhead = if circles_w > 0 { circles_w + 1 } else { 0 };
    let after_spark = extra.saturating_sub(spark_overhead);
    let bar_w = if !show_bar { 0 } else if after_spark > ideal_stats_w { (after_spark - ideal_stats_w).clamp(BAR_MIN, BAR_MAX) } else { BAR_MIN };
    let bar_overhead = if bar_w > 0 { 1 + bar_w } else { 0 };
    let stats_avail = after_spark.saturating_sub(bar_overhead);
    let effective_cw = col_widths.with_budget(stats_avail, show_drp, show_dup);
    let (addr_gap, gap) = super::pick_gaps(stats_avail + 3, &effective_cw, show_drp, show_dup);
    let space_hidden = super::compute_space_hidden(&effective_cw, show_recent, circles_w, stats_avail, gap, show_drp, show_dup);

    if show_col_keys {
        let anim_state = if s.scale_anim.is_some() { Some(s) } else { None };
        let (inline_range_label, inline_range_spans) = if bar_w > 0 { inline_range_key(anim_state, shared_scale, bar_w, args.ascii, &args.theme) } else { (String::new(), vec![]) };
        let pfx = KeysPrefix {
            lead: 3, probe: badge_w, name: name_part_w, addr: col_widths.label,
            trailer: addr_gap, resolve: ip_changes_slot_w, status: super::STATUS_BADGE_W,
            pings_gap: if circles_w > 0 { 1 } else { 0 }, pings: circles_w,
            inline_range_w: if bar_w > 0 { 1 + bar_w } else { 0 }, inline_range_label, inline_range_spans,
        };
        let hdr = build_stats_keys_line(&pfx, &effective_cw, gap, true, show_drp, show_dup, args.ascii, &args.theme);
        render_col_key_rule(f, hdr, chunks[0], chunks[1], args.ascii, &args.theme);
    }

    if show_headers {
        let hdr_chunk = chunks[2];
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(hdr_chunk);
        let border_area  = horiz[0];
        let content_area = horiz[1];
        let hdr_avail_w = content_area.width as usize;
        let bl = Line::from(Span::styled(border_ch, Style::default().fg(accent_color(s, args, Color::DarkGray))));
        f.render_widget(Paragraph::new(bl), border_area);
        let mut post_badge = build_current_rtt_spans(s, &effective_cw, &args.theme);
        if circles_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(build_target_sparkline_spans(s, args, circles_w, shared_scale));
        }
        if bar_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(trim_range_bar(build_range_bar_spans(s, args.ascii, shared_scale, &args.theme, bar_w, show_col_keys)));
        }
        let combined = build_combined_row_line(
            s, args,
            mode_label,
            &effective_cw,
            args.theme.hostname,
            shared_scale,
            tick,
            log_fmt,
            global_mode,
            badge_pad_w,
            ip_changes_slot_w,
            None,
            show_drp,
            show_dup,
            false,
            post_badge,
            false,
            true,
            gap,
            addr_gap,
        );
        let mut spans = combined.spans;
        if spans.first().map(|sp| sp.content.as_ref()) == Some("  ") {
            let trend = s.mtr_trend(args.graph_interval);
            spans[0] = Span::raw(" ");
            spans.insert(0, trend_spark_span(trend, args.ascii, &args.theme));
        }
        let combined_line = truncate_line(Line::from(spans), hdr_avail_w);
        f.render_widget(Paragraph::new(combined_line), content_area);
    }

    // Graph + x-axis - always render; dialogs are overlaid on top
    // chunks: [0]=key_names [1]=key_rule [2]=hdr [3]=graph [4]=x_axis [5]=range_bar
    let graph_area = chunks[3];
    {
        let now = Instant::now();

        // Suppress the graph during calibration - it's noisy at an unstable scale.
        // It will appear cleanly once calibration ends and the scale is locked in.
        let is_calibrating = s.calibrating.map(|(_, u)| u > now).unwrap_or(false);
        if !is_calibrating {
            render_area_graph(f, graph_area, s, args, shared_scale);
        }

        // Waiting overlay (tick-based breathing, ~8 s cycle at 1 Hz probe rate).
        let br_tick: u8 = match tick % 8 {
            0 | 7 => 200,
            1 | 6 => 150,
            2 | 5 => 100,
            _     => 70,
        };
        if s.waiting {
            let text = "awaiting response...";
            let tw   = text.len() as u16;
            let ox   = graph_area.x + (graph_area.width.saturating_sub(tw)) / 2;
            let oy   = graph_area.y + graph_area.height / 2;
            let wait_rect = Rect::new(ox, oy, tw.min(graph_area.width), 1);
            let wait_line = Line::from(Span::styled(
                text,
                Style::default()
                    .fg(Color::Rgb(br_tick, br_tick, br_tick))
                    .add_modifier(Modifier::ITALIC),
            ));
            f.render_widget(Paragraph::new(wait_line), wait_rect);
        } else if let Some((start, until)) = s.calibrating {
            if until > now {
                let br = 140u8;

                // Progress through the calibration window (0.0 → 1.0).
                let total_ms   = until.duration_since(start).as_millis().max(1) as f64;
                let elapsed_ms = now.duration_since(start).as_millis() as f64;
                let progress   = (elapsed_ms / total_ms).clamp(0.0, 1.0);

                // Countdown: ceiling so it reads "5s … 1s" then disappears.
                let secs_left  = until.duration_since(now).as_secs() + 1;

                // Progress bar: 16 block chars
                const BAR_W: usize = 16;
                let filled = (progress * BAR_W as f64).round() as usize;
                let bar: String = "█".repeat(filled) + &"░".repeat(BAR_W - filled);

                let text = format!("{}  {}  {}s", loading_message(), bar, secs_left);
                let tw   = text.chars().count() as u16;
                let ox   = graph_area.x + (graph_area.width.saturating_sub(tw)) / 2;
                let oy   = graph_area.y + graph_area.height / 2;
                let rect = Rect::new(ox, oy, tw.min(graph_area.width), 1);
                let line = Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Rgb(br, br, br))
                        .add_modifier(Modifier::ITALIC),
                ));
                f.render_widget(Paragraph::new(line), rect);
            }
        }

        // X-axis time labels - suppressed while the gathering-data overlay is shown
        if !is_calibrating {
            let xaxis_line = build_xaxis_line(s, args, graph_area.width as usize);
            f.render_widget(Paragraph::new(xaxis_line), chunks[4]);
        }
    }

    // Range bar
    let range_line = build_range_line(s, args.ascii, shared_scale, &args.theme);
    f.render_widget(Paragraph::new(range_line), chunks[5]);

    render_fullscreen_overlays(f, area, std::slice::from_ref(s), args, dialog, frozen, sort_mode, None, false, tick, !log_fmt.is_empty(), space_hidden, show_col_keys, show_headers);
}

pub fn draw_fullscreen_multi_ui(f: &mut Frame, states: &[TargetState], ctx: &ViewCtx, fs_logo: Option<&crate::ui::logo::LogoAnim>) {
    let &ViewCtx { args, mode_labels, col_widths, shared_scale, log_fmt, dialog, tick, sort_order, sort_arrows, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = f.area();
    let n    = states.len() as u16;

    let rows_per_target: u16 = if show_headers { 1 } else { 0 };

    {
        let (min_w, min_h) = super::min_size("graph", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            draw_too_small(f, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    // Build layout constraints: col-key header (optional), rows_per_target per target, graph fill, x-axis, legend
    let mut constraints: Vec<Constraint> = Vec::new();
    if show_col_keys {
        constraints.push(Constraint::Length(1)); // col-key names
        constraints.push(Constraint::Length(1)); // col-key rule
    }
    for _ in 0..(n * rows_per_target) {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(4));    // graph
    constraints.push(Constraint::Length(1)); // x-axis
    constraints.push(Constraint::Length(1)); // legend

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Shared column computations used by both col-keys header and per-target rows.
    let global_mode: Option<&str> = if mode_labels.iter().all(|m| m.as_str() == "icmp") {
        Some("icmp")
    } else {
        None
    };
    let show_badges = super::mode_badge_visible(args.column_vis.mode, mode_labels);
    let badge_pad_w = mode_labels.iter().map(|m| m.len()).max().unwrap_or(0);
    let max_ip_chg  = states.iter().map(|s| s.ip_changes).max().unwrap_or(0);
    let ip_changes_slot_w = super::ip_changes_slot_width(args.column_vis.resolve, max_ip_chg);
    let show_drp = true;
    let show_dup = show_dups_any(states, args);
    let terminal_w = area.width as usize;
    let border_ch = if args.ascii { "|" } else { "\u{258c}" }; // ▌
    let name_part_w = if col_widths.name_w > 0 { col_widths.name_w + 2 } else { 0 };
    let ideal_stats_w = col_widths.stats_width_with_gap(2, show_drp, show_dup);
    let prefix_w = 2 + badge_pad_w + 2 + name_part_w + col_widths.label + 3 + ip_changes_slot_w;
    const BAR_MIN: usize = 15;
    const BAR_MAX: usize = 25;
    let show_recent = args.extra_stats.contains(&ExtraStat::Recent);
    let show_bar    = args.extra_stats.contains(&ExtraStat::Bar);
    let content_w = terminal_w.saturating_sub(1); // accent border
    let extra = content_w.saturating_sub(prefix_w);
    let bar_reserve = if show_bar { 1 + BAR_MIN } else { 0 };
    let space_for_spark = extra.saturating_sub(ideal_stats_w + bar_reserve);
    let circles_w = if show_recent { let b = space_for_spark.saturating_sub(1); if b >= TARGET_SPARK_MIN { b.min(TARGET_SPARK_W) } else { 0 } } else { 0 };
    let spark_overhead = if circles_w > 0 { circles_w + 1 } else { 0 };
    let after_spark = extra.saturating_sub(spark_overhead);
    let bar_w = if !show_bar { 0 } else if after_spark > ideal_stats_w { (after_spark - ideal_stats_w).clamp(BAR_MIN, BAR_MAX) } else { BAR_MIN };
    let bar_overhead = if bar_w > 0 { 1 + bar_w } else { 0 };
    let stats_budget = after_spark.saturating_sub(bar_overhead);
    let effective_cw = col_widths.with_budget(stats_budget, show_drp, show_dup);
    let (addr_gap, gap) = super::pick_gaps(stats_budget + 3, &effective_cw, show_drp, show_dup);
    let space_hidden = super::compute_space_hidden(&effective_cw, show_recent, circles_w, stats_budget, gap, show_drp, show_dup);

    let col_keys_offset = if show_col_keys { 2usize } else { 0 };

    if show_col_keys {
        let badge_part = if show_badges { badge_pad_w + 3 } else { 0 };
        let anim_state = states.iter().find(|s| s.scale_anim.is_some());
        let (inline_range_label, inline_range_spans) = if bar_w > 0 { inline_range_key(anim_state, shared_scale, bar_w, args.ascii, &args.theme) } else { (String::new(), vec![]) };
        let pfx = KeysPrefix {
            lead: 3, probe: badge_part, name: name_part_w, addr: col_widths.label,
            trailer: addr_gap, resolve: ip_changes_slot_w, status: super::STATUS_BADGE_W,
            pings_gap: if circles_w > 0 { 1 } else { 0 }, pings: circles_w,
            inline_range_w: if bar_w > 0 { 1 + bar_w } else { 0 }, inline_range_label, inline_range_spans,
        };
        let hdr = build_stats_keys_line(&pfx, &effective_cw, gap, true, show_drp, show_dup, args.ascii, &args.theme);
        render_col_key_rule(f, hdr, chunks[0], chunks[1], args.ascii, &args.theme);
    }

    // Per-target header + stats rows
    if show_headers {
    for (display_pos, &slot) in sort_order.iter().enumerate() {
        if slot >= states.len() { continue; }
        let state = &states[slot];
        let (cr, cg, cb) = args.theme.target_color(slot);
        let color = Color::Rgb(cr, cg, cb);

        let sort_arrow = sort_arrows.get(slot).and_then(|&opt| opt).and_then(|(t, up)| {
            if t.elapsed().as_secs() < crate::constants::SORT_ARROW_SECS { Some(up) } else { None }
        });

        let mut post_badge = build_current_rtt_spans(state, &effective_cw, &args.theme);
        if circles_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(build_target_sparkline_spans(state, args, circles_w, shared_scale));
        }
        if bar_w > 0 {
            post_badge.push(Span::raw(" "));
            post_badge.extend(trim_range_bar(build_range_bar_spans(state, args.ascii, shared_scale, &args.theme, bar_w, show_col_keys)));
        }
        let line = build_combined_row_line(
            state, args,
            &mode_labels[slot],
            &effective_cw,
            color,
            shared_scale,
            tick,
            if display_pos == 0 { log_fmt } else { "" },
            global_mode,
            badge_pad_w,
            ip_changes_slot_w,
            sort_arrow,
            show_drp,
            show_dup,
            false,
            post_badge,
            false,
            true,
            gap,
            addr_gap,
        );
        let ac = accent_color(state, args, Color::Rgb(cr, cg, cb));
        let row_chunk = chunks[col_keys_offset + display_pos];
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(row_chunk);
        let border_area  = horiz[0];
        let content_area = horiz[1];
        let chunk_w = content_area.width as usize;
        let bl = Line::from(Span::styled(border_ch, Style::default().fg(ac)));
        f.render_widget(Paragraph::new(bl), border_area);
        let mut spans = line.spans;
        if spans.first().map(|sp| sp.content.as_ref()) == Some("  ") {
            let trend = state.mtr_trend(args.graph_interval);
            spans[0] = Span::raw(" ");
            spans.insert(0, trend_spark_span(trend, args.ascii, &args.theme));
        }
        f.render_widget(Paragraph::new(truncate_line(Line::from(spans), chunk_w)), content_area);
    }
    } // end show_headers

    // Ambient logo - top-right of header area when there are enough rows to fit it
    let header_rows = n * rows_per_target;
    if !args.ascii
        && header_rows >= crate::ui::logo::LogoAnim::HEIGHT
        && area.width >= crate::ui::logo::LogoAnim::WIDTH + 30
    {
        if let Some(logo) = fs_logo {
            let logo_x = area.x + area.width - crate::ui::logo::LogoAnim::WIDTH;
            let logo_area = ratatui::layout::Rect::new(
                logo_x, area.y,
                crate::ui::logo::LogoAnim::WIDTH,
                crate::ui::logo::LogoAnim::HEIGHT,
            );
            logo.render(f, logo_area, &args.theme);
        }
    }

    let graph_chunk_idx  = col_keys_offset + (n * rows_per_target) as usize;
    let xaxis_chunk_idx  = graph_chunk_idx + 1;
    let legend_chunk_idx = graph_chunk_idx + 2;

    // Graph
    {
        let now = Instant::now();

        // Suppress the graph while any target is still calibrating.
        let any_calibrating = states.iter().any(|s| s.calibrating.map(|(_, u)| u > now).unwrap_or(false));
        if !any_calibrating {
            render_area_graph_multi(f, chunks[graph_chunk_idx], states, args, shared_scale, sort_order);
        }

        // Calibrating overlay - shown while any target is still in its calibration window.
        // Use the latest-expiring window so the countdown reflects when all targets are ready.
        if let Some((start, until)) = states.iter()
            .filter_map(|s| s.calibrating)
            .max_by_key(|&(_, u)| u)
        {
            if until > now {
                let br = 140u8;

                let total_ms   = until.duration_since(start).as_millis().max(1) as f64;
                let elapsed_ms = now.duration_since(start).as_millis() as f64;
                let progress   = (elapsed_ms / total_ms).clamp(0.0, 1.0);
                let secs_left  = until.duration_since(now).as_secs() + 1;

                const BAR_W: usize = 16;
                let filled = (progress * BAR_W as f64).round() as usize;
                let bar: String = "█".repeat(filled) + &"░".repeat(BAR_W - filled);

                let graph_area = chunks[graph_chunk_idx];
                let text = format!("{}  {}  {}s", loading_message(), bar, secs_left);
                let tw   = text.chars().count() as u16;
                let ox   = graph_area.x + (graph_area.width.saturating_sub(tw)) / 2;
                let oy   = graph_area.y + graph_area.height / 2;
                let rect = Rect::new(ox, oy, tw.min(graph_area.width), 1);
                let line = Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Rgb(br, br, br))
                        .add_modifier(Modifier::ITALIC),
                ));
                f.render_widget(Paragraph::new(line), rect);
            }
        }

        // X-axis: suppressed while the gathering-data overlay is shown
        if !any_calibrating {
            let ref_state = states.iter().find(|s| !s.waiting).unwrap_or(&states[0]);
            let xaxis_line = build_xaxis_line(ref_state, args, chunks[graph_chunk_idx].width as usize);
            f.render_widget(Paragraph::new(xaxis_line), chunks[xaxis_chunk_idx]);
        }
    }

    // Legend row: ⇅mode  ● label  ● label  …
    let legend_line = build_legend_line(states, args, sort_order, chunks[legend_chunk_idx].width as usize);
    f.render_widget(Paragraph::new(legend_line), chunks[legend_chunk_idx]);

    render_fullscreen_overlays(f, area, states, args, dialog, frozen, sort_mode, sort_mode_changed, true, tick, !log_fmt.is_empty(), space_hidden, show_col_keys, show_headers);
}

/// Legend row shown at the bottom of multi-target fullscreen.
/// Renders "⇅mode  ● target0   ● target1   …" with each label in its target color.
/// When the full labels don't fit `available_width`, shared prefix/suffix
/// words are stripped (and, if still too wide, labels are shortened to the
/// shortest prefix that stays unique) - see `ui::labels`. Disabled by
/// `--no-condense-labels`.
pub fn build_legend_line<'a>(states: &[TargetState], args: &Args, sort_order: &[usize], available_width: usize) -> Line<'a> {
    let ascii  = args.ascii;
    let bullet = if ascii { "* " } else { "\u{25cf} " }; // ●
    let sep    = "   ";

    let slots: Vec<usize> = sort_order.iter().copied().filter(|&slot| slot < states.len()).collect();
    let display_labels: Vec<&str> = slots.iter().map(|&slot| {
        let label = &states[slot].label;
        if let Some(pos) = label.rfind(" (") {
            if label.ends_with(')') { &label[..pos] } else { &label[..] }
        } else {
            &label[..]
        }
    }).collect();

    let labels = if args.no_condense_labels {
        display_labels.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    } else {
        let overhead = bullet.chars().count() + sep.chars().count();
        super::labels::condense_for_width(&display_labels, available_width, overhead)
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (display_pos, (&slot, label)) in slots.iter().zip(labels.iter()).enumerate() {
        if display_pos > 0 { spans.push(Span::raw(sep)); }
        let (cr, cg, cb) = args.theme.target_color(slot);
        spans.push(Span::styled(bullet, Style::default().fg(Color::Rgb(cr, cg, cb))));
        spans.push(Span::styled(
            label.clone(),
            Style::default().fg(Color::Rgb(cr, cg, cb)).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}


/// Standalone range bar line (bar + scale) for the fullscreen footer.
pub fn build_range_line<'a>(s: &TargetState, ascii: bool, shared_scale: f64, theme: &Theme) -> Line<'a> {
    let mut spans = Vec::new();
    spans.push(Span::raw("  "));
    for span in build_range_bar_spans(s, ascii, shared_scale, theme, 28, false).into_iter().skip(2) {
        spans.push(span);
    }
    Line::from(spans)
}

/// X-axis time labels for the fullscreen graph.
/// Data grows left-to-right; once full it scrolls. "now" tracks the rightmost
/// real sample, time markers fan out leftward from there.
pub fn build_xaxis_line<'a>(s: &TargetState, args: &Args, total_width: usize) -> Line<'a> {
    let y_label_w: usize = 9; // must match render_area_graph
    let graph_w = total_width.saturating_sub(y_label_w);
    if graph_w == 0 { return Line::from(""); }

    let dim   = Style::default().fg(args.theme.c(args.theme.xaxis_dim));
    let white = Style::default().fg(args.theme.xaxis_now);

    // ms each output column represents - fixed at span/width regardless of
    // how much history exists (horizontal scale never changes).
    let ms_per_col = args.span_ms() as f64 / graph_w as f64;

    // Use the cache's data_end for a stable "now" column that tracks the same
    // fill level as the rendered graph (no recomputation drift).
    let data_cols  = s.graph_col_cache.borrow().data_end.min(graph_w);
    let now_col    = data_cols.saturating_sub(1);

    // Total time the visible data represents
    let total_secs = data_cols as f64 * ms_per_col / 1000.0;

    // Occupancy bitmap (true = column is taken by a label)
    let mut occupied = vec![false; graph_w];
    // Labels to render: (start_col, text, is_now)
    let mut labels: Vec<(usize, String, bool)> = Vec::new();

    // Place "now" at now_col
    let now_label = "now";
    let now_start = now_col.saturating_sub(now_label.len().saturating_sub(1));
    if now_start + now_label.len() <= graph_w {
        occupied[now_start..now_start + now_label.len()].fill(true);
        labels.push((now_start, now_label.to_string(), true));
    }

    // Time markers fanning leftward from now_col
    let marker_interval_secs: f64 = if total_secs <= 120.0 { 30.0 }
        else if total_secs <= 600.0  { 60.0 }
        else if total_secs <= 1800.0 { 300.0 }
        else { 600.0 };

    let mut t = marker_interval_secs;
    while t < total_secs - marker_interval_secs * 0.3 {
        let col_offset = (t * 1000.0 / ms_per_col) as usize;
        if col_offset > now_col { break; }
        let col = now_col - col_offset;

        let label = if t < 60.0 {
            format!("{}s", t as u64)
        } else if t < 3600.0 {
            let m = (t / 60.0) as u64;
            let sec = (t as u64) % 60;
            if sec == 0 { format!("{}m", m) } else { format!("{}m{}s", m, sec) }
        } else {
            let h = (t / 3600.0) as u64;
            let rem_min = (t as u64 % 3600) / 60;
            if rem_min == 0 { format!("{}h", h) } else { format!("{}h{}m", h, rem_min) }
        };

        let half  = label.len() / 2;
        let start = col.saturating_sub(half);
        let end   = (start + label.len()).min(graph_w);
        if end <= graph_w && !occupied[start..end].iter().any(|&c| c) {
            occupied[start..end].fill(true);
            labels.push((start, label, false));
        }

        t += marker_interval_secs;
    }

    // Sort labels by column for left-to-right rendering
    labels.sort_unstable_by_key(|&(col, _, _)| col);

    // Render
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" ".repeat(y_label_w)));

    let mut col = 0;
    let mut label_idx = 0;
    while col < graph_w {
        if label_idx < labels.len() && labels[label_idx].0 == col {
            let (_, ref text, is_now) = labels[label_idx];
            let style = if is_now { white } else { dim };
            let len = text.len();
            spans.push(Span::styled(text.clone(), style));
            col += len;
            label_idx += 1;
        } else {
            // Find next label start (or end of buffer)
            let next_label_col = labels.get(label_idx).map(|l| l.0).unwrap_or(graph_w);
            let run = next_label_col - col;
            if run > 0 {
                spans.push(Span::raw(" ".repeat(run)));
                col += run;
            }
        }
    }

    Line::from(spans)
}

#[cfg(test)]
mod single_view_layout_tests {
    use super::*;
    use crate::cli::Args;
    use clap::Parser;
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::Duration;

    fn render_single_with_args(width: u16, height: u16, extra: &[&str]) -> String {
        let mut argv = vec!["vlat", "127.0.0.1", "-v", "single"];
        argv.extend_from_slice(extra);
        let mut args = Args::parse_from(argv);
        args.theme = args.theme_name.to_theme();
        let (stats, vis) = crate::cli::resolve_columns(&args.extra_stats).unwrap();
        args.extra_stats = stats;
        args.column_vis = vis;

        let mut state = TargetState::new("127.0.0.1".to_string());
        for seq in 0..6usize {
            state.record_sent(seq);
            state.record_result(seq, Ok(12.3 + seq as f64), 0, false);
        }

        let states = vec![state];
        let col_widths = super::super::compute_col_widths(
            &states, args.is_window(), &args.extra_stats, &args.hidden_base_stats, args.ipv6, &args.column_vis, args.interval,
        );
        let mode_labels = vec!["tcp".to_string()];
        let sort_order = vec![0usize];
        let sort_arrows: Vec<Option<(Instant, bool)>> = vec![None];
        let dialog = super::super::DialogMode::None;

        let ctx = super::super::ViewCtx {
            args:              &args,
            mode_labels:       &mode_labels,
            col_widths:        &col_widths,
            shared_scale:      50.0,
            log_fmt:           "",
            tick:              0,
            dialog:            &dialog,
            sort_order:        &sort_order,
            sort_arrows:       &sort_arrows,
            sort_mode:         &args.sort,
            sort_mode_changed: None,
            frozen:            false,
            show_headers:      false,
            show_col_keys:     false,
        };

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_single_ui(f, &states, &ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// Like `render_single_with_args`, but the caller builds the target's history
    /// itself instead of getting the fixed 6-hit fixture - needed for the
    /// pre-first-reply / gradual-growth tests below.
    fn render_single_state(width: u16, height: u16, extra: &[&str], state: TargetState) -> String {
        let mut argv = vec!["vlat", "127.0.0.1", "-v", "single"];
        argv.extend_from_slice(extra);
        let mut args = Args::parse_from(argv);
        args.theme = args.theme_name.to_theme();
        let (stats, vis) = crate::cli::resolve_columns(&args.extra_stats).unwrap();
        args.extra_stats = stats;
        args.column_vis = vis;

        let states = vec![state];
        let col_widths = super::super::compute_col_widths(
            &states, args.is_window(), &args.extra_stats, &args.hidden_base_stats, args.ipv6, &args.column_vis, args.interval,
        );
        let mode_labels = vec!["tcp".to_string()];
        let sort_order = vec![0usize];
        let sort_arrows: Vec<Option<(Instant, bool)>> = vec![None];
        let dialog = super::super::DialogMode::None;

        let ctx = super::super::ViewCtx {
            args:              &args,
            mode_labels:       &mode_labels,
            col_widths:        &col_widths,
            shared_scale:      50.0,
            log_fmt:           "",
            tick:              0,
            dialog:            &dialog,
            sort_order:        &sort_order,
            sort_arrows:       &sort_arrows,
            sort_mode:         &args.sort,
            sort_mode_changed: None,
            frozen:            false,
            show_headers:      false,
            show_col_keys:     false,
        };

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_single_ui(f, &states, &ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn bordered_row_count(out: &str, width: u16) -> usize {
        let border = '\u{258c}';
        out.lines().filter(|l| l.starts_with(border)).count().min(width as usize)
    }

    #[test]
    fn waiting_for_first_reply_renders_a_single_compact_line() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        // Three probes in flight, none resolved yet - last_up is still None.
        state.record_sent(0);
        state.record_sent(1);
        state.record_sent(2);

        let out = render_single_state(100, 20, &[], state);
        let non_blank: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_blank.len(), 1, "only one line should render while waiting for the first reply: {non_blank:?}");
        assert!(non_blank[0].contains("3 probes"), "should show the in-flight probe count: {:?}", non_blank[0]);
        assert!(!non_blank[0].contains("DOWN"), "should not claim DOWN before any probe has timed out: {:?}", non_blank[0]);
    }

    #[test]
    fn timed_out_probe_before_first_reply_shows_down() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Err(()), 0, false); // times out - still never had a successful reply

        let out = render_single_state(100, 20, &[], state);
        let non_blank: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_blank.len(), 1, "still just one compact line once down, not the full row layout: {non_blank:?}");
        assert!(non_blank[0].contains("DOWN"), "should show DOWN once a probe has timed out with no reply ever: {:?}", non_blank[0]);
        assert!(non_blank[0].contains("no reply, 1 probe, "), "should read as \"no reply, N probes, <elapsed>\": {:?}", non_blank[0]);
    }

    #[test]
    fn history_rows_grow_one_at_a_time_after_first_reply() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false); // one real sample - last_up now Some

        let extra = ["--history-rows", "10"];
        let (width, height) = (100u16, 20u16);
        let out = render_single_state(width, height, &extra, state.clone());
        assert_eq!(bordered_row_count(&out, width), 2, "1 history row + the name/address row above it, not the full 10");

        state.record_sent(1);
        state.record_result(1, Ok(13.0), 0, false);
        let out2 = render_single_state(width, height, &extra, state);
        assert_eq!(bordered_row_count(&out2, width), 3, "a second real sample should grow the block by exactly one row");
    }

    #[test]
    fn history_still_grows_gradually_after_a_long_down_backlog() {
        // A target that was down for a while before finally answering already has
        // a pile of resolved (Drop) samples in history by the time the first Hit
        // lands. n_hist must not count that backlog - it should grow exactly the
        // same way as history_rows_grow_one_at_a_time_after_first_reply, not jump
        // straight to the full --history-rows count the instant it answers.
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        for seq in 0..30usize {
            state.record_sent(seq);
            state.record_result(seq, Err(()), 0, false);
        }
        state.record_sent(30);
        state.record_result(30, Ok(12.3), 0, false); // finally answers

        let extra = ["--history-rows", "10"];
        let (width, height) = (100u16, 20u16);
        let out = render_single_state(width, height, &extra, state.clone());
        assert_eq!(bordered_row_count(&out, width), 2, "should still start at 1 history row despite the 30-drop backlog, not jump to 10: {out:?}");

        state.record_sent(31);
        state.record_result(31, Ok(13.0), 0, false);
        let out2 = render_single_state(width, height, &extra, state);
        assert_eq!(bordered_row_count(&out2, width), 3, "a second reply since answering should grow the block by exactly one row");
    }

    #[test]
    fn status_line_does_not_shift_as_history_fills_in() {
        // The recent-bar/stats/status block must paint at its final row position
        // the moment nominal view is entered (the first reply), and stay there -
        // not migrate down the screen reply by reply as the history block above it
        // fills in, which would read as the terminal scrolling.
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);

        let extra = ["--history-rows", "10"];
        let (width, height) = (100u16, 20u16);
        let status_row = |out: &str| out.lines().position(|l| l.contains(" probe")).expect("status line present");

        let out = render_single_state(width, height, &extra, state.clone());
        let first_row = status_row(&out);

        state.record_sent(1);
        state.record_result(1, Ok(13.0), 0, false);
        let out2 = render_single_state(width, height, &extra, state.clone());
        assert_eq!(status_row(&out2), first_row, "status line moved after the second reply: {out2:?}");

        for seq in 2..8usize {
            state.record_sent(seq);
            state.record_result(seq, Ok(12.0 + seq as f64), 0, false);
        }
        let out3 = render_single_state(width, height, &extra, state);
        assert_eq!(status_row(&out3), first_row, "status line moved while the history block kept filling in: {out3:?}");
    }

    #[test]
    fn bar_and_stats_rows_wait_for_a_second_sample() {
        // The default columns include the range bar and the base stats (avg/range/
        // jitter/loss), which are meaningless (or literally just one flat point)
        // with a single sample - they should stay off screen until reply #2 so the
        // transition out of the compact waiting-line isn't 1 line -> everything at once.
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);

        let (width, height) = (100u16, 20u16);
        let out = render_single_state(width, height, &[], state.clone());
        assert!(!out.contains("avg"), "stats row shouldn't render from a single sample: {out:?}");

        state.record_sent(1);
        state.record_result(1, Ok(13.0), 0, false);
        let out2 = render_single_state(width, height, &[], state);
        assert!(out2.contains("avg"), "stats row should appear once a second sample lands: {out2:?}");
    }

    #[test]
    fn no_return_line_reads_no_response_once_down() {
        // Never had a successful reply, so there's no "last seen" reference point -
        // the text should read "no reply, N probes, <elapsed>" instead.
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Err(()), 0, false);

        let out = render_single_state(100, 20, &[], state);
        let non_blank: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_blank.len(), 1);
        assert!(non_blank[0].contains("no reply, 1 probe, "), "should read \"no reply, N probes, <elapsed>\": {:?}", non_blank[0]);
        assert!(!non_blank[0].contains("last seen"), "never having been up, must not say \"last seen\": {:?}", non_blank[0]);
    }

    #[test]
    fn nominal_status_line_shows_down_note_once_past_the_threshold() {
        // Had a successful reply before, then dropped - "down" measures time since
        // that last successful reply. Suppressed until the drop is old enough to be
        // worth a note (backdate last_up here to get past that threshold).
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);
        state.record_sent(1);
        state.record_result(1, Ok(13.0), 0, false);
        state.record_sent(2);
        state.record_result(2, Err(()), 0, false); // now down, having been up before
        state.last_up = state.last_up.map(|t| t - Duration::from_secs(30));

        let out = render_single_state(100, 20, &[], state);
        assert!(out.contains("down 30s"), "should append a \"down <elapsed>\" note once down long enough: {out:?}");
        assert!(!out.contains("last seen") && !out.contains("last down"), "{out:?}");
    }

    #[test]
    fn nominal_status_line_suppresses_fresh_down_note() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);
        state.record_sent(1);
        state.record_result(1, Err(()), 0, false); // just went down

        let out = render_single_state(100, 20, &[], state);
        assert!(!out.contains("down "), "a drop that just happened shouldn't get a \"down\" callout yet: {out:?}");
    }

    #[test]
    fn nominal_status_line_shows_down_word_not_the_xx_badge() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);
        state.record_sent(1);
        state.record_result(1, Err(()), 0, false); // now down

        let out = render_single_state(100, 20, &[], state);
        let status_line = out.lines().find(|l| l.contains("2 probes,")).expect("status line present");
        assert!(status_line.contains("DOWN"), "should show the \"DOWN\" word, matching the pre-first-reply line: {status_line:?}");
        assert!(!status_line.contains('\u{2717}'), "should not also show the compact \u{2717}\u{2717} badge: {status_line:?}");
    }

    #[test]
    fn nominal_status_line_omits_down_suffix_while_up() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);
        state.record_sent(1);
        state.record_result(1, Ok(13.0), 0, false);

        let out = render_single_state(100, 20, &[], state);
        assert!(!out.contains("last seen") && !out.contains("last down"), "no down suffix while currently up: {out:?}");
    }

    #[test]
    fn status_column_is_opt_in_and_shares_the_status_line_text() {
        let mut state = TargetState::new("127.0.0.1".to_string());
        state.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        state.record_sent(0);
        state.record_result(0, Ok(12.3), 0, false);
        state.record_sent(1);
        state.record_result(1, Ok(13.0), 0, false);

        // Disabled by default - "N probes," should appear exactly once, from the
        // dedicated status line, not a second time from a `status` stats row.
        let out_default = render_single_state(100, 20, &[], state.clone());
        assert_eq!(out_default.matches("2 probes,").count(), 1, "status column must be off by default: {out_default:?}");

        // Explicitly enabled ("default" composes in the base set + status): a
        // second "N probes, <elapsed>" now also appears in the stats row.
        let out_on = render_single_state(100, 20, &["--columns", "default,status"], state);
        assert_eq!(out_on.matches("2 probes,").count(), 2, "enabling --columns status should add a second copy in the stats row: {out_on:?}");
        assert!(out_on.contains("status"), "the stats-row word label should read \"status\": {out_on:?}");
    }

    #[test]
    fn history_leads_status_then_stats_then_the_recent_bar_closes_it_out() {
        // --history-rows defaults to 0 now (adjustable via Up/Down), so this
        // test - specifically about history-row/status/stats/bar ordering -
        // asks for some history rows explicitly rather than relying on render_single.
        // The recent-trend bar is already in the default --columns set.
        let out = render_single_with_args(100, 20, &["--history-rows", "10"]);
        let lines: Vec<&str> = out.lines().collect();

        // First line is now a history row (no separate name/address row above it).
        assert!(lines[0].starts_with('\u{258c}'), "first line should be a history row: {:?}", lines[0]);

        let non_blank: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        // Bottom three rows are, in order: the combined name/address + status line,
        // the stats line, then the large recent-trend bar (swapped from the old
        // bar/stats/status order so the name/status line follows straight after
        // the history rows).
        let status = non_blank[non_blank.len() - 3];
        let stats  = non_blank[non_blank.len() - 2];
        let bar    = non_blank[non_blank.len() - 1];
        assert!(!status.contains("avg"), "third-to-last line should be the combined name/status line: {:?}", status);
        assert!(stats.contains("avg"), "second-to-last line should be the stats line: {:?}", stats);
        assert!(!bar.contains("avg"), "last line should be the recent-trend bar: {:?}", bar);
        assert!(status.contains("probe"), "name/status line should be the probe-summary line: {:?}", status);
        assert!(status.contains("tcp"), "name/status line should also carry the name/address header (mode badge here): {:?}", status);
        let lead = |s: &str| s.chars().take_while(|c| *c == ' ').count();
        assert_eq!(lead(bar), 2, "recent bar indent: {:?}", bar);
        assert_eq!(lead(stats), 2, "stats line indent: {:?}", stats);
        assert!(status.starts_with('\u{258c}'), "combined name/status line starts right at the border, not indented: {:?}", status);
    }

    #[test]
    fn many_columns_wrap_onto_additional_stats_rows() {
        let out = render_single_with_args(60, 20, &["--columns", "all"]);
        let lines: Vec<&str> = out.lines().collect();

        // A stats row is one containing any of the (word-form) stat labels. With
        // `--columns all` there are far more labels than fit on one 60-wide row, so
        // they must be spread across several rows rather than truncated onto one.
        let labels = ["avg ", "range ", "jitter ", "loss ", "mtr ", "std ", "p01 ", "p10 ", "p50 ", "p95 ", "p99 ", "cv ", "srtt ", "streak ", "last "];
        let is_stats_row = |l: &&str| labels.iter().any(|lbl| l.contains(lbl));
        let stats_lines: Vec<&str> = lines.iter().copied().filter(is_stats_row).collect();
        assert!(stats_lines.len() >= 2, "expected the enabled columns to wrap onto 2+ rows: {:?}", lines);

        // Every enabled stat must actually appear exactly once - none silently dropped.
        for lbl in labels {
            let count = lines.iter().filter(|l| l.contains(lbl)).count();
            assert_eq!(count, 1, "label {:?} should appear exactly once across the wrapped rows: {:?}", lbl, lines);
        }

        // All wrapped stats rows keep the shared 2-space indent.
        let lead = |s: &str| s.chars().take_while(|c| *c == ' ').count();
        for l in &stats_lines {
            assert_eq!(lead(l), 2, "wrapped stats row should keep the 2-space indent: {:?}", l);
        }

        // avg (first stat) must appear no later than streak (the last extra stat) in reading order.
        let avg_idx    = lines.iter().position(|l| l.contains("avg ")).unwrap();
        let streak_idx = lines.iter().position(|l| l.contains("streak ")).unwrap();
        assert!(avg_idx <= streak_idx, "stats should stay in display order across wrapped rows");
    }

    #[test]
    fn single_history_avail_matches_what_actually_renders() {
        // With --history-rows set far past anything that could fit, every allocated
        // history row actually renders real content (not the "continue"-blanked
        // filler rows draw_single_ui uses when there isn't enough sample history
        // yet) as long as there are at least `avail` samples recorded. That lets us
        // count real rendered history lines and compare against what
        // `single_history_avail` reports - the same number the Up/Down key handler
        // uses to know when to stop growing `--history-rows` further.
        let (width, height) = (100u16, 20u16);

        let mut args = Args::parse_from(vec!["vlat", "127.0.0.1", "-v", "single", "--history-rows", "999"]);
        args.theme = args.theme_name.to_theme();
        let (stats, vis) = crate::cli::resolve_columns(&args.extra_stats).unwrap();
        args.extra_stats = stats;
        args.column_vis = vis;
        let mut state = TargetState::new("127.0.0.1".to_string());
        for seq in 0..50usize {
            state.record_sent(seq);
            state.record_result(seq, Ok(12.3 + seq as f64), 0, false);
        }
        let states = vec![state];
        let col_widths = super::super::compute_col_widths(
            &states, args.is_window(), &args.extra_stats, &args.hidden_base_stats, args.ipv6, &args.column_vis, args.interval,
        );
        let area = Rect { x: 0, y: 0, width, height };
        let avail = single_history_avail(area, &states, &args, &col_widths);
        assert!(avail > 0 && (avail as u16) < height, "avail should be well within the terminal height: {avail}");

        let mode_labels = vec!["tcp".to_string()];
        let sort_order = vec![0usize];
        let sort_arrows: Vec<Option<(Instant, bool)>> = vec![None];
        let dialog = super::super::DialogMode::None;
        let ctx = super::super::ViewCtx {
            args: &args, mode_labels: &mode_labels, col_widths: &col_widths, shared_scale: 50.0,
            log_fmt: "", tick: 0, dialog: &dialog, sort_order: &sort_order, sort_arrows: &sort_arrows,
            sort_mode: &args.sort, sort_mode_changed: None, frozen: false, show_headers: false, show_col_keys: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_single_ui(f, &states, &ctx)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let border = '\u{258c}';
        // Every history row (and the name row above them) leads with the accent
        // border in column 0; the bar/stats/status rows below are indented instead.
        let bordered_rows = (0..buf.area.height).filter(|&y| buf[(0, y)].symbol() == border.to_string()).count();
        let rendered_history_rows = bordered_rows - 1; // minus the name/address row

        assert_eq!(rendered_history_rows, avail, "avail should equal what actually rendered");

        // Growing the terminal grows the reported max in lock-step.
        let taller = Rect { x: 0, y: 0, width, height: height + 5 };
        let avail_taller = single_history_avail(taller, &states, &args, &col_widths);
        assert_eq!(avail_taller, avail + 5, "avail should grow with the available terminal rows");
    }
}
