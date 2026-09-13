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

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use super::{Theme, logo::LogoAnim, scatter::AxisMetric};
use crate::cli::{BaseStat, ExtraStat, OutputFormat, SortMode};

// ── Interactive help menu ─────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
pub enum HelpSubMenu {
    View    { cursor: usize },
    Sort    { cursor: usize },
    Theme   { cursor: usize },
    Logging { cursor: usize },
}

#[derive(Clone, Copy, PartialEq)]
pub enum HelpItem {
    ToggleHelp,
    Explain,
    /// Section separator. `sec` is the collapsible-section index, or `None` for a plain divider.
    Separator { label: &'static str, sec: Option<u8> },
    ToggleColKeys,
    ToggleExtraStats,
    ToggleHeaders,
    FreezeToggle,
    ViewMenu,
    AxisMenu,
    SortMenu,
    ThemeMenu,
    SetWindow,
    SaveDefaults,
    ReResolve,
    LoggingMenu,
    Quit,
    CloseHelp,
}

/// Returns the section index (0/1/2) that owns the item at `idx`,
/// or `None` for items before the first collapsible separator.
pub fn item_section(items: &[HelpItem], idx: usize) -> Option<u8> {
    match items.get(idx) {
        Some(HelpItem::Separator { sec, .. }) => *sec,
        _ => {
            let mut cur = None;
            for (i, item) in items.iter().enumerate() {
                if i == idx { return cur; }
                if let HelpItem::Separator { sec, .. } = item { cur = *sec; }
            }
            None
        }
    }
}

/// Returns true if the item at `idx` should be skipped during Up/Down navigation.
pub fn nav_skip(items: &[HelpItem], idx: usize, collapsed: &[bool; 3]) -> bool {
    match items.get(idx) {
        Some(HelpItem::Separator { sec: None, .. }) => true,
        Some(HelpItem::Separator { sec: Some(s), .. }) => !collapsed[*s as usize],
        Some(_) => {
            let mut cur_sec = None;
            for (i, item) in items.iter().enumerate() {
                if i == idx { break; }
                if let HelpItem::Separator { sec, .. } = item { cur_sec = *sec; }
            }
            cur_sec.is_some_and(|s| collapsed[s as usize])
        }
        None => true,
    }
}

/// Keys reflect display order (see VIEW_DISPLAY_ORDER below), not array index.
/// "list" and "single" share the '0' jump-key and a single picker slot - only
/// one of them is ever shown/reachable at a time, chosen by target count (see
/// the compact-section handling in the picker draw functions and the `_` arm
/// of `enter_view_id!` in app.rs).
pub const HELP_VIEWS: &[(&str, &str, &str)] = &[
    ("0", "list",   "one line per target, no graph"),
    ("1", "graph",  "fullscreen area chart"),
    ("3", "worm",   "retro worm screensaver"),
    ("4", "radar",  "radar sweep"),
    ("2", "ekg",    "EKG monitor"),
    ("5", "bars",   "vertical bars: RTT height, avg ─, p95 ╌, ghost trail"),
    ("6", "cards",  "grid of per-target panels"),
    ("7", "bubble",  "floating latency bubbles — size = avg RTT"),
    ("8", "scatter", "scatter plot: avg RTT vs packet loss (a: pick axes)"),
    // pong (index 9) is intentionally excluded from VIEW_DISPLAY_ORDER and
    // VIEW_PICKER_ORDER below - it's still WIP and hidden from the in-app
    // picker/hotkeys. The entry is kept here (and the view itself fully
    // implemented in ui/pong.rs) so it's not lost; re-add its index to both
    // order arrays below, and bump the "ambient" count in VIEW_GROUPS, once
    // it's ready to ship.
    ("9", "pong",    "pong screensaver \u{2014} fast, jittery bounces mean rough latency; slow, steady ones mean a calm connection"),
    ("0", "single",  "detailed view for one target (single target only)"),
];

/// Maps a '1'..'9' hotkey position to a HELP_VIEWS index. This is the global
/// jump-key order (unrelated to how the view picker lists things - see
/// VIEW_PICKER_ORDER for that); list/single use '0' instead (handled
/// separately - see enter_view_id! in app.rs), and pong (index 9) is
/// deliberately hidden (see the comment above HELP_VIEWS), so only 8 of the
/// 11 views appear here.
/// Hotkey order: graph, ekg, worm, radar, bars, cards, bubble, scatter
pub const VIEW_DISPLAY_ORDER: &[usize] = &[1, 4, 2, 3, 5, 6, 7, 8];

/// Maps view-picker cursor index to HELP_VIEWS index. Same set of views as
/// VIEW_DISPLAY_ORDER plus a single merged list/single slot, reordered for
/// the picker's on-screen listing. The first (compact) slot holds index 10
/// (single) as a sentinel meaning "list or single, whichever fits the
/// current target count" - the draw functions and `enter_view_id!`'s `_` arm
/// both resolve it that way, so it never needs its own list (index 0) slot.
/// pong (index 9) is deliberately hidden (see the comment above HELP_VIEWS).
/// Display order: list/single, graph, ekg, radar, bars, cards, scatter, worm, bubble
pub const VIEW_PICKER_ORDER: &[usize] = &[10, 1, 4, 3, 5, 6, 8, 2, 7];

/// Section layout: (name, picker-cursor start, item count)
const VIEW_GROUPS: &[(&str, usize, usize)] = &[
    ("compact",  0, 1),
    ("timeline", 1, 2),
    ("ambient",  3, 6),
];

pub const HELP_SORTS: &[(&str, &str)] = &[
    ("none",   "specified order \u{2014} no automatic sorting"),
    ("name",   "alphabetical by label or hostname"),
    ("avg",    "lowest average RTT to top"),
    ("loss",   "fewest packet drops to top"),
    ("jitter", "smoothest jitter to top"),
    ("mtr",    "best combined latency and loss to top"),
    ("std",    "most consistent RTT (lowest stddev) to top"),
    ("p01",    "lowest best-case (p01) RTT to top"),
    ("p10",    "lowest 10th-percentile RTT to top"),
    ("p50",    "lowest median RTT to top"),
    ("p95",    "lowest 95th-percentile RTT to top"),
    ("p99",    "lowest 99th-percentile RTT to top"),
    ("cv",     "lowest coefficient of variation (std/avg) to top"),
    ("srtt",   "lowest smoothed RTT (RFC\u{00a0}6298 SRTT) to top"),
    ("streak", "no current drop streak first"),
    ("last",   "most recently responded (host up) to top"),
];

/// Per-row symbol for the sort picker, one entry per `HELP_SORTS` row (same order).
/// Reuses the exact glyph the 'c' column dialog shows for the matching stat, so a
/// sort mode and its column read as the same thing across both dialogs.
const SORT_SYMBOLS_ASCII: &[&str] = &[
    " ", "a", "~", "x", "j", "w", "s", "0", "1", "p", "5", "9", "%", "t", "#", "u",
];
const SORT_SYMBOLS_UNICODE: &[&str] = &[
    " ", "a", "\u{2248}", "\u{2717}", "\u{03b4}", "\u{03a9}", "\u{00b1}",
    "\u{2080}", "\u{2081}", "\u{00bd}", "\u{2085}", "\u{2089}", "%", "\u{03c4}", "#", "\u{2191}",
];

/// Section layout for the sort picker: (display name, start index into HELP_SORTS, item count)
pub const SORT_GROUPS: &[(&str, usize, usize)] = &[
    ("basic",  0, 2),   // none, name
    ("stats",  2, 5),   // avg, loss, jitter, mtr, std
    ("detail", 7, 9),   // p01, p10, p50, p95, p99, cv, srtt, streak, last
];

pub const HELP_THEMES: &[&str] = &[
    "colorful", "nord", "gruvbox", "dracula", "solarized",
    "okabe", "highcontrast", "phosphor", "retro", "nocolor",
];

pub fn help_menu_items(sort_available: bool, has_headers: bool, show_axis_menu: bool) -> Vec<HelpItem> {
    let _ = sort_available; // sort always present; used by caller for dimmed rendering
    let mut items = vec![
        HelpItem::ToggleHelp,
        HelpItem::Explain,
        HelpItem::Separator { label: "toggles", sec: Some(0) },
        HelpItem::ToggleColKeys,
        HelpItem::ToggleExtraStats,
    ];
    if has_headers { items.push(HelpItem::ToggleHeaders); }
    items.extend([
        HelpItem::FreezeToggle,
        HelpItem::Separator { label: "display", sec: Some(1) },
        HelpItem::ViewMenu,
    ]);
    if show_axis_menu { items.push(HelpItem::AxisMenu); }
    items.extend([
        HelpItem::SortMenu,
        HelpItem::ThemeMenu,
        HelpItem::SetWindow,
        HelpItem::Separator { label: "utilities", sec: Some(2) },
        HelpItem::SaveDefaults,
        HelpItem::ReResolve,
        HelpItem::LoggingMenu,
        HelpItem::Separator { label: "", sec: None },
        HelpItem::Quit,
        HelpItem::CloseHelp,
    ]);
    items
}

// ─────────────────────────────────────────────────────────────────────────────

/// Which axis list currently has keyboard focus in the scatter axis picker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AxisField { X, Y }

#[derive(PartialEq)]
pub enum DialogMode {
    None,
    Warning { dismiss_at: crate::time::Instant, message: String },
    Help { page: usize, scroll: u16, dismiss_at: crate::time::Instant, logo: LogoAnim, cursor: usize, sub_menu: Option<HelpSubMenu>, collapsed: [bool; 3] },
    Explain { scroll: u16 },
    FilenameInput {
        format: OutputFormat,
        input: String,
    },
    FreezeNotice { dismiss_at: crate::time::Instant, now_frozen: bool },
    SortNotice { dismiss_at: crate::time::Instant, sort_mode: SortMode },
    ThemeNotice { dismiss_at: crate::time::Instant, theme_name: &'static str },
    SortPicker  { cursor: usize },
    ThemePicker { cursor: usize },
    ViewPicker  { cursor: usize },
    /// Scatter-view axis picker ('a'). `cursor` indexes AxisMetric::ALL within
    /// whichever axis `field` currently has focus; selection is applied live
    /// to the active ScatterState as the cursor moves (see app.rs), so this
    /// variant only needs to track navigation position.
    AxisPicker  { cursor: usize, field: AxisField },
    /// Single-metric picker ('a') shared by the worm and radar views. Single-
    /// column counterpart to `AxisPicker` - `cursor` indexes `AxisMetric::ALL`;
    /// the metric it lands on is applied live to whichever screensaver is
    /// active (WormState or RadarState) as the cursor moves (see app.rs), so
    /// this variant only needs to track navigation position.
    MetricPicker { cursor: usize },
    ViewNotice  { dismiss_at: crate::time::Instant, view_name: &'static str, view_desc: &'static str },
    WindowInput { input: String },
    /// `identity` caches the effective on/off state of the 5 identity columns
    /// (mode, name, port, addr, resolve) - auto rules resolved when the dialog
    /// opens, then flipped to explicit on/off as the user toggles them.
    StatColumnToggle { cursor: usize, identity: [bool; 5] },
    SaveDefaults {
        view_name:      Option<&'static str>,
        theme_name:     &'static str,
        sort_name:      &'static str,
        config_path:    String,
        save_view:      Option<bool>,
        save_theme:     Option<bool>,
        save_sort:      Option<bool>,
        save_keys:      Option<bool>,
        save_window:    Option<bool>,
        save_cols:      Option<bool>,
        cursor:         usize,
        keys_current:   bool,
        window_current: u64,
        cols_delta:     String,
        cols_cli:       String,
        file_view:      Option<String>,
        file_theme:     Option<String>,
        file_sort:      Option<String>,
        file_keys:      Option<String>,
        file_window:    Option<String>,
        file_cols:      Option<String>,
    },
}

/// Returns the ideal height (content lines + 2 border rows) for the help dialog.
/// Used by the inline-mode viewport expansion logic before rendering.
pub fn help_dialog_ideal_height(current_view: &str, _sort_name: &str) -> u16 {
    // h, e, sep, k, x, Space, sep, v, s, t, w, sep, d, r, c/j, sep, q, Esc  =  18
    let mut n: u16 = 18;
    if matches!(current_view, "graph" | "worm" | "radar" | "ekg" | "pong" | "bubble") { n += 1; } // i
    if current_view == "scatter" { n += 1; } // a
    n + 2 // border rows
}

pub fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;
    let w = width.min(r.width);
    let h = height.min(r.height);
    Rect::new(x, y, w, h)
}

pub fn draw_frozen_notice(f: &mut Frame, area: Rect, ascii: bool, theme: &Theme, _tick: u64) {
    let icon  = if ascii { "|| " } else { "\u{23f8}  " };
    let label = " FROZEN \u{2014} Space to resume ";
    let full_text = format!("{}{}", icon, label);
    let text_w = full_text.chars().count() as u16;

    let bar_style = Style::default().fg(Color::Black).bg(theme.dlg_warning).add_modifier(Modifier::BOLD);

    if area.width < text_w || area.height == 0 { return; }

    // Position changes every 10 seconds. Use the 10-second slot as a hash seed
    // so each slot maps to a stable pseudo-random (x, y) within the available area.
    let slot = crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() / 10;
    let h  = slot.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let h2 = h.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);

    let max_x = area.width.saturating_sub(text_w) as u64;
    let max_y = area.height.saturating_sub(1) as u64;

    let bx = area.x + if max_x > 0 { (h  % (max_x + 1)) as u16 } else { 0 };
    let by = area.y + if max_y > 0 { (h2 % (max_y + 1)) as u16 } else { 0 };

    let render_rect = Rect::new(bx, by, text_w, 1);
    f.render_widget(Paragraph::new(Line::from(Span::styled(full_text, bar_style))), render_rect);
}

pub fn draw_freeze_notice_dialog(f: &mut Frame, area: Rect, ascii: bool, theme: &Theme, now_frozen: bool, secs_left: u64) {
    let (icon, verb) = if now_frozen {
        (if ascii { "|| " } else { "\u{23f8}  " }, "frozen")
    } else {
        (if ascii { "> " } else { "\u{25b6}  " }, "resumed")
    };
    let timer_str = if secs_left > 0 { format!(" {}s ", secs_left) } else { "    ".to_string() };
    let title = Line::from(vec![
        Span::styled(
            format!(" {} display {} ", icon, verb),
            Style::default().fg(Color::Black).bg(theme.dlg_warning).add_modifier(Modifier::BOLD),
        ),
        Span::styled(timer_str, Style::default().fg(theme.c(theme.dlg_timer))),
    ]);
    let body = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(
            if now_frozen {
                if ascii { "display is now frozen -- Space to resume" } else { "display is now frozen \u{2014} Space to resume" }
            } else {
                if ascii { "display resumed -- Space to freeze again" } else { "display resumed \u{2014} Space to freeze again" }
            },
            Style::default().fg(theme.c(theme.dlg_help_label)),
        ),
        Span::raw("  "),
    ])];
    let dialog_w   = 54u16.min(area.width.saturating_sub(4));
    let dialog_h   = 3u16;
    let dialog_area = centered_rect(dialog_w, dialog_h, area);
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_warning))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_window_input_dialog(f: &mut Frame, area: Rect, input: &str, current_secs: u64, theme: &Theme) {
    let cur = crate::cli::format_window_hms(current_secs);
    let default_hint = if current_secs == 0 { "300" } else { "0" };
    let title = " w \u{2014} set window ";
    let body = vec![
        Line::from(format!("  Current: {}  \u{2014}  Enter new value (e.g. 5m, 1h30m, 300).", cur)),
        Line::from(format!("  Default on empty Enter: {}  (0 = lifetime, no expiry).", default_hint)),
        Line::from("  Esc to cancel."),
        Line::from(""),
        Line::from(Span::styled(
            format!("  Window: {}_", input),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    let content_h = body.len() as u16;
    let ideal_h   = content_h + 2;
    let dialog_h  = ideal_h.min(area.height);
    let dialog_w  = 58u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(Span::styled(title, Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD))))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_sort_notice_dialog(f: &mut Frame, area: Rect, _ascii: bool, theme: &Theme, sort_mode: &SortMode, secs_left: u64) {
    let name = sort_mode.as_str();
    let (mode_name, description) = HELP_SORTS.iter()
        .find(|&&(n, _)| n == name)
        .copied()
        .unwrap_or((name, ""));
    let timer_str = if secs_left > 0 { format!(" {}s ", secs_left) } else { "    ".to_string() };
    let title = Line::from(vec![
        Span::styled(
            " s \u{2014} sort order changed ",
            Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(timer_str, Style::default().fg(theme.c(theme.dlg_timer))),
    ]);
    let body = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  sort: "),
            Span::styled(mode_name.to_owned(), Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::BOLD)),
            Span::raw("  \u{2014}  "),
            Span::styled(description.to_owned(), Style::default().fg(theme.c(theme.dlg_help_label))),
            Span::raw("  "),
        ]),
        Line::raw(""),
    ];
    let dialog_w   = 62u16.min(area.width.saturating_sub(4));
    let dialog_h   = 5u16;
    let dialog_area = centered_rect(dialog_w, dialog_h, area);
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_theme_notice_dialog(f: &mut Frame, area: Rect, _ascii: bool, theme: &Theme, theme_name: &str, secs_left: u64) {
    let timer_str = if secs_left > 0 { format!(" {}s ", secs_left) } else { "    ".to_string() };
    let title = Line::from(vec![
        Span::styled(
            " t \u{2014} theme changed ",
            Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(timer_str, Style::default().fg(theme.c(theme.dlg_timer))),
    ]);
    let body = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  theme: "),
            Span::styled(theme_name.to_owned(), Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
        ]),
        Line::raw(""),
    ];
    let dialog_w   = 62u16.min(area.width.saturating_sub(4));
    let dialog_h   = 5u16;
    let dialog_area = centered_rect(dialog_w, dialog_h, area);
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_sort_picker_dialog(f: &mut Frame, area: Rect, ascii: bool, theme: &Theme, cursor: usize, reverse: bool) {
    let key    = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let label  = Style::default().fg(theme.c(theme.dlg_help_label));
    let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let hint   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);
    // Matches the 'c' column dialog's checkbox/symbol palette exactly.
    let check_style = Style::default().fg(theme.rtt_good).add_modifier(Modifier::BOLD);
    let sym_style   = Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::DIM);

    // Reverse-sort is a real on/off toggle, so it keeps a checkbox.
    let mark_on  = if ascii { "x" } else { "\u{2713}" };
    let mark_off = " ";
    // The metric list is single-select: only the active row gets a mark, so it
    // doesn't read as a checkbox list. The selection bar itself marks the rest.
    let radio_on  = if ascii { ">" } else { "\u{25b8}" };
    let radio_off = " ";
    let symbols: &[&str] = if ascii { SORT_SYMBOLS_ASCII } else { SORT_SYMBOLS_UNICODE };

    let nav_str = if ascii {
        "  Up/Down to select   Enter/s to confirm   R to reverse   Esc to cancel"
    } else {
        "  \u{2191}\u{2193} to select   Enter/s to confirm   R to reverse   Esc to cancel"
    };

    let cursor = cursor.min(HELP_SORTS.len().saturating_sub(1));
    let name_w = HELP_SORTS.iter().map(|&(name, _)| name.len()).max().unwrap_or(0);

    let (rev_mark, rev_style) = if reverse {
        (mark_on, check_style)
    } else {
        (mark_off, hint)
    };

    let mut body: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_str.to_owned(), hint)),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  r     ["),
            Span::styled(rev_mark, rev_style),
            Span::styled("]", hint),
            Span::styled("  reverse sort  (worst to top)", label),
        ]),
        Line::raw(""),
    ];

    for &(sec_name, start, count) in SORT_GROUPS {
        let sep_str = if ascii {
            format!(" -- {} ", sec_name)
        } else {
            format!(" \u{2500}\u{2500} {} ", sec_name)
        };
        body.push(Line::from(Span::styled(sep_str, hint)));
        for (i, &(name, desc)) in HELP_SORTS.iter().enumerate().skip(start).take(count) {
            let is_sel = i == cursor;
            let sym = symbols.get(i).copied().unwrap_or(" ");
            if is_sel {
                body.push(Line::from(vec![
                    Span::styled("  ", sel_fg),
                    Span::styled(radio_on, sel_fg),
                    Span::styled(" ", sel_fg),
                    Span::styled(format!("{:<w$}", name, w = name_w), sel_fg),
                    Span::styled(" ", sel_fg),
                    Span::styled(sym, sel_fg),
                    Span::styled(format!("  {}", desc), sel_fg),
                    Span::raw("  "),
                ]).style(sel_bg));
            } else {
                body.push(Line::from(vec![
                    Span::styled("  ", hint),
                    Span::styled(radio_off, hint),
                    Span::styled(" ", hint),
                    Span::styled(format!("{:<w$}", name, w = name_w), key),
                    Span::styled(" ", hint),
                    Span::styled(sym, sym_style),
                    Span::styled(format!("  {}", desc), label),
                ]));
            }
        }
    }

    body.push(Line::raw(""));

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 66u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    let title = Line::from(Span::styled(
        " s \u{2014} select sort order ",
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_theme_picker_dialog(f: &mut Frame, area: Rect, ascii: bool, theme: &Theme, cursor: usize) {
    let key   = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let hint   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let pref_n: &str = "  ";
    let pref_s: &str = if ascii { "> " } else { "\u{25b8} " };

    let nav_str = if ascii {
        "  Up/Down to preview   Enter to confirm   Esc to cancel"
    } else {
        "  \u{2191}\u{2193} to preview   Enter to confirm   Esc to cancel"
    };

    let cursor = cursor.min(HELP_THEMES.len().saturating_sub(1));

    let mut body: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_str.to_owned(), hint)),
        Line::raw(""),
    ];

    for (i, &name) in HELP_THEMES.iter().enumerate() {
        let is_sel = i == cursor;
        if is_sel {
            body.push(Line::from(vec![
                Span::styled(format!("{}{}", pref_s, name), sel_fg),
                Span::raw("  "),
            ]).style(sel_bg));
        } else {
            body.push(Line::from(vec![
                Span::raw(pref_n),
                Span::styled(name.to_owned(), key),
            ]));
        }
    }
    body.push(Line::raw(""));

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 44u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    let title = Line::from(Span::styled(
        " t \u{2014} select theme ",
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_view_notice_dialog(f: &mut Frame, area: Rect, _ascii: bool, theme: &Theme, view_name: &str, _view_desc: &str, secs_left: u64) {
    let timer_str = if secs_left > 0 { format!(" {}s ", secs_left) } else { "    ".to_string() };
    let title = Line::from(vec![
        Span::styled(
            "\n v \u{2014} view mode changed \n",
            Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(timer_str, Style::default().fg(theme.c(theme.dlg_timer))),
    ]);
    let body = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  view mode: "),
            Span::styled(view_name.to_owned(), Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
        ]),
        Line::raw(""),
    ];
    let dialog_w    = 62u16.min(area.width.saturating_sub(4));
    let dialog_h    = 5u16;
    let dialog_area = centered_rect(dialog_w, dialog_h, area);
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_view_picker_dialog(f: &mut Frame, area: Rect, ascii: bool, theme: &Theme, cursor: usize, target_count: usize) {
    let key    = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let label  = Style::default().fg(theme.c(theme.dlg_help_label));
    let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let hint   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let pref_n: &str = "  ";
    let pref_s: &str = if ascii { "> " } else { "\u{25b8} " };

    let nav_str = if ascii {
        "  Up/Down to preview   Enter to confirm   Esc to cancel"
    } else {
        "  \u{2191}\u{2193} to preview   Enter to confirm   Esc to cancel"
    };

    let cursor = cursor.min(VIEW_PICKER_ORDER.len().saturating_sub(1));

    let mut body: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_str.to_owned(), hint)),
        Line::raw(""),
    ];

    for &(sec_name, start, count) in VIEW_GROUPS {
        let sep_str = if ascii {
            format!(" -- {} ", sec_name)
        } else {
            format!(" \u{2500}\u{2500} {} ", sec_name)
        };
        body.push(Line::from(Span::styled(sep_str, hint)));
        for (display_idx, &help_idx) in VIEW_PICKER_ORDER.iter().enumerate().skip(start).take(count) {
            // help_idx 10 is the merged list/single slot - show whichever
            // applies to the current target count (see VIEW_PICKER_ORDER).
            let &(k, name, desc) = if help_idx == 10 && target_count != 1 {
                &HELP_VIEWS[0]
            } else {
                &HELP_VIEWS[help_idx]
            };
            let is_sel = display_idx == cursor;
            if is_sel {
                body.push(Line::from(vec![
                    Span::styled(format!("{}{:<2} {:<6}", pref_s, k, name), sel_fg),
                    Span::styled(format!("  {}", desc), sel_fg),
                    Span::raw("  "),
                ]).style(sel_bg));
            } else {
                body.push(Line::from(vec![
                    Span::raw(pref_n),
                    Span::styled(format!("{:<2} {:<6}", k, name), key),
                    Span::styled(format!("  {}", desc), label),
                ]));
            }
        }
    }
    body.push(Line::raw(""));

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 62u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    let title = Line::from(Span::styled(
        " v \u{2014} select view ",
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

/// Scatter-view axis picker: two side-by-side columns of metrics (X, Y) plus
/// a log-scale toggle for the X axis. Selection applies live as the cursor
/// moves - see the `DialogMode::AxisPicker` handler in app.rs.
#[allow(clippy::too_many_arguments)]
pub fn draw_axis_picker_dialog(
    f: &mut Frame, area: Rect, ascii: bool, theme: &Theme,
    cursor: usize, field: AxisField,
    x_axis: AxisMetric, y_axis: AxisMetric, log_x: bool,
) {
    let key    = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let label  = Style::default().fg(theme.c(theme.dlg_help_label));
    let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let hint   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let pref_n: &str = "  ";
    let pref_s: &str = if ascii { "> " } else { "\u{25b8} " };

    let nav_str = if ascii {
        "  Left/Right switch axis   Up/Down select   l log-scale X   Esc/a to close"
    } else {
        "  \u{2190}\u{2192} switch axis   \u{2191}\u{2193} select   l log-scale X   Esc/a to close"
    };

    let cursor = cursor.min(AxisMetric::ALL.len().saturating_sub(1));
    let x_row  = if field == AxisField::X { cursor } else { AxisMetric::ALL.iter().position(|&m| m == x_axis).unwrap_or(0) };
    let y_row  = if field == AxisField::Y { cursor } else { AxisMetric::ALL.iter().position(|&m| m == y_axis).unwrap_or(0) };

    let col_w = 20usize;
    let mut body: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_str.to_owned(), hint)),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{:<w$}", "X AXIS", w = col_w), if field == AxisField::X { key } else { hint }),
            Span::styled("Y AXIS", if field == AxisField::Y { key } else { hint }),
        ]),
    ];

    for (i, &m) in AxisMetric::ALL.iter().enumerate() {
        let x_here = i == x_row;
        let y_here = i == y_row;
        let x_pref = if x_here { pref_s } else { pref_n };
        let y_pref = if y_here { pref_s } else { pref_n };
        let x_text = format!("{}{}", x_pref, m.label());
        let y_text = format!("{}{}", y_pref, m.label());

        let x_style = if x_here && field == AxisField::X { sel_fg } else if x_here { key } else { label };
        let y_style = if y_here && field == AxisField::Y { sel_fg } else if y_here { key } else { label };

        let x_span = if x_here && field == AxisField::X {
            Span::styled(format!("{:<w$}", x_text, w = col_w), x_style).style(sel_bg)
        } else {
            Span::styled(format!("{:<w$}", x_text, w = col_w), x_style)
        };
        let y_span = if y_here && field == AxisField::Y {
            Span::styled(y_text, y_style).style(sel_bg)
        } else {
            Span::styled(y_text, y_style)
        };

        body.push(Line::from(vec![x_span, y_span]));
    }

    body.push(Line::raw(""));
    let (log_check, log_style) = if log_x { ("[x]", key) } else { ("[ ]", hint) };
    body.push(Line::from(vec![
        Span::raw(pref_n),
        Span::styled("l     ", key),
        Span::styled(log_check, log_style),
        Span::styled("  log-scale X axis", label),
    ]));
    body.push(Line::raw(""));

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 56u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    let title = Line::from(Span::styled(
        " a \u{2014} select scatter axes ",
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

/// Single-metric picker shared by the worm and radar views: a single column
/// of metrics - whichever one the cursor lands on drives that view's
/// screensaver behavior (worm speed/length, or radar blip distance).
/// Selection applies live as the cursor moves - see the
/// `DialogMode::MetricPicker` handler in app.rs. `view_label` names the
/// active view in the dialog title (e.g. "worm", "radar").
pub fn draw_metric_picker_dialog(f: &mut Frame, area: Rect, ascii: bool, theme: &Theme, cursor: usize, view_label: &str) {
    let key    = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let hint   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let pref_n: &str = "  ";
    let pref_s: &str = if ascii { "> " } else { "\u{25b8} " };

    let nav_str = if ascii {
        "  Up/Down select   Esc/a to close"
    } else {
        "  \u{2191}\u{2193} select   Esc/a to close"
    };

    let cursor = cursor.min(AxisMetric::ALL.len().saturating_sub(1));

    let mut body: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_str.to_owned(), hint)),
        Line::raw(""),
    ];

    for (i, &m) in AxisMetric::ALL.iter().enumerate() {
        let is_sel = i == cursor;
        if is_sel {
            body.push(Line::from(vec![
                Span::styled(format!("{}{}", pref_s, m.label()), sel_fg),
                Span::raw("  "),
            ]).style(sel_bg));
        } else {
            body.push(Line::from(vec![
                Span::raw(pref_n),
                Span::styled(m.label(), key),
            ]));
        }
    }
    body.push(Line::raw(""));

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 30u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    let title = Line::from(Span::styled(
        format!(" a \u{2014} select {} metric ", view_label),
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_warning_dialog(f: &mut Frame, area: Rect, message: &str, secs_left: u64, ascii: bool, theme: &Theme) {
    let timer_str = if secs_left > 0 { format!(" {}s ", secs_left) } else { " ".to_string() };
    let warn_icon = if ascii { " ! notice " } else { " \u{26a0} notice " };
    let em_dash   = if ascii { " -- " } else { "  \u{2014} " };
    let title = Line::from(vec![
        Span::styled(warn_icon, Style::default().fg(Color::Black).bg(theme.dlg_warning).add_modifier(Modifier::BOLD)),
        Span::styled(timer_str, Style::default().fg(theme.c(theme.dlg_timer))),
    ]);

    let body = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                if ascii { message.replace('\u{2014}', "--") } else { message.to_string() },
                Style::default().fg(theme.dlg_warning)
            ),
            Span::styled(format!("{}any key to dismiss", em_dash), Style::default().fg(theme.c(theme.dlg_timer)).add_modifier(Modifier::DIM)),
        ]),
    ];

    let dialog_w  = 76u16.min(area.width.saturating_sub(4));
    let dialog_h  = 3; // 1 body line + 2 border
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_warning))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_filename_dialog(f: &mut Frame, area: Rect, format: &OutputFormat, input: &str, theme: &Theme) {
    let is_json = matches!(format, OutputFormat::Json);
    let ext = if is_json { ".json" } else { ".csv" };
    let title = " Save log ";

    let sel  = Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD);
    let dim  = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);
    let hint = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let csv_style  = if !is_json { sel  } else { dim };
    let json_style = if is_json  { sel  } else { dim };

    let body = vec![
        Line::from(format!("  Extension {} added automatically if omitted.", ext)),
        Line::from("  Esc or empty Enter to cancel."),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Format: "),
            Span::styled(" CSV ", csv_style),
            Span::raw("  "),
            Span::styled(" JSON ", json_style),
            Span::styled("   Tab to toggle", hint),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!("  Filename: {}_", input),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 54u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(Span::styled(title, Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD))))
            .alignment(Alignment::Left),
        dialog_area,
    );
}


#[allow(clippy::too_many_arguments)]
pub fn draw_help_dialog(
    f: &mut Frame,
    area: Rect,
    _page: usize,
    _scroll: u16,
    ascii: bool,
    theme: &Theme,
    secs_left: u64,
    sort_name: &str,
    frozen: bool,
    current_view: &str,
    logo: &LogoAnim,
    window_secs: u64,
    cursor: usize,
    sub_menu: Option<&HelpSubMenu>,
    is_logging: bool,
    show_col_keys: bool,
    show_headers: bool,
    extra_cols_on: bool,
    collapsed: &[bool; 3],
    target_count: usize,
) {
    let key   = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let label = Style::default().fg(theme.c(theme.dlg_help_label));
    let sel    = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let hint   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let pref_n: &str = "  ";
    let pref_s: &str = if ascii { "> " } else { "\u{25b8} " };
    let sub_m:  &str = if ascii { ">" } else { "\u{203a}" };

    let has_sort    = sort_name != "n/a";
    let has_headers = matches!(current_view, "graph" | "worm" | "radar" | "ekg" | "pong" | "bubble");

    // ── Sub-menu rendering ────────────────────────────────────────────────────
    if let Some(sub) = sub_menu {
        let nav_str = if ascii { "  Up/Down   Enter select   Esc back" }
                      else     { "  \u{2191}\u{2193}   Enter select   Esc back" };

        // View sub-menu: sectioned layout (compact / graph / compare)
        if let HelpSubMenu::View { cursor: sc } = sub {
            let sc = (*sc).min(VIEW_PICKER_ORDER.len().saturating_sub(1));
            let mut body: Vec<Line> = vec![Line::raw(""), Line::from(Span::styled(nav_str.to_owned(), hint)), Line::raw("")];
            for &(sec_name, start, count) in VIEW_GROUPS {
                body.push(dialog_section_sep(sec_name, ascii, theme));
                for (display_idx, &help_idx) in VIEW_PICKER_ORDER.iter().enumerate().skip(start).take(count) {
                    // help_idx 10 is the merged list/single slot - show whichever
                    // applies to the current target count (see VIEW_PICKER_ORDER).
                    let &(k, name, desc) = if help_idx == 10 && target_count != 1 {
                        &HELP_VIEWS[0]
                    } else {
                        &HELP_VIEWS[help_idx]
                    };
                    let is_sel = display_idx == sc;
                    let pref = if is_sel { pref_s } else { pref_n };
                    if is_sel {
                        body.push(Line::from(vec![
                            Span::styled(format!("{}{}  {:<5}", pref, k, name), sel),
                            Span::styled(format!("  {}", desc), sel),
                        ]).style(sel_bg));
                    } else {
                        body.push(Line::from(vec![
                            Span::raw(pref_n),
                            Span::styled(format!("{}  {:<5}", k, name), key),
                            Span::styled(format!("  {}", desc), label),
                        ]));
                    }
                }
            }
            body.push(Line::raw(""));
            let content_h   = body.len() as u16;
            let ideal_h     = content_h + 2;
            let dialog_h    = ideal_h.min(area.height);
            let dialog_w    = 62u16.min(area.width.saturating_sub(4));
            let dialog_area = centered_rect(dialog_w, dialog_h, area);
            let title = Line::from(Span::styled(" keys \u{203a} view ", Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD)));
            if !ascii && dialog_area.y >= area.y + 4 {
                let logo_x = area.x + area.width.saturating_sub(LogoAnim::WIDTH) / 2;
                let logo_y = dialog_area.y - LogoAnim::HEIGHT - 1;
                logo.render(f, Rect::new(logo_x, logo_y, LogoAnim::WIDTH.min(area.width), LogoAnim::HEIGHT), theme);
                let ver = concat!("v", env!("CARGO_PKG_VERSION"));
                let ver_w = ver.len() as u16;
                let ver_x = logo_x + LogoAnim::WIDTH.saturating_sub(ver_w) / 2;
                f.render_widget(Clear, Rect::new(ver_x, logo_y + LogoAnim::HEIGHT, ver_w.min(area.width), 1));
                f.render_widget(Paragraph::new(Span::styled(ver, Style::default().fg(theme.c(theme.dlg_help_label)))), Rect::new(ver_x, logo_y + LogoAnim::HEIGHT, ver_w.min(area.width), 1));
            }
            f.render_widget(Clear, dialog_area);
            f.render_widget(Paragraph::new(body).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.dlg_help_title)).title(title)).alignment(Alignment::Left), dialog_area);
            return;
        }

        // Generic sub-menu rendering (Sort, Theme, Logging)
        let (sub_title_str, sub_items, sub_cursor): (&str, Vec<(&str, &str)>, usize) = match sub {
            HelpSubMenu::Sort { cursor: sc } =>
                (" keys \u{203a} sort ", HELP_SORTS.iter().map(|&(n, d)| (n, d)).collect(), *sc),
            HelpSubMenu::Theme { cursor: sc } =>
                (" keys \u{203a} theme ", HELP_THEMES.iter().map(|&n| (n, "")).collect(), *sc),
            HelpSubMenu::Logging { cursor: sc } => {
                let items: Vec<(&str, &str)> = if is_logging {
                    vec![("CSV",  "start CSV logging"), ("JSON", "start JSON logging"), ("stop", "stop current logging")]
                } else {
                    vec![("CSV",  "start CSV logging"), ("JSON", "start JSON logging")]
                };
                (" keys \u{203a} logging ", items, *sc)
            }
            HelpSubMenu::View { .. } => unreachable!(),
        };
        let sc = sub_cursor.min(sub_items.len().saturating_sub(1));

        let mut body: Vec<Line> = vec![Line::raw(""), Line::from(Span::styled(nav_str.to_owned(), hint)), Line::raw("")];
        for (i, (name, desc)) in sub_items.iter().enumerate() {
            let is_sel = i == sc;
            let pref = if is_sel { pref_s } else { pref_n };
            if is_sel {
                let mut spans: Vec<Span<'static>> = vec![Span::styled(format!("{}{:<6}", pref, name), sel)];
                if !desc.is_empty() { spans.push(Span::styled(format!("  {}", desc), sel)); }
                body.push(Line::from(spans).style(sel_bg));
            } else {
                let mut spans: Vec<Span<'static>> = vec![Span::raw(pref_n), Span::styled(format!("{:<6}", name), key)];
                if !desc.is_empty() { spans.push(Span::styled(format!("  {}", desc), label)); }
                body.push(Line::from(spans));
            }
        }
        body.push(Line::raw(""));

        let content_h   = body.len() as u16;
        let ideal_h     = content_h + 2;
        let dialog_h    = ideal_h.min(area.height);
        let dialog_w    = 60u16.min(area.width.saturating_sub(4));
        let dialog_area = centered_rect(dialog_w, dialog_h, area);

        let title = Line::from(Span::styled(
            sub_title_str,
            Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
        ));

        if !ascii && dialog_area.y >= area.y + 4 {
            let logo_x    = area.x + area.width.saturating_sub(LogoAnim::WIDTH) / 2;
            let logo_y    = dialog_area.y - LogoAnim::HEIGHT - 1;
            let logo_area = Rect::new(logo_x, logo_y, LogoAnim::WIDTH.min(area.width), LogoAnim::HEIGHT);
            logo.render(f, logo_area, theme);
            let ver   = concat!("v", env!("CARGO_PKG_VERSION"));
            let ver_w = ver.len() as u16;
            let ver_x = logo_x + LogoAnim::WIDTH.saturating_sub(ver_w) / 2;
            let ver_area = Rect::new(ver_x, logo_y + LogoAnim::HEIGHT, ver_w.min(area.width), 1);
            f.render_widget(Clear, ver_area);
            f.render_widget(Paragraph::new(Span::styled(ver, Style::default().fg(theme.c(theme.dlg_help_label)))), ver_area);
        }

        f.render_widget(Clear, dialog_area);
        f.render_widget(
            Paragraph::new(body)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.dlg_help_title))
                    .title(title))
                .alignment(Alignment::Left),
            dialog_area,
        );
        return;
    }

    // ── Main menu rendering ───────────────────────────────────────────────────
    let win_str    = crate::cli::format_window_hms(window_secs);
    let is_scatter      = current_view == "scatter";
    let is_worm         = current_view == "worm";
    let is_radar        = current_view == "radar";
    let show_axis_menu  = is_scatter || is_worm || is_radar;
    let items           = help_menu_items(has_sort, has_headers, show_axis_menu);
    let cursor     = cursor.min(items.len().saturating_sub(1));

    // Every row below shares one fixed column layout - key(10, right-aligned)
    // | gap | checkbox(3) | gap | chevron(1) | gap | description - matching
    // the sort/column dialogs' fixed mark/symbol columns, so rows line up
    // whether or not they carry a checkbox, a chevron, both, or neither.
    let mark_on        = if ascii { "x" } else { "\u{2713}" };
    let mark_off       = " ";
    // Matches the column dialog's "on" check color exactly.
    let check_on_style = Style::default().fg(theme.rtt_good).add_modifier(Modifier::BOLD);
    // Longest value-row label ("set window") - the others pad out to it so
    // every "(value)" starts in the same column.
    const VALUE_LABEL_W: usize = 10;
    let value_desc = |lbl: &str, val: &str| -> String { format!("{:<w$} ({})", lbl, val, w = VALUE_LABEL_W) };

    // `checkbox`: Some(on) draws a colored [x]/[ ] mark; None reserves the
    // same width blank. `chevron`: draws the "opens a sub-dialog" indicator
    // in its own fixed slot. `forced_dim` keeps hint/dim styling even while
    // selected - used for the single-target sort row, which should still
    // read as unavailable under the cursor.
    let help_row = |is_sel: bool, k: &str, checkbox: Option<bool>, chevron: bool, desc: String, forced_dim: bool| -> Line<'static> {
        let pref = if is_sel { pref_s } else { pref_n };
        let box_str = match checkbox {
            Some(true)  => format!("[{}]", mark_on),
            Some(false) => format!("[{}]", mark_off),
            None        => "   ".to_string(),
        };
        let chev_str = if chevron { sub_m } else { " " };
        let ind = format!("{} {} ", box_str, chev_str);

        let (key_sty, ind_sty, desc_sty) = if forced_dim {
            (hint, hint, hint)
        } else if is_sel {
            (sel, sel, sel)
        } else {
            let ind_sty = match checkbox {
                Some(true)  => check_on_style,
                Some(false) => hint,
                None        => hint,
            };
            (key, ind_sty, label)
        };

        let mut spans = vec![
            Span::styled(format!("{}{:>10}", pref, k), key_sty),
            Span::raw("  "),
            Span::styled(ind, ind_sty),
            Span::styled(desc, desc_sty),
        ];
        if is_sel { spans.push(Span::raw("  ")); }
        Line::from(spans)
    };

    // Row index within `body` that the cursor lands on, so the dialog can
    // scroll to keep it in view - `cursor` is an index into `items`, not
    // `body` (separators fold and collapsed items disappear), so this has to
    // be tracked as rows are actually emitted.
    let mut sel_row: Option<u16> = None;

    let mut body: Vec<Line<'static>> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let is_sel = i == cursor;

        // ── Separator / section header ────────────────────────────────────────
        if let HelpItem::Separator { label, sec } = item {
            match sec {
                None => {
                    // Plain non-collapsible divider - same rule color and full
                    // width as the named section separators below, so it reads
                    // as part of the same visual family instead of a stray mark.
                    let rule_sty = Style::default().fg(theme.c(theme.col_key_rule));
                    let fill = if ascii { "-".repeat(55) } else { "\u{2500}".repeat(55) };
                    body.push(Line::from(Span::styled(format!("  {}", fill), rule_sty)));
                }
                Some(s) => {
                    let is_collapsed = collapsed[*s as usize];
                    if is_collapsed {
                        // Count the (visible) items this section is hiding, so
                        // a collapsed header hints at what's inside it.
                        let count = items.iter().skip(i + 1)
                            .take_while(|it| !matches!(it, HelpItem::Separator { .. }))
                            .count();
                        let open_icon   = if ascii { "> " } else { "\u{25b8} " };
                        let expand_hint = if ascii { format!("  [+{}]", count) } else { format!("  \u{25b9} ({})", count) };
                        if is_sel {
                            sel_row = Some(body.len() as u16);
                            body.push(Line::from(vec![
                                Span::styled(format!("{}{}", pref_s, label), sel),
                                Span::styled(expand_hint, sel),
                                Span::raw("  "),
                            ]).style(sel_bg));
                        } else {
                            body.push(Line::from(vec![
                                Span::styled(format!("  {}{}", open_icon, label), hint),
                                Span::styled(expand_hint, hint),
                            ]));
                        }
                    } else {
                        body.push(dialog_section_sep(label, ascii, theme));
                    }
                }
            }
            continue;
        }

        // Skip items that belong to a collapsed section
        if nav_skip(&items, i, collapsed) { continue; }

        if is_sel { sel_row = Some(body.len() as u16); }

        let line: Line<'static> = match item {
            HelpItem::ToggleHelp      => help_row(is_sel, "h", None, false, "toggle this help".into(), false),
            HelpItem::Explain         => help_row(is_sel, "e", None, true,  "explain output legend".into(), false),
            HelpItem::ToggleColKeys   => help_row(is_sel, "k", Some(show_col_keys), false, "column keys".into(), false),
            HelpItem::ToggleExtraStats => help_row(is_sel, "c", Some(extra_cols_on), true, "show / hide columns".into(), false),
            HelpItem::AxisMenu if is_worm  => help_row(is_sel, "a", None, true, "pick worm metric".into(), false),
            HelpItem::AxisMenu if is_radar => help_row(is_sel, "a", None, true, "pick radar metric".into(), false),
            HelpItem::AxisMenu             => help_row(is_sel, "a", None, true, "pick scatter axes / log scale".into(), false),
            HelpItem::ToggleHeaders   => help_row(is_sel, "i", Some(show_headers), false, "per-target statistics".into(), false),
            HelpItem::FreezeToggle    => help_row(is_sel, "Space", Some(frozen), false, "freeze display".into(), false),
            HelpItem::ViewMenu        => help_row(is_sel, "v", None, true, value_desc("view", current_view), false),
            HelpItem::SortMenu if !has_sort => {
                // Sort not available (single target): show dim with N/A note
                help_row(is_sel, "s", None, false, value_desc("sort", "N/A \u{2014} single target"), true)
            }
            HelpItem::SortMenu        => help_row(is_sel, "s", None, true, value_desc("sort", sort_name), false),
            HelpItem::ThemeMenu       => help_row(is_sel, "t", None, true, value_desc("theme", theme.name), false),
            HelpItem::SetWindow       => help_row(is_sel, "w", None, true, value_desc("set window", &win_str), false),
            HelpItem::SaveDefaults    => help_row(is_sel, "d", None, true, "save view / theme / sort defaults".into(), false),
            HelpItem::ReResolve       => help_row(is_sel, "r", None, false, "re-resolve DNS now".into(), false),
            HelpItem::LoggingMenu => {
                if is_logging {
                    help_row(is_sel, "l", None, false, "stop logging  (active)".into(), false)
                } else {
                    help_row(is_sel, "l", None, true, "start logging (CSV / JSON)".into(), false)
                }
            }
            HelpItem::Quit      => help_row(is_sel, "q", None, false, "quit".into(), false),
            HelpItem::CloseHelp => help_row(is_sel, "Esc", None, false, "close / return to normal view".into(), false),
            HelpItem::Separator { .. } => unreachable!(),
        };
        body.push(if is_sel { line.style(sel_bg) } else { line });
    }

    let content_h    = body.len() as u16;
    let ideal_h      = content_h + 2;
    let dialog_h     = ideal_h.min(area.height);
    let dialog_w     = 60u16.min(area.width.saturating_sub(4));
    let visible_rows = dialog_h.saturating_sub(2);
    // Scroll just far enough to keep the selected row on screen - otherwise
    // the cursor can land past the visible window (e.g. a short terminal)
    // with no highlighted row anywhere in the dialog.
    let max_scroll   = content_h.saturating_sub(visible_rows);
    let scroll       = sel_row
        .map(|r| (r + 1).saturating_sub(visible_rows))
        .unwrap_or(0)
        .min(max_scroll);

    let scroll_hint  = Style::default().fg(theme.c(theme.dlg_timer)).add_modifier(Modifier::DIM);
    let nav_hint_str = if ascii { "  Up/Down   Enter   </> collapse" } else { "  \u{2191}\u{2193}   Enter   \u{2190}\u{2192} collapse" };
    let mut title_spans: Vec<Span<'static>> = vec![
        Span::styled(" keys ", Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}s ", secs_left), Style::default().fg(theme.c(theme.dlg_timer))),
        Span::styled(nav_hint_str.to_owned(), hint),
    ];
    if content_h > visible_rows {
        title_spans.push(Span::styled(" \u{2191}\u{2193} scroll ", scroll_hint));
    }
    let title = Line::from(title_spans);

    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    if !ascii && dialog_area.y >= area.y + 4 {
        let logo_x    = area.x + area.width.saturating_sub(LogoAnim::WIDTH) / 2;
        let logo_y    = dialog_area.y - LogoAnim::HEIGHT - 1;
        let logo_area = Rect::new(logo_x, logo_y, LogoAnim::WIDTH.min(area.width), LogoAnim::HEIGHT);
        logo.render(f, logo_area, theme);
        let ver   = concat!("v", env!("CARGO_PKG_VERSION"));
        let ver_w = ver.len() as u16;
        let ver_x = logo_x + LogoAnim::WIDTH.saturating_sub(ver_w) / 2;
        let ver_area = Rect::new(ver_x, logo_y + LogoAnim::HEIGHT, ver_w.min(area.width), 1);
        f.render_widget(Clear, ver_area);
        f.render_widget(Paragraph::new(Span::styled(ver, Style::default().fg(theme.c(theme.dlg_help_label)))), ver_area);
    }

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body.clone())
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .scroll((scroll, 0))
            .alignment(Alignment::Left),
        dialog_area,
    );
    if content_h > visible_rows {
        let mut sb_state = ScrollbarState::new(content_h as usize)
            .position(scroll as usize);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(theme.c(theme.dlg_help_label))),
            dialog_area,
            &mut sb_state,
        );
    }
}

// Minimum visible content rows needed to show the explain dialog usefully.
const EXPLAIN_MIN_ROWS: u16 = 6;

/// Returns the clamped max scroll for the explain dialog given a terminal height.
pub fn explain_max_scroll(ascii: bool, terminal_height: u16) -> u16 {
    let dialog_h     = terminal_height.saturating_sub(2).max(1);
    let visible_rows = dialog_h.saturating_sub(2);
    let total        = explain_content_lines(ascii).len() as u16;
    total.saturating_sub(visible_rows)
}

fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

fn raw(text: impl Into<String>) -> Span<'static> {
    Span::raw(text.into())
}

pub fn explain_content_lines(ascii: bool) -> Vec<Line<'static>> {
    let hdr    = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let bld    = Style::default().add_modifier(Modifier::BOLD);
    let dim    = Style::default().add_modifier(Modifier::DIM);
    let grn    = Style::default().fg(Color::Green);
    let yel    = Style::default().fg(Color::Yellow);
    let red    = Style::default().fg(Color::Red);
    let mag    = Style::default().fg(Color::Magenta);
    let red_bg = Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD);

    // ── OUTPUT LEGEND ────────────────────────────────────────────────────────
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(""),
        Line::from(span("OUTPUT LEGEND", hdr)),
    ];

    // ── NUMERICAL STATS (first) ───────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(span("Numerical stats:", hdr)));
    if ascii {
        lines.push(Line::from(vec![
            raw("  "),
            span("23.3ms", grn),
            raw("  ~ 27.5ms  r 21ms<>34ms  j 2.1ms  "),
            span("x 0 0.0%", grn),
        ]));
    } else {
        lines.push(Line::from(vec![
            raw("  "),
            span("23.3ms", grn),
            raw("  \u{2248} 27.5ms  \u{21D5} 21ms\u{2194}34ms  \u{03b4} 2.1ms  "),
            span("\u{2717} 0 0.0%", grn),
        ]));
    }
    lines.push(Line::from(""));
    if ascii {
        for (sym, desc) in [
            ("~", "window average RTT"),
            ("r", "range \u{2014} best and worst RTT seen in the window"),
            ("j", "average jitter \u{2014} mean abs. difference between consecutive RTTs"),
            ("x", "drop count and loss %  (green = none  red = drops present)"),
            ("+", "duplicate count and %  (shown only when non-zero)"),
        ] {
            lines.push(Line::from(vec![raw("  "), span(sym, bld), raw("  "), raw(desc)]));
        }
    } else {
        for (sym, desc) in [
            ("\u{2248}", "window average RTT"),
            ("\u{21D5}", "range \u{2014} best \u{2194} worst RTT seen in the window"),
            ("\u{03b4}", "average jitter \u{2014} mean abs. difference between consecutive RTTs"),
            ("\u{2717}", "drop count and loss %  (green = none  red = drops present)"),
            ("\u{2295}", "duplicate count and %  (shown only when non-zero)"),
        ] {
            lines.push(Line::from(vec![raw("  "), span(sym, bld), raw("  "), raw(desc)]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![raw("  Optional extra stat columns \u{2014} enable with "), span("--columns", bld), raw(":")]));
    if ascii {
        for (sym, name, desc) in [
            ("w", "mtr   ", "mean time to reliability: avg \u{00f7} (1 \u{2212} loss)"),
            ("s", "std   ", "standard deviation of RTT"),
            ("0", "p01   ", "1st-percentile RTT"),
            ("1", "p10   ", "10th-percentile RTT"),
            ("p", "p50   ", "median RTT"),
            ("5", "p95   ", "95th-percentile RTT"),
            ("9", "p99   ", "99th-percentile RTT"),
            ("%", "cv    ", "coefficient of variation (stddev/avg %)"),
            ("t", "srtt  ", "RFC 6298 smoothed RTT"),
            ("#", "streak", "consecutive drop streak count"),
            ("u", "last  ", "time since the last successful response"),
            ("i", "status", "probe count + elapsed, plus down/last-drop once notable"),
        ] {
            lines.push(Line::from(vec![raw("    "), span(sym, bld), raw("  "), raw(name), raw("  "), raw(desc)]));
        }
    } else {
        for (sym, name, desc) in [
            ("\u{03a9}", "mtr   ", "mean time to reliability: avg \u{00f7} (1 \u{2212} loss)"),
            ("\u{00b1}", "std   ", "standard deviation of RTT"),
            ("\u{2080}", "p01   ", "1st-percentile RTT"),
            ("\u{2081}", "p10   ", "10th-percentile RTT"),
            ("\u{00bd}", "p50   ", "median RTT"),
            ("\u{2085}", "p95   ", "95th-percentile RTT"),
            ("\u{2089}", "p99   ", "99th-percentile RTT"),
            ("%",        "cv    ", "coefficient of variation (stddev/avg %)"),
            ("\u{03c4}", "srtt  ", "RFC 6298 smoothed RTT"),
            ("#",        "streak", "consecutive drop streak count"),
            ("\u{2191}", "last  ", "time since the last successful response"),
            ("\u{2139}", "status", "probe count + elapsed, plus down/last-drop once notable"),
        ] {
            lines.push(Line::from(vec![raw("    "), span(sym, bld), raw("  "), raw(name), raw("  "), raw(desc)]));
        }
    }
    lines.push(Line::from(vec![raw("  Identity columns \u{2014} automatic by default, force on with "), span("--columns", bld), raw(", off via "), span("none", bld), raw(":")]));
    for (name, desc) in [
        ("mode   ", "probe-type badge  (auto: shown when targets have mixed modes)"),
        ("name   ", "custom label or hostname  (auto: shown when one exists)"),
        ("port   ", "port suffix on the mode badge, e.g. tcp:443  (auto: non-default ports)"),
        ("addr   ", "resolved IP address  (auto: always shown)"),
        ("resolve", "DNS re-resolve counter \u{21bb}N  (auto: shown after 2+ IP changes)"),
    ] {
        lines.push(Line::from(vec![raw("    "), span(name, bld), raw("  "), raw(desc)]));
    }
    lines.push(Line::from(vec![
        raw("  "),
        span("--columns all", bld), raw(" \u{2014} all columns   "),
        span("--columns none", bld), raw(" \u{2014} no columns   "),
        span("--columns default", bld), raw(" \u{2014} default set, composable (e.g. "),
        span("--columns default,mtr", bld), raw(")"),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        raw("  Current RTT: "),
        span("green", grn),
        raw(" stable   "),
        span("red text", red),
        raw(" elevated   "),
        span(" red bg ", red_bg),
        raw(" sustained spike (3+ in a row)"),
    ]));

    // ── TREND INDICATORS ─────────────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(span("Trend indicators:", hdr)));
    lines.push(Line::from(""));
    lines.push(Line::from(raw("  MTR direction (shown left of target name):")));
    lines.push(Line::from(""));
    if ascii {
        for (sym, style, desc) in [
            ("^", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),   "improving steeply"),
            ("^", Style::default().fg(Color::Green),                                "improving gently"),
            ("-", dim,                                                               "stable"),
            ("v", Style::default().fg(Color::Yellow),                               "degrading gently"),
            ("v", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),  "degrading steeply"),
            ("x", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),     "all recent probes dropped"),
        ] {
            lines.push(Line::from(vec![raw("    "), span(sym, style), raw("  "), raw(desc)]));
        }
    } else {
        for (sym, style, desc) in [
            ("\u{2191}", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),   "improving steeply"),
            ("\u{2197}", Style::default().fg(Color::Green),                                "improving gently"),
            ("\u{2192}", dim,                                                               "stable"),
            ("\u{2198}", Style::default().fg(Color::Yellow),                               "degrading gently"),
            ("\u{2193}", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),  "degrading steeply"),
            ("\u{2715}", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),     "all recent probes dropped"),
        ] {
            lines.push(Line::from(vec![raw("    "), span(sym, style), raw("  "), raw(desc)]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(raw("  Per-response history (shown right of stats, newest on left):")));
    lines.push(Line::from(""));
    if ascii {
        for (sym, desc) in [
            ("o", "fast     (RTT < 25% of recent baseline)  \u{2014}  bright"),
            ("o", "normal   (within normal range)  \u{2014}  medium"),
            ("O", "elevated (RTT > 175% of recent baseline)  \u{2014}  dim"),
            ("X", "dropped"),
            (".", "pending / in-flight"),
        ] {
            lines.push(Line::from(vec![raw("    "), span(sym, bld), raw("  "), raw(desc)]));
        }
    } else {
        for (sym, desc) in [
            ("\u{25CB}", "fast     (RTT < 25% of recent baseline)  \u{2014}  bright"),
            ("\u{25CB}", "normal   (within normal range)  \u{2014}  medium"),
            ("O",        "elevated (RTT > 175% of recent baseline)  \u{2014}  dim"),
            ("X",        "dropped"),
            (".",        "pending / in-flight"),
        ] {
            lines.push(Line::from(vec![raw("    "), span(sym, bld), raw("  "), raw(desc)]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(raw("  Baseline = recent p95 RTT.  fast and normal both use \u{25CB} \u{2014} normal is dimmer.")));
    lines.push(Line::from(raw("  All three speed levels use shades of the theme green \u{2014} no red circles.")));

    // Range bar
    lines.push(Line::from(""));
    lines.push(Line::from(span("Range bar:", hdr)));
    if ascii {
        lines.push(Line::from(vec![
            raw("  "),
            span("range  0ms [----", dim),
            span("|", grn),
            span("------", dim),
            span("o", yel),
            span("----*..----------------]  100ms", dim),
        ]));
    } else {
        lines.push(Line::from(vec![
            raw("  "),
            span("range  0ms [\u{2500}\u{2500}\u{2500}\u{2500}", dim),
            span("\u{2577}", grn),
            span("\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}", dim),
            span("\u{25cf}", yel),
            span("\u{2500}\u{2500}\u{2500}\u{2500}\u{2022}\u{00b7}\u{00b7}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}]  100ms", dim),
        ]));
    }
    lines.push(Line::from(""));
    if ascii {
        lines.push(Line::from(vec![raw("  "), span("|", grn), raw("  window minimum \u{2014} best RTT in the rolling window")]));
        lines.push(Line::from(vec![raw("  "), span("o", yel), raw("  current RTT position  ("), span("^ rising", yel), raw("  "), span("v falling", yel), raw(")")]));
        lines.push(Line::from(vec![raw("  "), span("*..", dim), raw("  fading trail \u{2014} up to 4 previous positions")]));
    } else {
        lines.push(Line::from(vec![raw("  "), span("\u{2577}", grn), raw("  window minimum \u{2014} best RTT in the rolling window")]));
        lines.push(Line::from(vec![raw("  "), span("\u{25cf}", yel), raw("  current RTT position  ("), span("\u{2197} rising", yel), raw("  "), span("\u{2198} falling", yel), raw(")")]));
        lines.push(Line::from(vec![raw("  "), span("\u{2022}\u{00b7}\u{00b7}", dim), raw("   fading trail \u{2014} up to 4 previous positions")]));
    }
    lines.push(Line::from("  X    most recent probe was a drop"));

    // Timeline
    lines.push(Line::from(""));
    lines.push(Line::from(span("Timeline:", hdr)));
    if ascii {
        lines.push(Line::from(vec![
            raw("  "),
            span("timeline  now ", dim),
            span("____##", grn),
            span("@@@@", yel),
            span("***", red),
            span("@", mag),
            span("@@##", yel),
            span("____", grn),
            raw("  30s"),
        ]));
    } else {
        lines.push(Line::from(vec![
            raw("  "),
            span("timeline  now ", dim),
            span("\u{28c0}\u{28c0}\u{28c0}\u{28c4}\u{28c4}", grn),
            span("\u{28f6}\u{28f6}\u{28f7}\u{28f7}", yel),
            span("\u{28ff}\u{28ff}\u{28ff}", red),
            span("\u{28f7}", mag),
            span("\u{28f6}\u{28c4}", yel),
            span("\u{28c0}\u{28c0}\u{28c0}\u{28c0}", grn),
            raw("  30s"),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("  Past on the left; newest samples on the right (now)."));
    if ascii {
        lines.push(Line::from("  Each character encodes 2 samples across 4 height levels."));
    } else {
        lines.push(Line::from("  Each braille cell encodes 2 samples across 4 height levels."));
    }
    lines.push(Line::from(vec![
        raw("  "),
        span("green (low)", grn),
        raw("  "),
        span("yellow (mid)", yel),
        raw("  "),
        span("red (high)", red),
        raw("  "),
        span("magenta", mag),
        raw(" = drop or no data"),
    ]));
    if ascii {
        lines.push(Line::from("  Omit --ascii to use braille characters (requires a UTF-8 terminal)."));
    } else {
        lines.push(Line::from("  Use --ascii for block characters instead of braille."));
    }

    // View switching
    lines.push(Line::from(""));
    lines.push(Line::from(span("View switching  (v / 1-5):", hdr)));
    lines.push(Line::from("  v  cycles view: list \u{2192} graph \u{2192} worm \u{2192} radar \u{2192} ekg \u{2192} list"));
    lines.push(Line::from("  1=list  2=graph  3=worm  4=radar  5=ekg"));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![raw("  "), span("list   ", bld), raw("  one line per target, no graph - useful for many targets")]));
    lines.push(Line::from(vec![raw("  "), span("graph  ", bld), raw("  fullscreen area chart with labelled Y-axis, gridlines, and "), span("avg", dim), raw(" / "), span("p95", dim), raw(" reference lines")]));
    lines.push(Line::from(vec![raw("  "), span("worm   ", bld), raw("  retro worm screensaver - classic CGA palette")]));
    lines.push(Line::from(vec![raw("  "), span("radar  ", bld), raw("  radar sweep - targets plotted by current RTT")]));
    lines.push(Line::from(vec![raw("  "), span("ekg    ", bld), raw("  EKG monitor - scrolling latency trace")] ));

    // Rolling window
    lines.push(Line::from(""));
    lines.push(Line::from(span("Rolling window  (-w):", hdr)));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        raw("  Stats can be computed over a sliding time window (10s\u{2013}24h) or lifetime (default)."),
    ]));
    lines.push(Line::from("  Only probes sent within the window are counted; older results expire automatically."));
    lines.push(Line::from("  Set the window with -w:  vlat -w 60s   vlat -w 10m   vlat -w 1h   vlat -w 0 (lifetime)"));
    lines.push(Line::from(""));
    lines.push(Line::from("  Window-based:"));
    for (name, what) in [
        ("avg", "mean RTT over the window"),
        ("min", "best RTT seen in the window"),
        ("max", "worst RTT seen in the window"),
        ("jtr", "average jitter (mean abs. diff. between consecutive RTTs)"),
        ("mtr", "mean time to reliability \u{2014} avg \u{00f7} (1 \u{2212} loss rate)"),
        ("RTT color", "green/red trend is relative to the window average"),
    ] {
        lines.push(Line::from(vec![raw("    "), span(name, bld), raw("  "), raw(what)]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        raw("  Press "),
        span("w", bld),
        raw(" to change the window at runtime (0 = lifetime; resets the graph)."),
    ]));
    lines.push(Line::from(vec![
        raw("  Use "),
        span("--span", bld),
        raw(" to set the graph time-axis width, and "),
        span("-w", bld),
        raw(" to set the starting window (default 0 = lifetime)."),
    ]));

    // Sorting
    lines.push(Line::from(""));
    lines.push(Line::from(span("Sorting  (multi-target):", hdr)));
    lines.push(Line::from(""));
    lines.push(Line::from("  When watching multiple targets, rows can be sorted automatically in any view."));
    lines.push(Line::from(vec![
        raw("  Press "),
        span("s", bld),
        raw(" to cycle through modes, or set a default with "),
        span("--sort", bld),
        raw("."),
    ]));
    lines.push(Line::from(""));
    for (mode, desc) in [
        ("auto", "starts in mtr order \u{2014} best performers rise, worst sink (default)"),
        ("mtr ", "best performers rise to the top; worst sink \u{2014} re-evaluated periodically"),
        ("name", "alphabetical by label, hostname, or IP \u{2014} grouped by probe type"),
        ("none", "specified order matching the command-line argument list"),
    ] {
        lines.push(Line::from(vec![raw("    "), span(mode, bld), raw("  "), raw(desc)]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        raw("  In "),
        span("mtr", bld),
        raw(" mode a "),
        span("\u{25b2}", Style::default().fg(Color::Green)),
        raw(" or "),
        span("\u{25bc}", Style::default().fg(Color::Red)),
        raw(" arrow appears next to a target\u{2019}s label when it moves up or down."),
    ]));
    lines.push(Line::from(vec![
        raw("  mtr sort key: "),
        span("avg \u{00f7} (1 \u{2212} loss rate)", bld),
        raw(" \u{2014} penalises both high latency and packet loss."),
    ]));
    lines.push(Line::from(vec![
        raw("  Targets with fewer than 3 samples or still waiting for a reply are not reordered."),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(span("Output columns \u{2014} summary (shown on exit):", hdr)));
    lines.push(Line::from("  (all values cover the full session lifetime except mtr, which uses the final window)"));
    lines.push(Line::from(""));
    for (name, desc) in [
        ("avg     ", "lifetime average RTT"),
        ("stddev  ", "standard deviation of RTT \u{2014} spread around the average"),
        ("mtr     ", "mean time to reliability using the final window (same formula as live mtr)"),
        ("jitter  ", "lifetime average jitter"),
        ("min     ", "lifetime minimum RTT"),
        ("max     ", "lifetime maximum RTT"),
        ("sent    ", "total probes transmitted"),
        ("received", "probes that got a response"),
        ("loss    ", "drop count and loss % (column omitted when zero for all targets)"),
        ("changes ", "number of times the target's IP address changed (omitted when none)"),
    ] {
        lines.push(Line::from(vec![raw("  "), span(name, bld), raw("  "), raw(desc)]));
    }

    // Probe modes
    lines.push(Line::from(""));
    lines.push(Line::from(span("Probe modes:", hdr)));
    lines.push(Line::from("  (RTT is measured from probe start to first valid response for all types)"));
    lines.push(Line::from(""));
    for (name, desc) in [
        ("icmp ", "Raw ICMP echo \u{2014} most accurate; requires root or CAP_NET_RAW."),
        ("udp  ", "Sends a UDP datagram; success on ICMP Port Unreachable reply."),
        ("tcp  ", "TCP connect; measures handshake RTT; refused counts as success."),
        ("http ", "HTTP GET /; any valid HTTP response counts as success."),
        ("https", "HTTPS GET / with TLS; cert validated against system trust store by default (skip with --tls-no-verify)."),
        ("dns  ", "DNS A-record query over UDP (default name: example.net)."),
        ("tls  ", "TCP connect + TLS handshake only; measures time to secure channel."),
        ("ntp  ", "NTP request over UDP; measures RTT to a time server (default port: 123)."),
        ("ssh  ", "TCP connect + read SSH-2.0 banner; measures time to first banner byte (default port: 22)."),
        ("smtp ", "TCP connect + read SMTP 220 greeting; measures time to first banner byte (default port: 25)."),
        ("smtps", "TLS connect + read SMTP 220 greeting (default port: 465)."),
        ("exec ", "Run a shell command via sh -c; exit 0 = Hit, non-zero = Drop. RTT = wall-clock duration."),
        ("quic ", "QUIC handshake \u{2014} UDP-based, measures QUIC+TLS 1.3 setup time (default port: 443)."),
    ] {
        lines.push(Line::from(vec![raw("  "), span(name, bld), raw("  "), raw(desc)]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("  exec probes set VLAT_HOST, VLAT_IP, VLAT_PORT in the child environment."));
    lines.push(Line::from("  All probes are killed after the timeout (max 60s); exec procs receive SIGKILL."));

    // ICMP permissions
    lines.push(Line::from(""));
    lines.push(Line::from(span("ICMP permissions:", hdr)));
    lines.push(Line::from("  vlat tries ICMP first and falls back to UDP automatically."));
    lines.push(Line::from("  To enable ICMP:"));
    lines.push(Line::from("    sudo vlat ..."));
    lines.push(Line::from("    sudo setcap cap_net_raw+ep $(which vlat)"));
    lines.push(Line::from(""));

    lines
}

/// Full rendered height of the save-defaults dialog: nav hint, legend,
/// config path, header, 6 setting rows, spacer and save button, plus
/// surrounding blank lines and 2 border rows. Kept in sync with the body
/// built in `draw_save_defaults_dialog` below - callers use this (instead of
/// the generic `DIALOG_ROWS`) to reserve enough inline-viewport space before
/// opening the dialog, so it isn't clipped in non-fullscreen views.
pub const SAVE_DEFAULTS_DIALOG_H: u16 = 18;

#[allow(clippy::too_many_arguments)]
pub fn draw_save_defaults_dialog(
    f: &mut Frame,
    area: Rect,
    view_name:       Option<&str>,
    theme_name:      &str,
    sort_name:       &str,
    config_path:     &str,
    save_view:       Option<bool>,
    save_theme:      Option<bool>,
    save_sort:       Option<bool>,
    save_keys:       Option<bool>,
    save_window:     Option<bool>,
    save_cols:       Option<bool>,
    cursor:          usize,
    keys_current:    bool,
    window_current:  u64,
    cols_delta:      &str,
    file_view:       Option<&str>,
    file_theme:      Option<&str>,
    file_sort:       Option<&str>,
    file_keys:       Option<&str>,
    file_window:     Option<&str>,
    file_cols:       Option<&str>,
    ascii:           bool,
    theme:           &Theme,
) {
    let key_sty  = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let val_sty  = Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::BOLD);
    let dim_sty  = Style::default().fg(theme.c(theme.dlg_help_label));
    let hdr_sty  = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);
    let sel_fg   = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg   = Style::default().bg(theme.dlg_help_title);
    let hint_sty = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    let title = Line::from(Span::styled(
        " d \u{2014} save defaults ",
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    let nav_hint = if ascii {
        "  Up/Down navigate   Space/Enter toggle / select   Esc cancel"
    } else {
        "  \u{2191}\u{2193} navigate   Space/Enter toggle / select   Esc cancel"
    };

    let check_on    = if ascii { "[x] " } else { "[\u{2713}] " };
    let check_off   = "[ ] ";
    let check_clear = if ascii { "[-] " } else { "[\u{00d7}] " };
    let check_dis   = if ascii { "[-] " } else { "[\u{2014}] " };
    let ellipsis    = if ascii { "..." } else { "\u{2026}" };
    let ell_w       = if ascii { 3usize } else { 1 };
    let clr_sty     = Style::default().fg(theme.dlg_warning).add_modifier(Modifier::DIM);

    // Column layout (inside dialog borders):
    //   2   indent
    //   4   check "[x] "
    //   1   gap
    //   8   key name (right-padded to 8)
    //   16  current value (right-padded to 16, truncated with ellipsis)
    //       saved value (remainder)
    // Total prefix before current column = 2 + 4 + 1 + 8 = 15

    const CURR_W: usize = 16;

    let truncate = |s: &str| -> String {
        if s.chars().count() <= CURR_W {
            format!("{:<width$}", s, width = CURR_W)
        } else {
            let cut: String = s.chars().take(CURR_W.saturating_sub(ell_w)).collect();
            format!("{}{}", cut, ellipsis)
        }
    };

    let not_set = "(not set)";

    // Build a tristate toggle row.
    // state: Some(true)=save, None=skip, Some(false)=clear
    let make_row = |key: &str, state: Option<bool>, curr_val: &str, file_val: Option<&str>, is_sel: bool| -> Line {
        let (check, check_style_normal) = match state {
            Some(true)  => (check_on,    key_sty),
            None        => (check_off,   dim_sty),
            Some(false) => (check_clear, clr_sty),
        };
        let curr_col  = truncate(curr_val);
        let saved_col = file_val.unwrap_or(not_set);
        let curr_sty  = if state == Some(false) { clr_sty } else { val_sty };
        if is_sel {
            Line::from(vec![
                Span::styled(format!("  {} ", check), sel_fg),
                Span::styled(format!("{:<8}", key), sel_fg),
                Span::styled(curr_col, sel_fg),
                Span::styled(saved_col.to_owned(), sel_fg),
                Span::raw("  "),
            ]).style(sel_bg)
        } else {
            Line::from(vec![
                Span::styled(format!("  {} ", check), check_style_normal),
                Span::styled(format!("{:<8}", key), dim_sty),
                Span::styled(curr_col, curr_sty),
                Span::styled(saved_col.to_owned(), dim_sty),
            ])
        }
    };

    let keys_str   = if keys_current { "on" } else { "off" };
    let window_str = crate::cli::format_window_hms(window_current);

    let view_row = if let Some(vn) = view_name {
        make_row("view", save_view, vn, file_view, cursor == 0)
    } else {
        Line::from(vec![
            Span::styled(format!("  {} ", check_dis), dim_sty),
            Span::styled("view    ".to_owned(), dim_sty),
            Span::styled(
                format!("{:<width$}", "(pong \u{2014} no --view)", width = CURR_W),
                dim_sty,
            ),
            Span::styled(file_view.unwrap_or(not_set).to_owned(), dim_sty),
        ])
    };

    let theme_row  = make_row("theme",   save_theme,  theme_name,    file_theme,  cursor == 1);
    let sort_row   = make_row("sort",    save_sort,   sort_name,     file_sort,   cursor == 2);
    let keys_row   = make_row("keys",    save_keys,   keys_str,      file_keys,   cursor == 3);
    let window_row = make_row("window",  save_window, &window_str,   file_window, cursor == 4);
    let cols_row   = make_row("columns", save_cols,   cols_delta,    file_cols,   cursor == 5);

    let save_row = if cursor == 6 {
        Line::from(vec![
            Span::styled("  [ Save ]  ", sel_fg),
            Span::raw("  "),
        ]).style(sel_bg)
    } else {
        Line::from(Span::styled("  [ Save ]", key_sty))
    };

    // Same full-width rule color as the main menu's plain divider, separating
    // the settings rows from the save action instead of a bare blank line.
    let rule_sty = Style::default().fg(theme.c(theme.col_key_rule));
    let rule_fill = if ascii { "-".repeat(68) } else { "\u{2500}".repeat(68) };
    let action_rule = Line::from(Span::styled(format!("  {}", rule_fill), rule_sty));

    // Header row: align "current" and "saved default" with data columns
    // 15 chars prefix (indent + check + gap + key), then CURR_W for current column
    let header_row = Line::from(vec![
        Span::raw(format!("{:>15}", "")),
        Span::styled(format!("{:<width$}", "current", width = CURR_W), hdr_sty),
        Span::styled("saved default", hdr_sty),
    ]);

    // Legend explaining the three toggle states
    let legend_row = Line::from(vec![
        Span::raw("  "),
        Span::styled(check_on,    key_sty),
        Span::styled("save   ", dim_sty),
        Span::styled(check_off,   dim_sty),
        Span::styled("skip   ", dim_sty),
        Span::styled(check_clear, clr_sty),
        Span::styled("clear from file", hdr_sty),
    ]);

    let body = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_hint.to_owned(), hint_sty)),
        legend_row,
        Line::raw(""),
        Line::from(vec![Span::raw("  "), Span::styled(config_path.to_owned(), dim_sty)]),
        Line::raw(""),
        header_row,
        view_row,
        theme_row,
        sort_row,
        keys_row,
        window_row,
        cols_row,
        action_rule,
        save_row,
        Line::raw(""),
    ];

    let content_h   = body.len() as u16;
    let ideal_h     = content_h + 2;
    let dialog_h    = ideal_h.min(area.height);
    let dialog_w    = 72u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}

pub fn draw_explain_dialog(f: &mut Frame, area: Rect, scroll: u16, ascii: bool, theme: &Theme) {
    let dialog_w     = 96u16.min(area.width.saturating_sub(2));
    let dialog_h     = area.height.saturating_sub(2).max(1);
    let visible_rows = dialog_h.saturating_sub(2); // subtract borders

    let title_style  = Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD);
    let border_style = Style::default().fg(theme.dlg_help_title);

    if visible_rows < EXPLAIN_MIN_ROWS {
        // Viewport too small - show a short notice instead
        let msg = Line::from(vec![
            Span::raw("  "),
            Span::styled("terminal too small \u{2014} resize to see explain", Style::default().fg(theme.c(theme.dlg_timer))),
        ]);
        let small_w    = 52u16.min(area.width);
        let small_h    = 3u16.min(area.height);
        let small_area = centered_rect(small_w, small_h, area);
        f.render_widget(Clear, small_area);
        f.render_widget(
            Paragraph::new(vec![msg])
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(Line::from(Span::styled(" explain ", title_style)))),
            small_area,
        );
        return;
    }

    let lines      = explain_content_lines(ascii);
    let total      = lines.len() as u16;
    let max_scroll = total.saturating_sub(visible_rows);
    let scroll     = scroll.min(max_scroll);

    let nav_hint = Style::default().fg(theme.c(theme.dlg_timer)).add_modifier(Modifier::DIM);
    let title = if total > visible_rows {
        Line::from(vec![
            Span::styled(" explain ", title_style),
            Span::styled(" \u{2191}\u{2193} scroll  Esc close ", nav_hint),
        ])
    } else {
        Line::from(vec![
            Span::styled(" explain ", title_style),
            Span::styled(" Esc close ", nav_hint),
        ])
    };

    let dialog_area = centered_rect(dialog_w, dialog_h, area);
    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title))
            .scroll((scroll, 0))
            .alignment(Alignment::Left),
        dialog_area,
    );
    let mut sb_state = ScrollbarState::new(total as usize)
        .position(scroll as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(theme.c(theme.dlg_help_label))),
        dialog_area,
        &mut sb_state,
    );
}

/// Number of items in the column toggle dialog (5 identity + 4 base + 14 extra).
pub const STAT_TOGGLE_COUNT: usize = 23;

/// Full rendered height of the column toggle dialog: 3 header lines, the items,
/// 4 group separators, 1 trailing blank, plus 2 border rows.
pub const STAT_TOGGLE_DIALOG_H: u16 = 3 + STAT_TOGGLE_COUNT as u16 + 4 + 1 + 2;

/// Named section-header separator shared by dialogs that group their rows, e.g.
/// "  ─ identity ──────────────────────────────────". Rule chars use `col_key_rule`
/// (the same accent used for the column-key rule in the main display); the label
/// itself stays plain DIM rather than picking up that color, so it reads as text
/// sitting on the rule instead of part of it.
fn dialog_section_sep(label: &str, ascii: bool, theme: &Theme) -> Line<'static> {
    let rule_sty  = Style::default().fg(theme.c(theme.col_key_rule));
    let label_sty = Style::default().add_modifier(Modifier::DIM);
    let n = 52usize.saturating_sub(label.chars().count());
    let (lead, fill): (String, String) = if ascii {
        ("  - ".into(), "-".repeat(n))
    } else {
        ("  \u{2500} ".into(), "\u{2500}".repeat(n))
    };
    Line::from(vec![
        Span::styled(lead, rule_sty),
        Span::styled(label.to_owned(), label_sty),
        Span::styled(format!(" {}", fill), rule_sty),
    ])
}

#[allow(clippy::too_many_arguments)]
pub fn draw_stat_column_toggle_dialog(
    f: &mut Frame,
    area: Rect,
    cursor: usize,
    identity: &[bool; 5],
    extra_stats: &[ExtraStat],
    hidden_base: &[BaseStat],
    space_hidden: u32,
    no_data: u32,
    ascii: bool,
    theme: &Theme,
) {
    let title = Line::from(Span::styled(
        " c \u{2014} columns ",
        Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD),
    ));

    let nav_hint = if ascii {
        "  Up/Down navigate   Space toggle   Esc close"
    } else {
        "  \u{2191}\u{2193} navigate   Space toggle   Esc close"
    };

    let dim_style    = Style::default().fg(theme.c(theme.dlg_help_label));
    let sel_fg       = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg       = Style::default().bg(theme.dlg_help_title);
    let hint_style   = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);
    let narrow_style  = Style::default().fg(theme.dlg_warning).add_modifier(Modifier::DIM);
    let narrow_sym    = if ascii { "!" } else { "\u{2194}" }; // ↔ — 1 terminal col
    let no_data_style = Style::default().fg(theme.dlg_warning).add_modifier(Modifier::DIM);
    let no_data_sym   = if ascii { "?" } else { "\u{00d8}" }; // Ø — 1 terminal col

    // enabled row: green check, bright bold name, normal-weight description
    let check_on_style = Style::default().fg(theme.rtt_good).add_modifier(Modifier::BOLD);
    let name_on_style  = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let desc_on_style  = dim_style;
    // disabled row: everything muted
    let desc_off_style = hint_style;

    let mark_on  = if ascii { "x" } else { "\u{2713}" };
    let mark_off = " ";
    let sym_style = Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::DIM);

    // Per-row indicator symbol — the same glyph shown in the column-keys row.
    // Space for identity columns that have no single-char key symbol.
    let symbols: [&str; STAT_TOGGLE_COUNT] = if ascii {
        [" ", " ", ":", " ", " ",
         "~", "r", "j", "x",
         "w", "s", "0", "1", "p", "5", "9", "%", "t", "#", "u",
         "o", "|",
         "i"]
    } else {
        [" ", " ", ":", " ", "\u{21bb}",
         "\u{2248}", "\u{21d5}", "\u{03b4}", "\u{2717}",
         "\u{03a9}", "\u{00b1}", "\u{2080}", "\u{2081}", "\u{00bd}", "\u{2085}", "\u{2089}", "%", "\u{03c4}", "#", "\u{2191}",
         "\u{25cb}", "\u{258f}",
         "\u{2139}"]
    };

    // Named section separator — rule chars use col_key_rule, label uses plain DIM,
    // matching the column-key row and rule style in the main display.
    macro_rules! section_sep {
        ($label:expr) => { dialog_section_sep($label, ascii, theme) };
    }

    // (name, description, is_enabled)
    // Indices 0-4:  identity columns (enabled = effective state from `identity`)
    // Indices 5-8:  base stats (enabled = NOT in hidden_base)
    // Indices 9-22: extra stats (enabled = in extra_stats)
    let rows: [(&str, &str, bool); STAT_TOGGLE_COUNT] = [
        ("mode  ", "probe-type badge  (auto: mixed modes)",   identity[0]),
        ("name  ", "custom label or hostname",                identity[1]),
        ("port  ", "port on the mode badge, e.g. tcp:443",    identity[2]),
        ("addr  ", "resolved IP address",                     identity[3]),
        ("res   ", "DNS re-resolve counter  (\u{21bb}N)",     identity[4]),
        ("avg   ", "window average RTT",                    !hidden_base.contains(&BaseStat::Avg)),
        ("range ", "best\u{2194}worst RTT",                 !hidden_base.contains(&BaseStat::Range)),
        ("jitter", "average jitter",                        !hidden_base.contains(&BaseStat::Jitter)),
        ("drops ", "drop count and loss %",                 !hidden_base.contains(&BaseStat::Drops)),
        ("mtr   ", "mean time to reliability",              extra_stats.contains(&ExtraStat::Mtr)),
        ("std   ", "standard deviation of RTT",             extra_stats.contains(&ExtraStat::Std)),
        ("p01   ", "1st-percentile RTT",                    extra_stats.contains(&ExtraStat::P01)),
        ("p10   ", "10th-percentile RTT",                   extra_stats.contains(&ExtraStat::P10)),
        ("p50   ", "median RTT",                            extra_stats.contains(&ExtraStat::P50)),
        ("p95   ", "95th-percentile RTT",                   extra_stats.contains(&ExtraStat::P95)),
        ("p99   ", "99th-percentile RTT",                   extra_stats.contains(&ExtraStat::P99)),
        ("cv    ", "coefficient of variation",              extra_stats.contains(&ExtraStat::Cv)),
        ("srtt  ", "smoothed RTT  (RFC 6298)",              extra_stats.contains(&ExtraStat::Srtt)),
        ("streak", "consecutive drop streak",               extra_stats.contains(&ExtraStat::Streak)),
        ("last  ", "time since last successful response",   extra_stats.contains(&ExtraStat::Last)),
        ("recent", "per-probe sparkline on target row",     extra_stats.contains(&ExtraStat::Recent)),
        ("bar   ", "inline range bar on target row",        extra_stats.contains(&ExtraStat::Bar)),
        ("status", "probe count + elapsed, plus down/last-drop", extra_stats.contains(&ExtraStat::Status)),
    ];

    let mut body: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(nav_hint.to_owned(), hint_style)),
        section_sep!("identity"),
    ];

    for (i, (name, desc, enabled)) in rows.iter().enumerate() {
        if i == 5  { body.push(section_sep!("base stats")); }
        if i == 9  { body.push(section_sep!("extra stats")); }
        if i == 20 { body.push(section_sep!("visual")); }
        if i == 22 { body.push(section_sep!("text")); }

        let is_cursor  = i == cursor;
        let is_narrow  = *enabled && (space_hidden >> i) & 1 == 1;
        // Row 4 (resolve): no_data fires when auto-show condition isn't met (ip_changes <= 1),
        // same moment identity[4]=false; || i==4 lets Ø appear on the auto-hidden row.
        let is_no_data = (no_data >> i) & 1 == 1 && (*enabled || i == 4);
        let sym = symbols[i];
        let (row_prefix, pfx_sty_normal) = if is_narrow {
            (narrow_sym, narrow_style)
        } else if is_no_data {
            (no_data_sym, no_data_style)
        } else {
            (" ", dim_style)
        };

        if is_cursor {
            let pfx_fg = if is_narrow || is_no_data { sel_fg.fg(theme.dlg_warning) } else { sel_fg };
            body.push(Line::from(vec![
                Span::styled(row_prefix.to_owned(), pfx_fg),
                Span::styled(" [", sel_fg),
                Span::styled(if *enabled { mark_on } else { mark_off }, sel_fg),
                Span::styled("] ", sel_fg),
                Span::styled((*name).to_owned(), sel_fg),
                Span::styled(" ", sel_fg),
                Span::styled(sym, sel_fg),
                Span::styled("  ", sel_fg),
                Span::styled((*desc).to_owned(), sel_fg),
                Span::raw("  "),
            ]).style(sel_bg));
        } else {
            let (mark_sty, nm_sty, dsc_sty) = if *enabled {
                (check_on_style, name_on_style, desc_on_style)
            } else {
                (dim_style, dim_style, desc_off_style)
            };
            let mark = if *enabled { mark_on } else { mark_off };
            body.push(Line::from(vec![
                Span::styled(row_prefix.to_owned(), pfx_sty_normal),
                Span::styled(" [", dim_style),
                Span::styled(mark, mark_sty),
                Span::styled("] ", dim_style),
                Span::styled((*name).to_owned(), nm_sty),
                Span::styled(" ", dim_style),
                Span::styled(sym, sym_style),
                Span::styled("  ", dim_style),
                Span::styled((*desc).to_owned(), dsc_sty),
            ]));
        }
    }

    if space_hidden != 0 {
        body.push(Line::raw(""));
        body.push(Line::from(vec![
            Span::styled("  ".to_owned(), hint_style),
            Span::styled(narrow_sym.to_owned(), narrow_style),
            Span::styled(" = enabled but not shown (terminal too narrow)".to_owned(), hint_style),
        ]));
    }
    if no_data != 0 {
        body.push(Line::raw(""));
        body.push(Line::from(vec![
            Span::styled("  ".to_owned(), hint_style),
            Span::styled(no_data_sym.to_owned(), no_data_style),
            Span::styled(" = no data to display".to_owned(), hint_style),
        ]));
    }
    body.push(Line::raw(""));

    let content_h  = body.len() as u16;
    let ideal_h    = content_h + 2;
    let dialog_h   = ideal_h.min(area.height);
    let dialog_w   = 62u16.min(area.width.saturating_sub(4));
    let dialog_area = centered_rect(dialog_w, dialog_h, area);

    f.render_widget(Clear, dialog_area);
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.dlg_help_title))
                .title(title))
            .alignment(Alignment::Left),
        dialog_area,
    );
}
