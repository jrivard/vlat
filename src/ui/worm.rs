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
use std::time::Duration;
use crate::time::Instant;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use crate::state::TargetState;
use super::{AxisMetric, ViewCtx};
use super::{
    compute_no_data,
    dialogs::{DialogMode, draw_help_dialog, draw_explain_dialog, draw_warning_dialog, draw_filename_dialog, draw_frozen_notice, draw_freeze_notice_dialog, draw_sort_notice_dialog, draw_sort_picker_dialog, draw_theme_notice_dialog, draw_theme_picker_dialog, draw_view_notice_dialog, draw_view_picker_dialog, draw_save_defaults_dialog, draw_window_input_dialog, draw_metric_picker_dialog},
    layout::{build_legend_line, loading_message, render_window_label},
};

// ── worm appearance ───────────────────────────────────────────────────────────
// CP437: 219=█  178=▓  177=▒  176=░  (exact worm_chars[] from original)
const WORM_CHARS: [&str; 4] = ["█", "▓", "▒", "░"];

// Each worm cell is 2 terminal columns wide, matching Merkey's x[n] and x[n]+1 draws.
const CELL_W: i16 = 2;

const WORM_MIN_LEN: usize = 4;
const WORM_MAX_LEN: usize = 36;

const MIN_LEN: usize = WORM_MIN_LEN;
const MAX_LEN: usize = WORM_MAX_LEN;

const STEP_MS_SLOW: u64 = 1400; // slowest (zero jitter)
const STEP_MS_FAST: u64 = 160;  // fastest (max jitter)

// ── direction encoding (Merkey's convention) ──────────────────────────────────
// 0=down  1=SE  2=right  3=NE  4=up  5=NW  6=left  7=SW
fn dir_delta(dir: u8) -> (i16, i16) {
    match dir {
        0 => (0,       1),
        1 => (1,       1),
        2 => (CELL_W,  0),
        3 => (1,      -1),
        4 => (0,      -1),
        5 => (-1,     -1),
        6 => (-CELL_W, 0),
        7 => (-1,      1),
        _ => (0,       1),
    }
}

// ── character selection (Merkey's div/mod formula) ────────────────────────────
fn char_index(n: usize, length: usize) -> usize {
    let div  = length / 4;
    let mod_ = length % 4;
    if div == 0 { return 0; }
    let c = if n < (div + 1) * mod_ { n / (div + 1) } else { (n - mod_) / div };
    c % 4
}

// ── types ─────────────────────────────────────────────────────────────────────
#[derive(Clone)]
struct Worm {
    segs:          VecDeque<(u16, u16)>,
    dir:           u8,
    /// Current drawn length (grows/shrinks toward target_length).
    length:        usize,
    /// Desired length based on jitter.
    target_length: usize,
    runlength:     u16,
    next_step_at:  Instant,
    /// Random phase offset (0–3999 ms) so each worm's X-flash is desynchronized.
    flash_offset_ms: u64,
}

#[derive(Clone)]
pub struct WormState {
    worms: Vec<Worm>,
    rng:   u64,
    /// Metric that drives worm speed (faster = higher) and length (longer =
    /// higher). Selected via the in-app metric picker ('a') or --worm-metric;
    /// defaults to jitter. Selection applies live - see `set_metric`.
    pub metric: AxisMetric,
}

impl WormState {
    pub fn new(n: usize, w: u16, h: u16) -> Self {
        Self::with_metric(n, w, h, AxisMetric::Jitter)
    }

    /// Like `new()`, but starting on the given metric instead of the jitter
    /// default - used to seed the view from `--worm-metric`.
    pub fn with_metric(n: usize, w: u16, h: u16, metric: AxisMetric) -> Self {
        let seed = crate::time::SystemTime::now()
            .duration_since(crate::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEAD_BEEF_1337_CAFE);
        let mut rng = seed;
        let worms = (0..n).map(|_| mk_worm(&mut rng, w, h)).collect();
        WormState { worms, rng, metric }
    }

    pub fn set_metric(&mut self, m: AxisMetric) { self.metric = m; }

    pub fn step(&mut self, states: &[TargetState], global_max_metric: f64, w: u16, h: u16) {
        if w == 0 || h == 0 { return; }

        while self.worms.len() < states.len() {
            let worm = mk_worm(&mut self.rng, w, h);
            self.worms.push(worm);
        }
        self.worms.truncate(states.len());

        let now    = Instant::now();
        let metric = self.metric;

        for (i, worm) in self.worms.iter_mut().enumerate() {
            if now < worm.next_step_at { continue; }

            let drop_flash   = states.get(i).map_or(0u8, |s| s.drop_flash);
            // "All recent drops": window has no successes but drops exist → 100% loss.
            let all_dropping = states.get(i).is_some_and(|s| {
                s.window.is_empty() && s.win_drops > 0
            });

            // Stopped: 100% recent loss rate - one step every 10 seconds.
            if all_dropping {
                step_worm(worm, w, h, &mut self.rng);
                worm.next_step_at = now + Duration::from_millis(60_000);
                continue;
            }

            // Update target length: recoil to minimum on a drop (D), otherwise track the selected metric (A+D).
            let dropping = states.get(i).is_some_and(|s| s.last_was_drop);
            worm.target_length = if dropping {
                WORM_MIN_LEN
            } else {
                target_len(states.get(i), global_max_metric, metric)
            };
            if worm.length < worm.target_length {
                worm.length += 1;
            } else if worm.length > worm.target_length {
                worm.length -= 1;
            }
            worm.length = worm.length.max(WORM_MIN_LEN);

            step_worm(worm, w, h, &mut self.rng);

            // Slow down proportionally to drop_flash (6 = just dropped → 4× slower).
            let step_ms = step_interval_ms(states.get(i), global_max_metric, metric);
            let step_ms = if drop_flash > 0 {
                let factor = 1.0 + (drop_flash as f64 / 6.0) * 3.0; // 1×–4× based on recency
                (step_ms as f64 * factor).round() as u64
            } else {
                step_ms
            };
            worm.next_step_at = now + Duration::from_millis(step_ms);
        }
    }
}

// ── worm movement (Merkey's exact logic) ──────────────────────────────────────

fn mk_worm(rng: &mut u64, w: u16, h: u16) -> Worm {
    let cols = w.max(4) as u64;
    let rows = h.max(4) as u64;
    let col  = (rng_next(rng) % (cols - 1)) as u16;
    let row  = (rng_next(rng) % rows) as u16;
    // Start on a cardinal direction (even), matching Merkey's init.
    let dir  = (((rng_next(rng) % 9) >> 1) << 1) as u8 % 8;

    let mut segs = VecDeque::new();
    for _ in 0..WORM_MIN_LEN { segs.push_back((col, row)); }

    Worm {
        segs,
        dir,
        length:          WORM_MIN_LEN,
        target_length:   WORM_MIN_LEN,
        runlength:       WORM_MIN_LEN as u16,
        next_step_at:    Instant::now(),
        flash_offset_ms: rng_next(rng) % 4000,
    }
}

fn step_worm(worm: &mut Worm, w: u16, h: u16, rng: &mut u64) {
    let (dx, dy) = dir_delta(worm.dir);
    let head  = *worm.segs.front().unwrap();
    let mut nx  = head.0 as i16 + dx;
    let mut ny  = head.1 as i16 + dy;
    let mut dir = worm.dir as i16;

    let max_x = w as i16 - CELL_W;
    let max_y = h as i16 - 1;

    // ── boundary bouncing (Merkey's exact dir ± 4 logic) ─────────────────────
    if nx < 0 && worm.dir >= 5 {
        nx = 1; dir -= 4;
    } else if ny < 0 && (worm.dir >= 3 && worm.dir <= 5) {
        ny = 1; dir -= 4;
    } else if nx >= max_x && (worm.dir >= 1 && worm.dir <= 3) {
        nx = max_x; dir += 4;
    } else if ny >= h as i16 && (worm.dir == 7 || worm.dir == 0 || worm.dir == 1) {
        ny = max_y; dir += 4;
    }
    // ── voluntary direction changes ───────────────────────────────────────────
    else if worm.runlength == 0 {
        let rnd = rng_next(rng) % 128;
        if      rnd > 90 { dir += 2; }   // ~29% chance: 90° turn
        else if rnd == 1 { dir += 1; }
        else if rnd == 2 { dir -= 1; }
        worm.runlength = worm.length as u16;
    } else {
        worm.runlength -= 1;
        let rnd = rng_next(rng) % 128;
        if      rnd == 1 { dir += 1; }
        else if rnd == 2 { dir -= 1; }
    }

    dir = ((dir % 8) + 8) % 8;
    worm.dir = dir as u8;

    let nx = (nx.max(0) as u16).min(w.saturating_sub(CELL_W as u16));
    let ny = (ny.max(0) as u16).min(h.saturating_sub(1));

    worm.segs.push_front((nx, ny));
    while worm.segs.len() > worm.length { worm.segs.pop_back(); }
}

fn rng_next(s: &mut u64) -> u64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *s >> 17
}

// ── metric helpers ───────────────────────────────────────────────────────────

fn metric_norm(s: Option<&TargetState>, global_max: f64, metric: AxisMetric) -> f64 {
    if global_max <= 0.0 { return 0.0; }
    let v = s.map(|t| if t.waiting { 0.0 } else { metric.value(t, true) }).unwrap_or(0.0);
    (v / global_max).clamp(0.0, 1.0)
}

fn step_interval_ms(s: Option<&TargetState>, global_max: f64, metric: AxisMetric) -> u64 {
    let norm = metric_norm(s, global_max, metric);
    (STEP_MS_FAST as f64 + norm * (STEP_MS_SLOW - STEP_MS_FAST) as f64).round() as u64
}

fn target_len(s: Option<&TargetState>, global_max: f64, metric: AxisMetric) -> usize {
    let norm = metric_norm(s, global_max, metric);
    (MIN_LEN as f64 + norm * (MAX_LEN - MIN_LEN) as f64).round() as usize
}

// ── public draw entry point ───────────────────────────────────────────────────

pub fn draw_worm(
    frame:        &mut Frame,
    states:       &[TargetState],
    nw:           &WormState,
    ctx:   &ViewCtx,
) {
    let &ViewCtx { args, mode_labels, col_widths, log_fmt, tick, dialog, sort_mode, sort_mode_changed, frozen, show_headers, show_col_keys, .. } = ctx;
    let area = frame.area();
    let n = states.len() as u16;
    {
        let (min_w, min_h) = super::min_size("worm", states.len(), show_col_keys, show_headers, col_widths, mode_labels, args.column_vis.mode);
        if area.width < min_w || area.height < min_h {
            super::layout::draw_too_small(frame, area.width, area.height, min_w, min_h, &args.theme);
            return;
        }
    }

    let rows_per_target: u16 = if !show_headers { 0 } else { 1 };

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
    let worm_chunk_idx   = col_keys_offset + header_rows;
    let legend_chunk_idx = col_keys_offset + header_rows + 1;

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

    let worm_area = chunks[worm_chunk_idx];
    let buf = frame.buffer_mut();

    // Wall-clock milliseconds, captured once per frame.
    let now_ms = {
        use crate::time::SystemTime;
        SystemTime::now()
            .duration_since(crate::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    };

    // Draw tail-first so the head renders on top when worms overlap.
    let any_calibrating = states.iter().any(|s| s.calibrating.map(|(_, u)| u > Instant::now()).unwrap_or(false));
    if !any_calibrating {
        for (i, worm) in nw.worms.iter().enumerate() {
            if i >= states.len() { break; }
            let (r, g, b)  = args.theme.target_color(i);
            let drop_segs  = states[i].drop_flash as usize;
            let length     = worm.segs.len();
            if length < WORM_MIN_LEN { continue; }

            let blink_x = ((now_ms + worm.flash_offset_ms) / 1000) % 4 == 0;

            for (n, &(sx, sy)) in worm.segs.iter().enumerate().rev() {
                if sy >= worm_area.height { continue; }

                let (ch, style) = if n < drop_segs && blink_x {
                    ("X", Style::default().fg(Color::Red).bg(Color::Black))
                } else {
                    (WORM_CHARS[char_index(n, length)],
                     Style::default().fg(Color::Rgb(r, g, b)).bg(Color::Black))
                };

                for cw in 0u16..CELL_W as u16 {
                    let px = sx + cw;
                    if px < worm_area.width {
                        let cell = &mut buf[(worm_area.x + px, worm_area.y + sy)];
                        cell.set_symbol(ch);
                        cell.set_style(style);
                    }
                }
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
                let br = 140u8;
                let total_ms   = until.duration_since(start).as_millis().max(1) as f64;
                let elapsed_ms = now.duration_since(start).as_millis() as f64;
                let progress   = (elapsed_ms / total_ms).clamp(0.0, 1.0);
                let secs_left  = until.duration_since(now).as_secs() + 1;

                const BAR_W: usize = 16;
                let filled = (progress * BAR_W as f64).round() as usize;
                let bar: String = "█".repeat(filled) + &"░".repeat(BAR_W - filled);

                let text = format!("{}  {}  {}s", loading_message(), bar, secs_left);
                let tw   = text.chars().count() as u16;
                let ox   = worm_area.x + (worm_area.width.saturating_sub(tw)) / 2;
                let oy   = worm_area.y + worm_area.height / 2;
                let rect = Rect::new(ox, oy, tw.min(worm_area.width), 1);
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

    // ── dialog overlay ────────────────────────────────────────────────────────
    match dialog {
        DialogMode::None => {}
        DialogMode::Warning { message, dismiss_at } => {
            let secs_left = dismiss_at.saturating_duration_since(Instant::now()).as_secs();
            draw_warning_dialog(frame, area, message, secs_left, args.ascii, &args.theme);
        }
        DialogMode::Help { page, scroll, dismiss_at, logo, cursor, sub_menu, collapsed } => {
            let secs_left = dismiss_at.saturating_duration_since(Instant::now()).as_secs();
            let sort_name = if states.len() <= 1 { "n/a" } else { sort_mode.as_str() };
            let extra_cols_on = !args.extra_stats.is_empty();
            draw_help_dialog(frame, area, *page, *scroll, args.ascii, &args.theme, secs_left, sort_name, frozen, "worm", logo, args.window, *cursor, sub_menu.as_ref(), !log_fmt.is_empty(), show_col_keys, show_headers, extra_cols_on, collapsed, states.len());
        }
        DialogMode::Explain { scroll } => {
            draw_explain_dialog(frame, area, *scroll, args.ascii, &args.theme);
        }
        DialogMode::FilenameInput { format, input } => {
            draw_filename_dialog(frame, area, format, input, &args.theme);
        }
        DialogMode::FreezeNotice { dismiss_at, now_frozen } => {
            let secs_left = dismiss_at.saturating_duration_since(Instant::now()).as_secs();
            draw_freeze_notice_dialog(frame, area, args.ascii, &args.theme, *now_frozen, secs_left);
        }
        DialogMode::SortNotice { dismiss_at, sort_mode: mode } => {
            let secs_left = dismiss_at.saturating_duration_since(Instant::now()).as_secs();
            draw_sort_notice_dialog(frame, area, args.ascii, &args.theme, mode, secs_left);
        }
        DialogMode::ThemeNotice { dismiss_at, theme_name } => {
            let secs_left = dismiss_at.saturating_duration_since(Instant::now()).as_secs();
            draw_theme_notice_dialog(frame, area, args.ascii, &args.theme, theme_name, secs_left);
        }
        DialogMode::ViewNotice { dismiss_at, view_name, view_desc } => {
            let secs_left = dismiss_at.saturating_duration_since(Instant::now()).as_secs();
            draw_view_notice_dialog(frame, area, args.ascii, &args.theme, view_name, view_desc, secs_left);
        }
        DialogMode::SaveDefaults { view_name, theme_name, sort_name, config_path,
                                   save_view, save_theme, save_sort,
                                   save_keys, save_window, save_cols,
                                   cursor, keys_current, window_current, cols_delta,
                                   file_view, file_theme, file_sort,
                                   file_keys, file_window, file_cols, .. } => {
            draw_save_defaults_dialog(frame, area, *view_name, theme_name, sort_name, config_path,
                                      *save_view, *save_theme, *save_sort,
                                      *save_keys, *save_window, *save_cols,
                                      *cursor, *keys_current, *window_current, cols_delta,
                                      file_view.as_deref(), file_theme.as_deref(), file_sort.as_deref(),
                                      file_keys.as_deref(), file_window.as_deref(), file_cols.as_deref(),
                                      args.ascii, &args.theme);
        }
        DialogMode::WindowInput { input } => {
            draw_window_input_dialog(frame, area, input, args.window, &args.theme);
        }
        DialogMode::StatColumnToggle { cursor, identity } => {
            let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
            super::dialogs::draw_stat_column_toggle_dialog(frame, area, *cursor, identity, &args.extra_stats, &args.hidden_base_stats, 0, no_data, args.ascii, &args.theme);
        }
        DialogMode::SortPicker { cursor, .. } => {
            draw_sort_picker_dialog(frame, area, args.ascii, &args.theme, *cursor, args.reverse_sort);
        }
        DialogMode::ThemePicker { cursor, .. } => {
            draw_theme_picker_dialog(frame, area, args.ascii, &args.theme, *cursor);
        }
        DialogMode::ViewPicker { cursor, .. } => {
            draw_view_picker_dialog(frame, area, args.ascii, &args.theme, *cursor, states.len());
        }
        DialogMode::MetricPicker { cursor } => {
            draw_metric_picker_dialog(frame, area, args.ascii, &args.theme, *cursor, "worm");
        }
        // Scatter-only dialog; unreachable here, kept for exhaustiveness.
        DialogMode::AxisPicker { .. } => {}
    }
    // ── frozen scrolling banner (drawn last so it sits on top of any dialog) ──
    if frozen { draw_frozen_notice(frame, area, args.ascii, &args.theme, tick); }
}
