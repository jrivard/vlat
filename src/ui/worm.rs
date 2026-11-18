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
    dialogs::{DialogMode, draw_help_dialog, draw_explain_dialog, draw_about_dialog, draw_warning_dialog, draw_filename_dialog, draw_frozen_notice, draw_freeze_notice_dialog, draw_sort_notice_dialog, draw_sort_picker_dialog, draw_theme_notice_dialog, draw_theme_picker_dialog, draw_view_notice_dialog, draw_view_picker_dialog, draw_save_defaults_dialog, draw_window_input_dialog, draw_metric_picker_dialog},
    layout::{loading_message, render_window_label},
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
    // Column keys label the per-target rows, so they hide along with them.
    let col_keys = super::col_keys_visible("worm", show_col_keys, show_headers);
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

    let mut constraints: Vec<Constraint> = Vec::new();
    if col_keys {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    for _ in 0..(n * rows_per_target) {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    let no_data_row = super::no_data::no_data_row_shown(states, show_headers);
    if no_data_row { constraints.push(Constraint::Length(1)); } // no-data strip

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let col_keys_offset  = if col_keys { 2usize } else { 0 };
    let header_rows      = (n * rows_per_target) as usize;
    let worm_chunk_idx   = col_keys_offset + header_rows;

    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let col_key_areas = if col_keys { Some((chunks[0], chunks[1])) } else { None };
    super::render_screensaver_headers(frame, states, ctx, &chunks[col_keys_offset..], true, col_key_areas);

    let stat_cols_shown = !super::dialogs::columns_hidden_by_headers("worm", show_headers);
    render_window_label(frame, area, states, args, sort_mode, sort_mode_changed, stat_cols_shown);

    let worm_area = chunks[worm_chunk_idx];
    let buf = frame.buffer_mut();
    if no_data_row {
        let avoid = super::layout::window_label_rect(area, states, args, sort_mode, stat_cols_shown);
        super::no_data::render_no_data_row(buf, chunks[worm_chunk_idx + 1], states, ctx.sort_order, args, tick, Some(avoid));
    }

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
            // Waiting and no-data targets aren't drawn (see `no_data`).
            if super::no_data::hidden_from_plot(&states[i]) { continue; }
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
        draw_worm_labels(buf, worm_area, states, nw, args, show_headers, now_ms);
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
            draw_help_dialog(frame, area, *page, *scroll, args.ascii, &args.theme, secs_left, sort_name, frozen, "worm", logo, args.window, *cursor, sub_menu.as_ref(), !log_fmt.is_empty(), show_col_keys, show_headers, extra_cols_on, collapsed, states.len(), args.max_range);
        }
        DialogMode::Explain { scroll } => {
            draw_explain_dialog(frame, area, *scroll, args.ascii, &args.theme);
        }
        DialogMode::About { logo, command } => {
            draw_about_dialog(frame, area, logo, command, &args.input_files, args.ascii, &args.theme);
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
                                   save_keys, save_window, save_scale, save_cols,
                                   cursor, keys_current, window_current, scale_current, cols_delta,
                                   file_view, file_theme, file_sort,
                                   file_keys, file_window, file_scale, file_cols, .. } => {
            draw_save_defaults_dialog(frame, area, *view_name, theme_name, sort_name, config_path,
                                      *save_view, *save_theme, *save_sort,
                                      *save_keys, *save_window, *save_scale, *save_cols,
                                      *cursor, *keys_current, *window_current, *scale_current, cols_delta,
                                      file_view.as_deref(), file_theme.as_deref(), file_sort.as_deref(),
                                      file_keys.as_deref(), file_window.as_deref(), *file_scale, file_cols.as_deref(),
                                      args.ascii, &args.theme);
        }
        DialogMode::WindowInput { input } => {
            draw_window_input_dialog(frame, area, input, args.window, &args.theme);
        }
        DialogMode::StatColumnToggle { cursor, identity } => {
            let no_data = compute_no_data(states, &args.extra_stats, &args.hidden_base_stats);
            super::dialogs::draw_stat_column_toggle_dialog(frame, area, *cursor, identity, &args.extra_stats, &args.hidden_base_stats, 0, no_data,
                super::dialogs::stat_toggles_not_in_view("worm"),
                super::dialogs::columns_hidden_by_headers("worm", show_headers),
                args.ascii, &args.theme);
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
        // Worm doesn't follow the chart scale, so 'y' never opens this here.
        DialogMode::ScalePicker(_) => {}
    }
    // ── frozen scrolling banner (drawn last so it sits on top of any dialog) ──
    if frozen { draw_frozen_notice(frame, area, args.ascii, &args.theme, tick); }
}

// ── head letters and name tags ────────────────────────────────────────────────

/// How much of each target's identity is written beside its worm's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TagTier {
    /// No name tags (head letters only).
    None,
    /// Shortened target name.
    Name,
    /// Shortened target name plus its address.
    NameAddr,
}

/// Smallest worm area (columns, rows) that gets name tags at all.
const TAG_MIN_W: u16 = 100;
const TAG_MIN_H: u16 = 30;
/// Worm-area cells per target needed for name tags, and for name + address.
const TAG_NAME_CELLS: u32 = 800;
const TAG_ADDR_CELLS: u32 = 1600;
/// Widest a name gets in a tag before it is shortened (see `labels::condense_each`).
const TAG_NAME_MAX_W: usize = 12;

/// Picks the tag tier from the worm area size and target count, so tags only
/// appear when each worm has enough room that they don't clutter the screen.
fn worm_tag_tier(w: u16, h: u16, n: usize) -> TagTier {
    if n == 0 || w < TAG_MIN_W || h < TAG_MIN_H { return TagTier::None; }
    let per = (w as u32 * h as u32) / n as u32;
    if per >= TAG_ADDR_CELLS { TagTier::NameAddr }
    else if per >= TAG_NAME_CELLS { TagTier::Name }
    else { TagTier::None }
}

/// Tag text for one target: `name`, or `name addr` at `NameAddr` (just the
/// address when the target is named by its address).
fn tag_text(name: &str, s: &TargetState, tier: TagTier) -> String {
    if tier != TagTier::NameAddr { return name.to_string(); }
    let addr = s.current_ip.map(|ip| ip.to_string()).unwrap_or_else(|| s.host.clone());
    if super::labels::display_label(&s.label) == addr || name == addr { addr } else { format!("{name} {addr}") }
}

/// Which of the head's two columns holds the letter: the one facing the
/// direction of travel (right column when heading right, left otherwise).
fn head_letter_col(dir: u8) -> u16 {
    if (1..=3).contains(&dir) { 1 } else { 0 }
}

/// Candidate tag positions (x, y) for a `w`-wide tag beside a head at
/// (`hx`, `hy`) heading `dir`, best first, kept off the worm's own body and
/// the path just ahead of it. A sideways worm's tag sits on the row above (or
/// below); otherwise it sits on the head's row, on the side away from where
/// the worm is moving in x (right of the head when moving straight up/down).
/// Positions are clamped into the `area_w` x `area_h` area.
fn tag_candidates(hx: u16, hy: u16, dir: u8, w: u16, area_w: u16, area_h: u16) -> Vec<(u16, u16)> {
    if w == 0 || w > area_w || area_h == 0 { return Vec::new(); }
    let cw = CELL_W as u16;
    let clamp_x = |x: i32| x.clamp(0, (area_w - w) as i32) as u16;
    let (hx_i, w_i) = (hx as i32, w as i32);
    let right = clamp_x(hx_i + cw as i32 + 1);
    let left  = clamp_x(hx_i - 1 - w_i);
    let mut out = Vec::new();
    match dir {
        2 | 6 => {
            // Heading right: end the tag over the head; heading left: start it there.
            let x = if dir == 2 { clamp_x(hx_i + cw as i32 - w_i) } else { clamp_x(hx_i) };
            if hy > 0 { out.push((x, hy - 1)); }
            if hy + 1 < area_h { out.push((x, hy + 1)); }
        }
        1 | 3 => { out.push((left, hy)); out.push((right, hy)); }
        5 | 7 => { out.push((right, hy)); out.push((left, hy)); }
        _     => { out.push((right, hy)); out.push((left, hy)); }
    }
    out
}

/// Places tags, in priority order (index 0 first): each takes its first
/// candidate that stays clear of every head and of tags already placed (one
/// column of padding between tags), or `None` when no candidate is clear.
/// `heads` are the (x, y) of every head, `CELL_W` wide; `tags` are
/// (candidates, width) per tag. Returns each tag's (x, y).
fn place_worm_tags(heads: &[(u16, u16)], tags: &[(Vec<(u16, u16)>, u16)]) -> Vec<Option<(u16, u16)>> {
    // Occupied spans (row, x0, x1 half-open).
    let mut used: Vec<(u16, u16, u16)> = heads.iter().map(|&(x, y)| (y, x, x + CELL_W as u16)).collect();
    let n_heads = used.len();
    tags.iter().map(|(cands, w)| {
        let clear = |used: &[(u16, u16, u16)], x: u16, y: u16| {
            used.iter().enumerate().all(|(i, &(r, a, b))| {
                let pad = if i < n_heads { 0 } else { 1 };
                r != y || x + w + pad <= a || x >= b + pad
            })
        };
        let pick = cands.iter().copied().find(|&(x, y)| clear(&used, x, y));
        if let Some((x, y)) = pick { used.push((y, x, x + w)); }
        pick
    }).collect()
}

/// Draws name tags beside each worm's head (when the area is large enough,
/// see `worm_tag_tier`), then the head letters on top of everything. Head
/// letters only appear alongside the per-target header rows that carry them.
fn draw_worm_labels(
    buf:          &mut ratatui::buffer::Buffer,
    area:         Rect,
    states:       &[TargetState],
    nw:           &WormState,
    args:         &crate::cli::Args,
    show_letters: bool,
    now_ms:       u64,
) {
    let n = nw.worms.len().min(states.len());
    // (slot, head x, head y, dir) for each drawn worm.
    let heads: Vec<(usize, u16, u16, u8)> = (0..n)
        .filter(|&i| nw.worms[i].segs.len() >= WORM_MIN_LEN && !super::no_data::hidden_from_plot(&states[i]))
        .filter_map(|i| {
            let w = &nw.worms[i];
            let &(x, y) = w.segs.front()?;
            (y < area.height && x < area.width).then_some((i, x, y, w.dir))
        })
        .collect();

    let put = |buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, sym: &str, style: Style| {
        if x < area.width && y < area.height {
            let cell = &mut buf[(area.x + x, area.y + y)];
            cell.set_symbol(sym);
            cell.set_style(style);
        }
    };

    // ── name tags ─────────────────────────────────────────────────────────────
    let tier = worm_tag_tier(area.width, area.height, states.len());
    if tier != TagTier::None {
        let raw: Vec<&str> = states.iter().map(|s| super::labels::display_label(&s.label)).collect();
        let names = if args.no_condense_labels {
            raw.iter().map(|s| s.to_string()).collect()
        } else {
            super::labels::condense_each(&raw, TAG_NAME_MAX_W)
        };
        // Front-most worm (drawn last) gets first pick.
        let order: Vec<&(usize, u16, u16, u8)> = heads.iter().rev().collect();
        let texts: Vec<String> = order.iter().map(|&&(i, ..)| tag_text(&names[i], &states[i], tier)).collect();
        let tags: Vec<(Vec<(u16, u16)>, u16)> = order.iter().zip(&texts).map(|(&&(_, x, y, dir), t)| {
            let w = t.chars().count().min(u16::MAX as usize) as u16;
            (tag_candidates(x, y, dir, w, area.width, area.height), w)
        }).collect();
        let head_xy: Vec<(u16, u16)> = heads.iter().map(|&(_, x, y, _)| (x, y)).collect();
        let spots = place_worm_tags(&head_xy, &tags);
        for ((&&(i, ..), text), spot) in order.iter().zip(&texts).zip(spots) {
            let Some((x, y)) = spot else { continue };
            let (r, g, b) = args.theme.target_color(i);
            let dim = |c: u8| (c as u16 * 3 / 4) as u8;
            let style = Style::default().fg(Color::Rgb(dim(r), dim(g), dim(b))).bg(Color::Black);
            for (k, ch) in text.chars().enumerate() {
                put(buf, x + k as u16, y, &ch.to_string(), style);
            }
        }
    }

    // ── head letters ──────────────────────────────────────────────────────────
    if !show_letters { return; }
    for &(i, x, y, dir) in &heads {
        let worm = &nw.worms[i];
        let blink_x = ((now_ms + worm.flash_offset_ms) / 1000).is_multiple_of(4);
        let bg = if states[i].drop_flash > 0 && blink_x {
            Color::Red
        } else {
            let (r, g, b) = args.theme.target_color(i);
            Color::Rgb(r, g, b)
        };
        let style = Style::default().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD);
        let cols = (area.width - x).min(CELL_W as u16);
        let letter_col = head_letter_col(dir).min(cols - 1);
        let tag = super::slot_tag(i).to_string();
        for cw in 0..cols {
            put(buf, x + cw, y, if cw == letter_col { &tag } else { " " }, style);
        }
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn tag_tier_needs_room_per_worm() {
        assert_eq!(worm_tag_tier(99, 50, 1), TagTier::None);
        assert_eq!(worm_tag_tier(200, 29, 1), TagTier::None);
        assert_eq!(worm_tag_tier(100, 30, 1), TagTier::NameAddr); // 3000 cells
        assert_eq!(worm_tag_tier(100, 30, 2), TagTier::Name);     // 1500 each
        assert_eq!(worm_tag_tier(100, 30, 3), TagTier::Name);     // 1000 each
        assert_eq!(worm_tag_tier(100, 30, 4), TagTier::None);     // 750 each
        assert_eq!(worm_tag_tier(100, 30, 0), TagTier::None);
    }

    #[test]
    fn letter_faces_direction_of_travel() {
        for d in [1, 2, 3] { assert_eq!(head_letter_col(d), 1); }
        for d in [0, 4, 5, 6, 7] { assert_eq!(head_letter_col(d), 0); }
    }

    #[test]
    fn tag_text_by_tier() {
        let mut s = TargetState::new("gw (192.168.1.1)".to_string());
        s.host = "gw".to_string();
        s.current_ip = Some("192.168.1.1".parse().unwrap());
        assert_eq!(tag_text("gw", &s, TagTier::Name), "gw");
        assert_eq!(tag_text("gw", &s, TagTier::NameAddr), "gw 192.168.1.1");
        let mut d = TargetState::new("1.1.1.1".to_string());
        d.host = "1.1.1.1".to_string();
        assert_eq!(tag_text("1.1.1.1", &d, TagTier::NameAddr), "1.1.1.1");
    }

    #[test]
    fn sideways_worm_tag_goes_above_then_below() {
        // Heading right at (20, 5): tag ends over the head, on the row above.
        assert_eq!(tag_candidates(20, 5, 2, 4, 80, 20), vec![(18, 4), (18, 6)]);
        // Heading left: tag starts at the head.
        assert_eq!(tag_candidates(20, 5, 6, 4, 80, 20), vec![(20, 4), (20, 6)]);
        // On the top row only the row below is available.
        assert_eq!(tag_candidates(20, 0, 2, 4, 80, 20), vec![(18, 1)]);
    }

    #[test]
    fn other_worms_tag_on_head_row_away_from_x_motion() {
        // Straight down: right of the head first, one column clear.
        assert_eq!(tag_candidates(20, 5, 0, 4, 80, 20), vec![(23, 5), (15, 5)]);
        // Heading SE (moving right): left side first.
        assert_eq!(tag_candidates(20, 5, 1, 4, 80, 20), vec![(15, 5), (23, 5)]);
        // Heading NW (moving left): right side first.
        assert_eq!(tag_candidates(20, 5, 5, 4, 80, 20), vec![(23, 5), (15, 5)]);
    }

    #[test]
    fn tags_are_clamped_on_screen() {
        assert_eq!(tag_candidates(78, 5, 0, 4, 80, 20), vec![(76, 5), (73, 5)]);
        assert_eq!(tag_candidates(0, 5, 6, 4, 80, 20)[0], (0, 4));
        assert!(tag_candidates(0, 5, 0, 81, 80, 20).is_empty());
    }

    #[test]
    fn placement_avoids_heads_and_earlier_tags() {
        let heads = [(20, 5), (30, 5)];
        // First tag's preferred spot is free.
        // Second tag's preferred spot overlaps the first tag (with padding), so it falls back.
        // Third tag's only candidate covers a head, so it is skipped.
        let tags = vec![
            (vec![(23, 5)], 4),
            (vec![(26, 5), (40, 5)], 3),
            (vec![(29, 5)], 2),
        ];
        assert_eq!(place_worm_tags(&heads, &tags), vec![Some((23, 5)), Some((40, 5)), None]);
    }
}
