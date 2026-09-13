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

use std::cell::RefCell;
use crate::time::Instant;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use crate::cli::{Args, BaseStat, ExtraStat};
use crate::constants::{BAR_ANIM_SECS, GRAPH_ANIM_SECS, TIER_FAST_PCT, TIER_HIGH_PCT};
use crate::state::{GraphColCache, MtrTrend, SparklineColCache, TargetState};
use crate::types::Sample;
use super::{ColWidths, RttColWidth, fmt_count, fmt_cv, fmt_last_up, fmt_rtt, fmt_rtt_nodec, probe_status_text, Theme};

pub fn trend_spark_span(trend: MtrTrend, ascii: bool, theme: &Theme) -> Span<'static> {
    let (ch, style) = if ascii {
        // ASCII: 3-level - both steep and gentle collapse to the same glyph.
        // Up = improving (lower MTR), down = degrading (higher MTR).
        match trend {
            MtrTrend::WarmingUp                    => (".", Style::default().add_modifier(Modifier::DIM)),
            MtrTrend::SteepUp                      => ("v", Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD)),
            MtrTrend::GentleUp                     => ("v", Style::default().fg(theme.rtt_warn)),
            MtrTrend::Flat                         => ("-", Style::default().add_modifier(Modifier::DIM)),
            MtrTrend::GentleDown                   => ("^", Style::default().fg(theme.rtt_good)),
            MtrTrend::SteepDown                    => ("^", Style::default().fg(theme.rtt_good).add_modifier(Modifier::BOLD)),
            MtrTrend::AllDrops                     => ("x", Style::default().fg(theme.drop_color).add_modifier(Modifier::BOLD)),
        }
    } else {
        // Unicode: 5-level using diagonal arrows for gentle changes.
        // Up = improving (lower MTR), down = degrading (higher MTR).
        match trend {
            MtrTrend::WarmingUp  => ("·", Style::default().add_modifier(Modifier::DIM)),
            MtrTrend::SteepUp    => ("↓", Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD)),
            MtrTrend::GentleUp   => ("↘", Style::default().fg(theme.rtt_warn)),
            MtrTrend::Flat       => ("→", Style::default().add_modifier(Modifier::DIM)),
            MtrTrend::GentleDown => ("↗", Style::default().fg(theme.rtt_good)),
            MtrTrend::SteepDown  => ("↑", Style::default().fg(theme.rtt_good).add_modifier(Modifier::BOLD)),
            MtrTrend::AllDrops   => ("✕", Style::default().fg(theme.drop_color).add_modifier(Modifier::BOLD)),
        }
    };
    Span::styled(ch, style)
}

/// Combine two samples into one taking the peak RTT - drops dominate, otherwise
/// the higher RTT wins.  Used when multiple source samples map to a single output
/// column so that brief spikes survive aggregation.
fn combine_peak(a: &Sample, b: &Sample) -> Sample {
    if a.is_drop() || b.is_drop() { return Sample::Drop; }
    let av = a.rtt().unwrap_or(0.0);
    let bv = b.rtt().unwrap_or(0.0);
    let peak = av.max(bv);
    if peak > 0.0 { Sample::Hit(peak) } else { Sample::Pending }
}

/// Peak-aggregate a chunk of source samples into a single output sample.
fn bucket_peak(chunk: &[Sample]) -> Sample {
    if chunk.is_empty() { return Sample::Pending; }
    if chunk.iter().any(|s| s.is_drop()) { return Sample::Drop; }
    let peak = chunk.iter().filter_map(|s| s.rtt()).fold(0.0_f64, f64::max);
    if peak > 0.0 { Sample::Hit(peak) } else { Sample::Pending }
}

/// Map `src` (up to `span_cols` most-recent samples) into exactly `out_cols` output columns.
///
/// **Stretch** (take ≤ out_cols): each source sample fans out across multiple columns using a
/// right-anchored mapping so the newest sample always occupies the rightmost column.
///
/// **Compression** (take > out_cols): bucket-aggregate by peak RTT - each output column
/// represents `take/out_cols` source samples and shows the peak (drops dominate) so brief
/// spikes survive the down-sampling.  Right-anchored: rightmost bucket holds the newest.
///
/// Returns the resampled vec and `data_end` - the number of columns with real data
/// (remaining columns to the right are `Sample::Pending` padding during fill-up).
fn resample_to_cols(hist: &[Sample], span_cols: usize, out_cols: usize) -> (Vec<Sample>, usize) {
    if out_cols == 0 || span_cols == 0 {
        return (vec![Sample::Pending; out_cols], 0);
    }
    let take = span_cols.min(hist.len());

    // ── Compression: bucket-aggregate take samples into out_cols cols, peak per bucket.
    if take > out_cols {
        let src   = &hist[hist.len() - take..];
        let mut v = vec![Sample::Pending; out_cols];
        for (col, slot) in v.iter_mut().enumerate() {
            let lo = (col * take) / out_cols;
            let hi = ((col + 1) * take) / out_cols;
            *slot = bucket_peak(&src[lo..hi]);
        }
        return (v, out_cols);
    }

    // ── Stretch/fill-up: left-anchored, fixed horizontal scale ──────────────
    // Oldest sample is at column 0; newest at data_end-1.  Blank columns are
    // on the RIGHT and disappear as history accumulates.  The scale is fixed:
    // each column represents span_cols/out_cols source samples throughout.
    let src      = &hist[hist.len() - take..];
    let data_end = (take * out_cols).div_ceil(span_cols).min(out_cols);
    let mut v    = vec![Sample::Pending; out_cols];
    for (col, slot) in v.iter_mut().enumerate().take(data_end) {
        let rc     = data_end - 1 - col;
        let src_hi = take - rc * take / data_end;
        let src_lo = (take - (rc + 1) * take / data_end).min(src_hi.saturating_sub(1));
        if src_lo >= src.len() { break; }
        let chunk = &src[src_lo..src_hi.min(src.len())];
        *slot = bucket_peak(chunk);
    }
    (v, data_end)
}

/// Incrementally update the column cache for one target.
///
/// Uses a fixed advance rate of `out_cols/span_cols` columns per sample
/// throughout both fill-up and steady-state, so the horizontal scale is
/// constant and columns are stable (no shimmer from variable rate).
///
/// Each new source sample is processed individually:
///  - If `frac` does not cross an integer boundary, the sample is folded into
///    the live (rightmost) bucket as a peak - drops dominate, otherwise the
///    higher RTT wins.  This means brief spikes that share a bucket with
///    faster samples still survive into the displayed graph.
///  - If `frac` crosses one or more boundaries, those columns are "frozen"
///    and the sample starts a fresh live bucket on the right.  In stretch
///    mode (advance > 1) a single sample may freeze multiple columns.
///
/// - Fill-up (data_end < out_cols): each column advance appends on the right;
///   the blank region on the right shrinks as history accumulates.
/// - Steady-state (data_end == out_cols): each column advance evicts the
///   leftmost column and appends a new one on the right (scrolls left).
///
/// Viewport/span changes invalidate the cache and trigger a full rebuild.
fn update_col_cache(
    cache:      &mut GraphColCache,
    hist:       &[Sample],
    push_count: usize,  // monotonic total pushes - stays accurate when hist.len() is capped
    span_cols:  usize,
    out_cols:   usize,
) {
    // ── Cache invalidation ──────────────────────────────────────────────────
    if cache.out_cols != out_cols || cache.span_cols != span_cols || cache.cols.len() != out_cols {
        let drain_count = cache.drain_count;
        let (cols, data_end) = resample_to_cols(hist, span_cols, out_cols);
        *cache = GraphColCache { cols, data_end, frac: 0.0,
                                 push_count, out_cols, span_cols, drain_count };
        return;
    }

    // Use push_count (not hist.len()) so that push+drain cycles at capacity
    // are detected as new samples - hist.len() stays constant when capped.
    let new_samples = push_count.saturating_sub(cache.push_count);
    cache.push_count = push_count;

    if new_samples == 0 { return; }

    // Catching up from a long pause / huge backlog: rebuild from scratch
    // rather than running a long per-sample loop.
    if new_samples > out_cols.saturating_mul(2) {
        let drain_count = cache.drain_count;
        let (cols, data_end) = resample_to_cols(hist, span_cols, out_cols);
        cache.cols        = cols;
        cache.data_end    = data_end;
        cache.frac        = 0.0;
        cache.drain_count = drain_count;
        return;
    }

    let advance      = out_cols as f64 / span_cols as f64;
    let recent_start = hist.len().saturating_sub(new_samples);

    for sample in hist[recent_start..].iter() {
        cache.frac += advance;
        let cols_for_sample = cache.frac.floor() as usize;

        if cols_for_sample == 0 {
            // Live bucket: combine into the rightmost column as peak so spikes
            // are preserved when multiple samples share a column.
            if cache.data_end == 0 {
                cache.cols[0]  = sample.clone();
                cache.data_end = 1;
            } else {
                let live = &mut cache.cols[cache.data_end - 1];
                *live = combine_peak(live, sample);
            }
            continue;
        }

        cache.frac -= cols_for_sample as f64;
        for _ in 0..cols_for_sample {
            if cache.data_end < out_cols {
                cache.cols[cache.data_end] = sample.clone();
                cache.data_end += 1;
            } else {
                cache.drain_count += 1;
                cache.cols.drain(..1);
                cache.cols.push(sample.clone());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_header_line<'a>(
    s: &TargetState,
    args: &Args,
    is_multi: bool,
    mode_label: &str,
    log_fmt: &str,
    tick_count: u64,
    label_color: Option<Color>,
    show_mode_badge: bool,
    badge_pad_w: usize,
    max_width: u16,
    sort_arrow: Option<bool>,
    show_trailing_trend: bool,
) -> Line<'a> {
    let mut spans = Vec::new();

    let label_style = if s.threshold_flash > 0 && s.threshold_flash % 2 == 1 {
        Style::default().fg(args.theme.hostname_flash_fg).bg(args.theme.hostname_flash_bg).add_modifier(Modifier::BOLD)
    } else if let Some(c) = label_color {
        Style::default().fg(c).add_modifier(Modifier::BOLD)
    } else if is_multi {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(args.theme.hostname).add_modifier(Modifier::BOLD)
    };

    match sort_arrow {
        Some(up) => {
            let sym   = if up { if args.ascii { "^" } else { "▲" } } else { if args.ascii { "v" } else { "▼" } };
            let color = if up { args.theme.rtt_good } else { args.theme.drop_color };
            spans.push(Span::styled(sym, Style::default().fg(color).add_modifier(Modifier::BOLD)));
        }
        None => spans.push(Span::raw("  ")),
    }

    // Mode badge - before the hostname, only when targets have mixed modes.
    // Padded to badge_pad_w so all hostnames start in the same column.
    if show_mode_badge {
        let mode_style = Style::default().fg(args.theme.mode_color(mode_label)).add_modifier(Modifier::DIM);
        let sep = if args.ascii { ">" } else { "\u{203a}" }; // ›
        spans.push(Span::styled(format!("{:<w$} {} ", mode_label, sep, w = badge_pad_w), mode_style));
    }

    let show_addr = args.column_vis.addr != Some(false);
    let name = if args.column_vis.name == Some(false) {
        None
    } else if s.custom_label {
        Some(s.label.clone())
    } else if s.host.parse::<std::net::IpAddr>().is_err() {
        Some(s.host.clone())
    } else {
        None
    };

    if !s.exec_cmd.is_empty() {
        let used = 1  // sort arrow
            + if show_mode_badge { badge_pad_w + 3 } else { 0 }
            + s.label.chars().count()
            + 2;  // separator
        let avail = (max_width as usize).saturating_sub(used);
        let ell = if args.ascii { "..." } else { "\u{2026}" };
        let raw_chars: Vec<char> = s.exec_cmd.chars().collect();
        let displayed = if raw_chars.len() <= avail {
            s.exec_cmd.clone()
        } else {
            let ell_len = ell.chars().count();
            let take = avail.saturating_sub(ell_len);
            format!("{}{}", raw_chars[..take].iter().collect::<String>(), ell)
        };
        if let Some(ref n) = name {
            spans.push(Span::styled(n.clone(), label_style));
            spans.push(Span::raw("  "));
        }
        if show_addr {
            spans.push(Span::styled(displayed, Style::default().add_modifier(Modifier::DIM)));
        }
    } else {
        let addr = if s.current_ip.is_none() {
            if args.ascii { "?.?.?.?".into() }
            else if args.ipv6 { "?:?:?:?:?:?:?:?".into() }
            else { "?.?.?.?".into() }
        } else {
            s.current_ip.map(|ip| ip.to_string()).unwrap_or_else(|| s.host.clone())
        };
        match name {
            Some(ref n) => {
                spans.push(Span::styled(n.clone(), label_style));
                if show_addr {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(addr, Style::default().add_modifier(Modifier::DIM)));
                }
            }
            None => {
                if show_addr {
                    spans.push(Span::styled(addr, label_style));
                }
            }
        }
    }

    // Resolving spinner (shown while DNS lookup is in flight)
    if s.resolving {
        let spinner_frames = if args.ascii {
            ["|", "/", "-", "\\"]
        } else {
            ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"] // ◐◓◑◒
        };
        let frame = spinner_frames[(tick_count as usize / 2) % 4];
        spans.push(Span::raw("  "));
        spans.push(Span::styled(frame, Style::default().fg(args.theme.resolving).add_modifier(Modifier::BOLD)));
    }

    // IP change counter - auto: only shown when > 1 (single re-resolve is unremarkable);
    // forced on/off via --columns resolve / the 'x' dialog.
    if args.column_vis.resolve.map_or(s.ip_changes > 1, |v| v) {
        let arrow = if args.ascii { "~" } else { "\u{21bb}" };
        spans.push(Span::styled(
            format!("  {}{}", arrow, s.ip_changes),
            Style::default().fg(args.theme.ip_change),
        ));
    }

    // MTR trend spark (trailing - suppressed when compact mode puts it at the front)
    if show_trailing_trend {
        let trend = s.mtr_trend(args.graph_interval);
        let spinner_showing = s.current_ip.is_none()
            || s.history.iter().rev().find(|s| !s.is_pending()).is_none();
        if !spinner_showing && trend != MtrTrend::AllDrops {
            spans.push(Span::raw("  "));
            spans.push(trend_spark_span(trend, args.ascii, &args.theme));
        }
    }

    // Log indicator (first target only)
    if !log_fmt.is_empty() {
        let tag = if args.ascii {
            format!("  [{}]", log_fmt)
        } else {
            format!("  [{} \u{25cf}]", log_fmt)
        };
        spans.push(Span::styled(tag, Style::default().fg(args.theme.log_badge).add_modifier(Modifier::BOLD)));
    }

    Line::from(spans)
}

/// Renders the 3-char UP/xX/spinner status badge as a vec of spans (always 3 display chars wide).
/// Returns empty vec if the target is in an error or resolving-wait state (caller should
/// skip the badge and let build_stats_line render the status text instead).
pub fn build_status_badge_spans(s: &TargetState, tick: u64, theme: &Theme, ascii: bool) -> Vec<Span<'static>> {
    let spin_tick = crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .map(|d| (d.as_millis() / 100) as usize)
        .unwrap_or(tick as usize);
    if s.current_ip.is_none() {
        let (frame_str, pad): (String, &'static str) = if ascii {
            let frames = &["\\", "-", "/", "|"];
            (frames[spin_tick % frames.len()].to_string(), "  ")
        } else {
            // 3-dot blob tracing the 12-position perimeter of the 4×4 braille grid CCW.
            let frames: &[&str] = &[
                "\u{280b}\u{2800}", "\u{2807}\u{2800}", "\u{2846}\u{2800}", "\u{28c4}\u{2800}",
                "\u{28c0}\u{2840}", "\u{2880}\u{28c0}", "\u{2800}\u{28e0}", "\u{2800}\u{28b0}",
                "\u{2800}\u{2838}", "\u{2800}\u{2819}", "\u{2808}\u{2809}", "\u{2809}\u{2801}",
            ];
            (frames[spin_tick % frames.len()].to_string(), " ")
        };
        return vec![
            Span::styled(frame_str, Style::default().fg(theme.resolving).add_modifier(Modifier::BOLD)),
            Span::raw(pad),
        ];
    }
    let is_drop = s.history.iter().rev().find(|s| !s.is_pending())
                   .map(|s| s.is_drop()).unwrap_or(false);
    if is_drop {
        let bright = Style::default().fg(theme.drop_color).add_modifier(Modifier::BOLD);
        let x = if ascii { "x" } else { "\u{2717}" }; // ✗
        vec![Span::styled(x, bright), Span::styled(x, bright), Span::raw(" ")]
    } else {
        let recent: Vec<&Sample> = s.history.iter().rev()
            .filter(|s| !s.is_pending())
            .take(5)
            .collect();
        if recent.is_empty() {
            // Waiting for first probe result - clockwise spinner, warm color
            let (frame_str, pad): (String, &'static str) = if ascii {
                let frames = &["|", "/", "-", "\\"];
                (frames[spin_tick % frames.len()].to_string(), "  ")
            } else {
                // 3-dot blob tracing the 12-position perimeter of the 4×4 braille grid CW.
                let frames: &[&str] = &[
                    "\u{2809}\u{2801}", "\u{2808}\u{2809}", "\u{2800}\u{2819}", "\u{2800}\u{2838}",
                    "\u{2800}\u{28b0}", "\u{2800}\u{28e0}", "\u{2880}\u{28c0}", "\u{28c0}\u{2840}",
                    "\u{28c4}\u{2800}", "\u{2846}\u{2800}", "\u{2807}\u{2800}", "\u{280b}\u{2800}",
                ];
                (frames[spin_tick % frames.len()].to_string(), " ")
            };
            return vec![
                Span::styled(frame_str, Style::default().fg(Color::Gray)),
                Span::raw(pad),
            ];
        }
        let recent_drop_count = recent.iter().filter(|s| s.is_drop()).count();
        let no_drops = recent_drop_count == 0;
        let (badge, badge_style): (String, Style) = if no_drops {
            let now = Instant::now();
            let secs_since_drop = s.win_drop_times.iter()
                .map(|t| now.duration_since(*t).as_secs())
                .min();
            let up_color = match secs_since_drop {
                Some(e) if e < 5  => theme.rtt_warn,    // just recovered: warn color (orange/red per theme)
                Some(e) if e < 30 => theme.ip_change,   // recently had drops: yellow per theme
                _                 => theme.rtt_good,    // all clear: green
            };
            ("UP ".into(), Style::default().fg(up_color).add_modifier(Modifier::BOLD))
        } else {
            (format!("X{} ", recent_drop_count), Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD))
        };
        vec![Span::styled(badge, badge_style)]
    }
}

pub fn build_current_rtt_spans(s: &TargetState, cw: &ColWidths, theme: &Theme) -> Vec<Span<'static>> {
    if s.resolve_error.is_some() || (s.waiting && s.resolving) {
        return vec![Span::raw(" ".repeat(cw.rtt.active_w()))];
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let is_drop = s.history.iter().rev().find(|ss| !ss.is_pending())
                   .map(|ss| ss.is_drop()).unwrap_or(false);
    if is_drop {
        let drop_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
        let col = &cw.rtt;
        return if col.frac_w > 0 {
            let n_int  = 2.min(col.int_w);
            let n_frac = 3usize.saturating_sub(n_int).min(col.frac_w);
            vec![
                Span::styled(format!("{:>w$}", "\u{2014}".repeat(n_int),  w = col.int_w),  drop_style),
                Span::styled(format!("{:<w$}", "\u{2014}".repeat(n_frac), w = col.frac_w), drop_style),
            ]
        } else {
            let n = 3.min(col.compact);
            vec![Span::styled(format!("{:>w$}", "\u{2014}".repeat(n), w = col.compact), drop_style)]
        };
    }
    if s.waiting {
        let d   = "\u{2014}"; // -
        let col = &cw.rtt;
        return if col.frac_w > 0 {
            vec![
                Span::styled(format!("{:>w$}", d, w = col.int_w), dim),
                Span::styled(" ".repeat(col.frac_w), dim),
            ]
        } else {
            vec![Span::styled(format!("{:>w$}", d, w = col.compact), dim)]
        };
    }
    let lat_style  = latency_style(s.current_pct, theme);
    let frac_style = lat_style.add_modifier(Modifier::DIM);
    let sv  = fmt_rtt(s.last_rtt);
    let col = &cw.rtt;
    if col.frac_w > 0 {
        if let Some(dot) = sv.find('.') {
            vec![
                Span::styled(format!("{:>w$}", &sv[..dot], w = col.int_w), lat_style),
                Span::styled(format!("{:<w$}", &sv[dot..], w = col.frac_w), frac_style),
            ]
        } else {
            vec![
                Span::styled(format!("{:>w$}", &sv, w = col.int_w), lat_style),
                Span::styled(" ".repeat(col.frac_w), frac_style),
            ]
        }
    } else {
        let full = format!("{:>w$}", &sv, w = col.compact);
        if let Some(dot) = full.find('.') {
            vec![
                Span::styled(full[..dot].to_string(), lat_style),
                Span::styled(full[dot..].to_string(), frac_style),
            ]
        } else {
            vec![Span::styled(full, lat_style)]
        }
    }
}

/// Column-layout descriptor for `build_stats_keys_line`.
///
/// Each field maps to a visible or unlabeled column in the stats row:
/// ```text
/// [lead] [probe] [name] [addr] [trailer] [status] rtt [pings_gap] [pings] avg mtr …
/// ```
/// Zero-width fields are omitted.
pub struct KeysPrefix {
    /// Unlabeled leading chars (border, sort-arrow placeholder, etc.).
    pub lead:      usize,
    /// Probe-type badge column total width (0 = absent). Label: "mode".
    pub probe:     usize,
    /// Name column total width including separator (0 = absent). Label: "name".
    pub name:      usize,
    /// Address/host column width. Label: "addr".
    pub addr:      usize,
    /// Unlabeled gap before the resolve-counter slot (resolving-spinner spacing).
    pub trailer:   usize,
    /// Resolve-counter (ip-changes) slot width (0 = absent). Label: "res".
    pub resolve:   usize,
    /// UP/DN status badge width (0 = absent). Label: "up".
    pub status:    usize,
    /// Gap between the rtt value and the pings area (0 if no circles).
    pub pings_gap: usize,
    /// Pings / trend-circles column width (0 = absent). Label: "pings".
    pub pings:     usize,
    /// Width of the inline range-bar slot between rtt and the stats columns (0 = absent).
    /// Compact/spark view inserts a stripped 28-char bar body plus 1 leading space here.
    pub inline_range_w:     usize,
    /// Scale label shown in the inline range-bar header slot, e.g. " 0 ────── 200".
    pub inline_range_label: String,
    /// When non-empty, rendered instead of `inline_range_label` (used for animated scale transitions).
    pub inline_range_spans: Vec<Span<'static>>,
}

/// Builds a dim column-name header line whose labels align with the live stats row.
#[allow(clippy::too_many_arguments)]
pub fn build_stats_keys_line(
    pfx:      &KeysPrefix,
    cw:       &ColWidths,
    gap:      usize,
    show_rtt: bool,
    show_drp: bool,
    show_dup: bool,
    _ascii:   bool,
    theme:    &Theme,
) -> Line<'static> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let _ = theme;
    let mut spans: Vec<Span<'static>> = Vec::new();

    if pfx.lead  > 0 { spans.push(Span::raw(" ".repeat(pfx.lead))); }
    if pfx.probe > 0 { spans.push(Span::styled(format!("{:<w$}", "mode",  w = pfx.probe), dim)); }
    if pfx.name  > 0 { spans.push(Span::styled(format!("{:<w$}", "name",  w = pfx.name),  dim)); }
    if pfx.addr  > 0 { spans.push(Span::styled(format!("{:<w$}", "addr",  w = pfx.addr),  dim)); }
    if pfx.trailer > 0 { spans.push(Span::raw(" ".repeat(pfx.trailer))); }
    if pfx.resolve > 0 { spans.push(Span::styled(format!("{:<w$}", "res", w = pfx.resolve), dim)); }
    if pfx.status  > 0 { spans.push(Span::styled(format!("{:<w$}", "up", w = pfx.status), dim)); }

    let rtt_w = cw.rtt.active_w();
    let jit_w = cw.jitter.active_w();
    let rng_w = cw.range_compact;

    if show_rtt {
        spans.push(Span::styled(format!("{:<w$}", "rtt", w = rtt_w), dim));
    }
    if pfx.pings_gap > 0 { spans.push(Span::raw(" ".repeat(pfx.pings_gap))); }
    if pfx.pings     > 0 { spans.push(Span::styled(format!("{:<w$}", "recent", w = pfx.pings), dim)); }
    if pfx.inline_range_w > 0 {
        if !pfx.inline_range_spans.is_empty() {
            spans.extend(pfx.inline_range_spans.iter().cloned());
        } else {
            spans.push(Span::styled(
                format!("{:<w$}", pfx.inline_range_label.as_str(), w = pfx.inline_range_w),
                dim,
            ));
        }
    }

    let hide = |b: &BaseStat| cw.hidden_base_stats.contains(b);
    if !hide(&BaseStat::Avg) {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(format!("{:<w$}", "avg",    w = 1 + rtt_w), dim));
    }
    if !hide(&BaseStat::Range) {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(format!("{:<w$}", "range",  w = 1 + rng_w + 1 + rng_w), dim));
    }
    if !hide(&BaseStat::Jitter) {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(format!("{:<w$}", "jitter", w = 1 + jit_w), dim));
    }

    if show_drp && !hide(&BaseStat::Drops) {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(format!("{:<w$}", "loss", w = 1 + cw.drp), dim));
    }
    if show_dup {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(format!("{:<w$}", "dup",  w = cw.dup + 6), dim));
    }

    for stat in &cw.stat_order {
        match stat {
            ExtraStat::Mtr    => if let Some(ref col) = cw.mtr    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "mtr",    w = 1 + col.active_w()), dim)); }
            ExtraStat::Std    => if let Some(ref col) = cw.std    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "std",    w = 1 + col.active_w()), dim)); }
            ExtraStat::P01    => if let Some(ref col) = cw.p01    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "p01",    w = 1 + col.active_w()), dim)); }
            ExtraStat::P10    => if let Some(ref col) = cw.p10    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "p10",    w = 1 + col.active_w()), dim)); }
            ExtraStat::P50    => if let Some(ref col) = cw.p50    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "p50",    w = 1 + col.active_w()), dim)); }
            ExtraStat::P95    => if let Some(ref col) = cw.p95    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "p95",    w = 1 + col.active_w()), dim)); }
            ExtraStat::P99    => if let Some(ref col) = cw.p99    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "p99",    w = 1 + col.active_w()), dim)); }
            ExtraStat::Cv     => if let Some(cv_w)   = cw.cv      { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "cv",     w = 1 + cv_w),           dim)); }
            ExtraStat::Srtt   => if let Some(ref col) = cw.srtt   { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "srtt",   w = 1 + col.active_w()), dim)); }
            ExtraStat::Streak => if let Some(stk_w)  = cw.streak  { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "streak", w = 1 + stk_w),          dim)); }
            ExtraStat::Last   => if let Some(last_w) = cw.last    { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "last",   w = 1 + last_w),         dim)); }
            ExtraStat::Status => if let Some(status_w) = cw.status { spans.push(Span::raw(" ".repeat(gap))); spans.push(Span::styled(format!("{:<w$}", "status", w = 1 + status_w),      dim)); }
            _ => {}
        }
    }

    Line::from(spans)
}

/// Render the column-key names line and its separator rule into their respective areas.
pub fn render_col_key_rule(frame: &mut Frame, hdr: Line<'_>, hdr_area: Rect, rule_area: Rect, ascii: bool, theme: &Theme) {
    let rule_w: usize = hdr.spans.iter().map(|s| s.content.chars().count()).sum();
    frame.render_widget(Paragraph::new(hdr), hdr_area);
    let rule_ch = if ascii { "-" } else { "\u{2500}" };
    let rule = Line::from(Span::styled(rule_ch.repeat(rule_w), Style::default().fg(theme.c(theme.col_key_rule))));
    frame.render_widget(Paragraph::new(rule), rule_area);
}

#[allow(clippy::too_many_arguments)]
pub fn build_stats_line<'a>(
    s: &TargetState,
    ascii: bool,
    stats_window: bool,
    shared_scale: f64,
    cw: &ColWidths,
    show_range: bool,
    show_status_badge: bool,
    show_current_rtt: bool,
    theme: &Theme,
    show_drp: bool,
    show_dup: bool,
    tick: u64,
    gap: usize,
    verbose_labels: bool,
    interval_ms: u64,
) -> Line<'a> {
    let is_drop    = s.history.iter().rev().find(|s| !s.is_pending())
                       .map(|s| s.is_drop()).unwrap_or(false);
    let dim        = Style::default().add_modifier(Modifier::DIM);
    let mid        = Style::default().fg(Color::Gray);
    let bright     = Style::default().fg(Color::White);
    // Symbol/word for a stat label: verbose_labels swaps the single-char glyph for the
    // full word (with a trailing space, since word labels don't glue to the value the
    // way glyphs do).
    let sym = |ascii_c: &'static str, uni_c: &'static str, word: &'static str| -> String {
        if verbose_labels { format!("{} ", word) } else if ascii { ascii_c.to_string() } else { uni_c.to_string() }
    };

    // Render a decimal-aligned or compact RTT value into `spans`.
    // When col.frac_w > 0: int part right-aligned in col.int_w, frac part left-aligned in col.frac_w.
    // When col.frac_w == 0: whole string right-aligned in col.compact (original behaviour).
    let push_rtt_val = |spans: &mut Vec<Span<'static>>, val: f64, col: &RttColWidth, int_style: Style, frac_style: Style| {
        let sv = fmt_rtt(val);
        if col.frac_w > 0 {
            if let Some(dot) = sv.find('.') {
                spans.push(Span::styled(format!("{:>w$}", &sv[..dot], w = col.int_w), int_style));
                spans.push(Span::styled(format!("{:<w$}", &sv[dot..], w = col.frac_w), frac_style));
            } else {
                spans.push(Span::styled(format!("{:>w$}", &sv, w = col.int_w), int_style));
                spans.push(Span::styled(format!("{:w$}", "", w = col.frac_w), frac_style));
            }
        } else {
            let full = format!("{:>w$}", &sv, w = col.compact);
            if let Some(dot) = full.find('.') {
                spans.push(Span::styled(full[..dot].to_string(), int_style));
                spans.push(Span::styled(full[dot..].to_string(), frac_style));
            } else {
                spans.push(Span::styled(full, int_style));
            }
        }
    };

    // Render ∞ or - for a slot that has no numeric value, filling the same column width.
    let push_mtr_placeholder = |spans: &mut Vec<Span<'static>>, sym: &'static str, sym_style: Style, col: &RttColWidth| {
        if col.frac_w > 0 {
            spans.push(Span::styled(format!("{:>w$}", sym, w = col.int_w), sym_style));
            spans.push(Span::styled(format!("{:w$}", "", w = col.frac_w), dim));
        } else {
            spans.push(Span::styled(format!("{:>w$}", sym, w = col.compact), sym_style));
        }
    };

    let mut spans = Vec::new();
    if show_status_badge {
        spans.extend(build_status_badge_spans(s, tick, theme, ascii));
    }
    if is_drop {
        if show_current_rtt {
            let drop_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
            let col = &cw.rtt;
            if col.frac_w > 0 {
                let n_int  = 2.min(col.int_w);
                let n_frac = 3usize.saturating_sub(n_int).min(col.frac_w);
                spans.push(Span::styled(format!("{:>w$}", "\u{2014}".repeat(n_int),  w = col.int_w),  drop_style));
                spans.push(Span::styled(format!("{:<w$}", "\u{2014}".repeat(n_frac), w = col.frac_w), drop_style));
            } else {
                let n = 3.min(col.compact);
                spans.push(Span::styled(format!("{:>w$}", "\u{2014}".repeat(n), w = col.compact), drop_style));
            }
        }
    } else if s.waiting {
        let d = "\u{2014}"; // -
        let hide_b = |b: &BaseStat| cw.hidden_base_stats.contains(b);
        if show_current_rtt {
            push_mtr_placeholder(&mut spans, d, dim, &cw.rtt);
        }
        let sp = " ".repeat(gap);
        if !hide_b(&BaseStat::Avg) {
            spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("~", "\u{2248}", "avg"), dim));   push_mtr_placeholder(&mut spans, d, dim, &cw.rtt);
        }
        if !hide_b(&BaseStat::Range) {
            spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("r", "\u{21D5}", "range"), dim));
            spans.push(Span::styled(format!("{:>w$}", d, w = cw.range_compact), dim));
            spans.push(Span::styled(if ascii { "-" } else { "\u{2194}" }, dim));
            spans.push(Span::styled(format!("{:>w$}", d, w = cw.range_compact), dim));
        }
        if !hide_b(&BaseStat::Jitter) {
            spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("j", "\u{03b4}", "jitter"), dim));   push_mtr_placeholder(&mut spans, d, dim, &cw.jitter);
        }
        // Extra stat placeholders for waiting state - rendered in stat_order
        for stat in &cw.stat_order {
            match stat {
                ExtraStat::Mtr    => if let Some(ref col) = cw.mtr    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("w", "\u{03a9}", "mtr"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::Std    => if let Some(ref col) = cw.std    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("s", "\u{00b1}", "std"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::P01    => if let Some(ref col) = cw.p01    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("0", "\u{2080}", "p01"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::P10    => if let Some(ref col) = cw.p10    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("1", "\u{2081}", "p10"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::P50    => if let Some(ref col) = cw.p50    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("p", "\u{00bd}", "p50"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::P95    => if let Some(ref col) = cw.p95    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("5", "\u{2085}", "p95"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::P99    => if let Some(ref col) = cw.p99    { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("9", "\u{2089}", "p99"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::Cv     => if let Some(cv_w) = cw.cv        { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("%", "%", "cv"), dim)); spans.push(Span::styled(format!("{:>w$}", d, w = cv_w), dim)); }
                ExtraStat::Srtt   => if let Some(ref col) = cw.srtt   { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("t", "\u{03c4}", "srtt"), dim)); push_mtr_placeholder(&mut spans, d, dim, col); }
                ExtraStat::Streak => if let Some(stk_w) = cw.streak   { spans.push(Span::raw(sp.clone())); spans.push(Span::styled(sym("#", "#", "streak"), dim)); spans.push(Span::styled(format!("{:>w$}", d, w = stk_w), dim)); }
                ExtraStat::Last   => if let Some(last_w) = cw.last    { spans.push(Span::raw(sp.clone())); if verbose_labels { spans.push(Span::styled("last ", dim)); } spans.push(Span::styled(format!("{:>w$}", d, w = last_w), dim)); }
                ExtraStat::Status => if let Some(status_w) = cw.status { spans.push(Span::raw(sp.clone())); if verbose_labels { spans.push(Span::styled("status ", dim)); } spans.push(Span::styled(format!("{:<w$}", d, w = status_w), dim)); }
                _ => {}
            }
        }
        return Line::from(spans);
    } else if show_current_rtt {
        let lat_style = latency_style(s.current_pct, theme);
        push_rtt_val(&mut spans, s.last_rtt, &cw.rtt, lat_style, lat_style.add_modifier(Modifier::DIM));
    }

    let (avg_val, min_val, max_val, jit_val) = if stats_window {
        (s.win_avg(), s.win_min(), s.win_max(), s.win_jitter_avg())
    } else {
        (s.avg_latency(), s.life_min(), s.life_max(), s.avg_jitter())
    };

    let hide_b = |b: &BaseStat| cw.hidden_base_stats.contains(b);

    if !hide_b(&BaseStat::Avg) {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(sym("~", "\u{2248}", "avg"), dim));
        push_rtt_val(&mut spans, avg_val, &cw.rtt, bright, mid);
    }
    if !hide_b(&BaseStat::Range) {
        let push_range_val = |spans: &mut Vec<Span<'static>>, val: f64| {
            let sv = fmt_rtt_nodec(val);
            spans.push(Span::styled(format!("{:>w$}", &sv, w = cw.range_compact), bright));
        };
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(sym("r", "\u{21D5}", "range"), dim));
        push_range_val(&mut spans, min_val);
        spans.push(Span::styled(if ascii { "-" } else { "\u{2194}" }, dim));
        push_range_val(&mut spans, max_val);
    }
    if !hide_b(&BaseStat::Jitter) {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(sym("j", "\u{03b4}", "jitter"), dim));
        if jit_val > 0.0 { push_rtt_val(&mut spans, jit_val, &cw.jitter, bright, mid); }
        else             { push_mtr_placeholder(&mut spans, "\u{2014}", dim, &cw.jitter); }
    }

    if show_drp && !hide_b(&BaseStat::Drops) {
        let (drop_count, drop_denom) = if stats_window {
            let total = s.win_drops as usize + s.window.len();
            (s.win_drops as u64, total as u64)
        } else {
            (s.drops as u64, s.total_sent)
        };
        let num_style = if s.drop_flash > 0 {
            Style::default().fg(Color::White).bg(theme.drop_color).add_modifier(Modifier::BOLD)
        } else if drop_count > 0 && drop_denom > 0 {
            let rate = drop_count as f64 / drop_denom as f64;
            Style::default().fg(theme.drop_num_color(rate)).add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(sym("x", "\u{2717}", "loss"), dim));
        if drop_denom == 0 {
            spans.push(Span::styled(format!("{:>w$}", "\u{2014}", w = cw.drp), dim));
        } else {
            let num_str  = fmt_count(drop_count);
            let denom_str = fmt_count(drop_denom);
            let pad = cw.drp.saturating_sub(num_str.len() + 1 + denom_str.len());
            if pad > 0 { spans.push(Span::raw(" ".repeat(pad))); }
            spans.push(Span::styled(num_str,   num_style));
            spans.push(Span::styled("/",       dim));
            spans.push(Span::styled(denom_str, dim));
        }
    }

    if show_dup {
        let (dup_count, dup_denom) = if stats_window {
            let total = s.win_drops as usize + s.window.len();
            (s.win_dups as u64, total as u64)
        } else {
            (s.dups as u64, s.total_sent)
        };
        let dup_pct   = if dup_denom == 0 { 0.0 } else { dup_count as f64 / dup_denom as f64 * 100.0 };
        let dup_style = if dup_count > 0 {
            Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD)
        } else {
            bright
        };
        let dup_pct_str = format!("{:>4}", if dup_pct < 10.0 { format!("{:.1}%", dup_pct) }
                                          else               { format!("{:.0}%",  dup_pct) });
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(sym("+", "\u{2295}", "dup"), dim));
        spans.push(Span::styled(format!("{:>w$}", fmt_count(dup_count), w = cw.dup), dup_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(dup_pct_str, dup_style));
    }

    // Optional extra stat columns - rendered in stat_order
    let d = "\u{2014}"; // - placeholder for unavailable values
    let has_drops = if stats_window { s.win_drops > 0 } else { s.drops > 0 };
    for stat in &cw.stat_order {
        spans.push(Span::raw(" ".repeat(gap)));
        match stat {
            ExtraStat::Mtr => if let Some(ref col) = cw.mtr {
                spans.push(Span::styled(sym("w", "\u{03a9}", "mtr"), dim)); // Ω
                let mtr_val = if stats_window { s.win_mtr() } else { s.life_mtr() };
                if let Some(mtr) = mtr_val { push_rtt_val(&mut spans, mtr, col, bright, mid); }
                else { push_mtr_placeholder(&mut spans, if has_drops { "\u{221e}" } else { "~" }, if has_drops { bright } else { dim }, col); }
            }
            ExtraStat::Std => if let Some(ref col) = cw.std {
                spans.push(Span::styled(sym("s", "\u{00b1}", "std"), dim)); // ±
                let std_val = if stats_window { s.win_stddev() } else { s.life_stddev() };
                push_rtt_val(&mut spans, std_val, col, bright, mid);
            }
            ExtraStat::P01 => if let Some(ref col) = cw.p01 {
                spans.push(Span::styled(sym("0", "\u{2080}", "p01"), dim)); // ₀
                if stats_window && !s.window.is_empty()           { push_rtt_val(&mut spans, s.win_p01(),    col, bright, mid); }
                else if !stats_window && !s.lifetime_rtts.is_empty() { push_rtt_val(&mut spans, s.life_p01(), col, bright, mid); }
                else                                               { push_mtr_placeholder(&mut spans, d, dim, col); }
            }
            ExtraStat::P10 => if let Some(ref col) = cw.p10 {
                spans.push(Span::styled(sym("1", "\u{2081}", "p10"), dim)); // ₁
                if stats_window && !s.window.is_empty()           { push_rtt_val(&mut spans, s.win_p10(),    col, bright, mid); }
                else if !stats_window && !s.lifetime_rtts.is_empty() { push_rtt_val(&mut spans, s.life_p10(), col, bright, mid); }
                else                                               { push_mtr_placeholder(&mut spans, d, dim, col); }
            }
            ExtraStat::P50 => if let Some(ref col) = cw.p50 {
                spans.push(Span::styled(sym("p", "\u{00bd}", "p50"), dim)); // ½
                if stats_window && !s.window.is_empty()           { push_rtt_val(&mut spans, s.win_median(),  col, bright, mid); }
                else if !stats_window && !s.lifetime_rtts.is_empty() { push_rtt_val(&mut spans, s.life_median(), col, bright, mid); }
                else                                               { push_mtr_placeholder(&mut spans, d, dim, col); }
            }
            ExtraStat::P95 => if let Some(ref col) = cw.p95 {
                spans.push(Span::styled(sym("5", "\u{2085}", "p95"), dim)); // ₅
                if stats_window && !s.window.is_empty()           { push_rtt_val(&mut spans, s.win_p95(),    col, bright, mid); }
                else if !stats_window && !s.lifetime_rtts.is_empty() { push_rtt_val(&mut spans, s.life_p95(), col, bright, mid); }
                else                                               { push_mtr_placeholder(&mut spans, d, dim, col); }
            }
            ExtraStat::P99 => if let Some(ref col) = cw.p99 {
                spans.push(Span::styled(sym("9", "\u{2089}", "p99"), dim)); // ₉
                if stats_window && !s.window.is_empty()           { push_rtt_val(&mut spans, s.win_p99(),    col, bright, mid); }
                else if !stats_window && !s.lifetime_rtts.is_empty() { push_rtt_val(&mut spans, s.life_p99(), col, bright, mid); }
                else                                               { push_mtr_placeholder(&mut spans, d, dim, col); }
            }
            ExtraStat::Cv => if let Some(cv_w) = cw.cv {
                spans.push(Span::styled(sym("%", "%", "cv"), dim));
                let cv = if stats_window { s.win_cv() } else { s.life_cv() };
                if cv > 0.0 { spans.push(Span::styled(format!("{:>w$}", fmt_cv(cv), w = cv_w), bright)); }
                else        { spans.push(Span::styled(format!("{:>w$}", d, w = cv_w), dim)); }
            }
            ExtraStat::Srtt => if let Some(ref col) = cw.srtt {
                spans.push(Span::styled(sym("t", "\u{03c4}", "srtt"), dim)); // τ
                if s.srtt > 0.0 { push_rtt_val(&mut spans, s.srtt, col, bright, mid); }
                else            { push_mtr_placeholder(&mut spans, d, dim, col); }
            }
            ExtraStat::Streak => if let Some(stk_w) = cw.streak {
                let count = s.cur_drop_streak;
                let stk_style = if count > 0 { Style::default().fg(theme.drop_color).add_modifier(Modifier::BOLD) } else { bright };
                spans.push(Span::styled(sym("#", "#", "streak"), dim));
                spans.push(Span::styled(format!("{:>w$}", fmt_count(count as u64), w = stk_w), stk_style));
            }
            ExtraStat::Last => if let Some(last_w) = cw.last {
                // No compact glyph (the value is self-explanatory, e.g. "12m") - only
                // the single-target verbose view gets a "last " word label.
                if verbose_labels { spans.push(Span::styled("last ", dim)); }
                if let Some(t) = s.last_up {
                    let val = fmt_last_up(Some(t), Instant::now());
                    spans.push(Span::styled(format!("{:>w$}", val, w = last_w), bright));
                } else {
                    spans.push(Span::styled(format!("{:>w$}", d, w = last_w), dim));
                }
            }
            ExtraStat::Status => if let Some(status_w) = cw.status {
                // No compact glyph (self-explanatory text, e.g. "31 probes, 30s") -
                // only the single-target verbose view gets a word label. Left-aligned,
                // unlike the numeric columns, since it's free text.
                if verbose_labels { spans.push(Span::styled("status ", dim)); }
                let text = probe_status_text(s, Instant::now(), interval_ms);
                spans.push(Span::styled(format!("{:<w$}", text, w = status_w), bright));
            }
            _ => { spans.pop(); } // remove the gap we pushed for unrecognised/pseudo variants
        }
    }

    // Range bar (suppressed in fullscreen mode - shown separately)
    if show_range {
        for span in build_range_bar_spans(s, ascii, shared_scale, theme, 28, false) {
            spans.push(span);
        }
    }

    Line::from(spans)
}

/// One-row combined line for multi-target fullscreen (used when width >= 120):
///   [label padded]  [stats]  [mode  spinner/changes]
#[allow(clippy::too_many_arguments)]
pub fn build_combined_row_line<'a>(
    s:               &TargetState,
    args:            &Args,
    mode_label:      &str,
    col_widths:      &ColWidths,
    label_color:     Color,
    shared_scale:    f64,
    tick:            u64,
    log_fmt:         &str,
    global_mode:     Option<&str>,  // Some(m) when all targets share mode m → suppress badge if this target matches
    badge_pad_w:     usize,
    ip_changes_slot_w: usize,       // fixed width of ip-changes slot (0 = no target has changes > 1)
    sort_arrow:      Option<bool>,
    show_drp:        bool,
    show_dup:        bool,
    show_inline_trend: bool,
    post_badge_spans: Vec<Span<'static>>,
    trailing_indicators: bool,
    skip_current_rtt: bool,
    gap:             usize,
    addr_gap:        usize,
) -> Line<'a> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    // ── Label (with mode badge prefix when targets have mixed modes) ─────────
    let label_style = if s.threshold_flash > 0 && s.threshold_flash % 2 == 1 {
        Style::default().fg(args.theme.hostname_flash_fg).bg(args.theme.hostname_flash_bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(label_color).add_modifier(Modifier::BOLD)
    };
    let show_mode = args.column_vis.mode.unwrap_or(global_mode != Some(mode_label));
    match sort_arrow {
        Some(up) => {
            let sym   = if up { if args.ascii { "^ " } else { "▲ " } } else { if args.ascii { "v " } else { "▼ " } };
            let color = if up { args.theme.rtt_good } else { args.theme.drop_color };
            spans.push(Span::styled(sym, Style::default().fg(color).add_modifier(Modifier::BOLD)));
        }
        None => spans.push(Span::raw("  ")),
    }
    if show_mode {
        let badge_style = Style::default().fg(args.theme.mode_color(mode_label)).add_modifier(Modifier::DIM);
        let sep = if args.ascii { ">" } else { "\u{203a}" }; // ›
        spans.push(Span::styled(format!("{:<w$} {} ", mode_label, sep, w = badge_pad_w), badge_style));
    }
    // name: custom label, or hostname for DNS targets, blank for pure IP targets
    let name = if s.custom_label {
        s.label.clone()
    } else if s.host.parse::<std::net::IpAddr>().is_err() {
        s.host.clone()
    } else {
        String::new()
    };
    // address: exec command for exec probes; resolved IP otherwise
    let addr: String = if !s.exec_cmd.is_empty() {
        let ell = if args.ascii { "..." } else { "\u{2026}" };
        let w = col_widths.label;
        let chars: Vec<char> = s.exec_cmd.chars().collect();
        if chars.len() <= w {
            s.exec_cmd.clone()
        } else {
            let ell_len = ell.chars().count();
            let take = w.saturating_sub(ell_len);
            format!("{}{}", chars[..take].iter().collect::<String>(), ell)
        }
    } else if s.current_ip.is_none() {
        if args.ipv6 { "?:?:?:?:?:?:?:?".into() } else { "?.?.?.?".into() }
    } else {
        s.current_ip.map(|ip| ip.to_string()).unwrap_or_else(|| s.host.clone())
    };
    if col_widths.name_w > 0 {
        spans.push(Span::styled(format!("{:<w$}", name, w = col_widths.name_w), label_style));
        spans.push(Span::raw("  "));
    }
    // label == 0 means the addr column is hidden (via --columns / 'x' dialog)
    if col_widths.label > 0 {
        let addr_style = if col_widths.name_w > 0 && !name.is_empty() {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            label_style
        };
        spans.push(Span::styled(
            format!("{:<w$}", addr, w = col_widths.label),
            addr_style,
        ));
    }

    if !trailing_indicators {
        // Resolving spinner slot (addr_gap chars wide) so stats columns stay aligned.
        if s.resolving {
            let frames = if args.ascii {
                ["|", "/", "-", "\\"]
            } else {
                ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"]
            };
            let pad = addr_gap.saturating_sub(1);
            if pad > 0 { spans.push(Span::raw(" ".repeat(pad))); }
            spans.push(Span::styled(
                frames[(tick as usize / 2) % 4],
                Style::default().fg(args.theme.resolving).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(" ".repeat(addr_gap)));
        }
        if ip_changes_slot_w > 0 {
            // Forced on shows the counter even at 0/1 changes; auto needs 2+.
            if args.column_vis.resolve.map_or(s.ip_changes > 1, |v| v) {
                let arrow = if args.ascii { "~" } else { "\u{21bb}" };
                let text = format!("{}{}", arrow, s.ip_changes);
                spans.push(Span::styled(text.clone(), Style::default().fg(args.theme.ip_change)));
                let pad = ip_changes_slot_w.saturating_sub(text.chars().count());
                if pad > 0 { spans.push(Span::raw(" ".repeat(pad))); }
            } else {
                spans.push(Span::raw(" ".repeat(ip_changes_slot_w)));
            }
        }
    }

    // ── MTR trend spark ───────────────────────────────────────────────────────
    if show_inline_trend {
        let trend = s.mtr_trend(args.graph_interval);
        let spinner_showing = s.current_ip.is_none()
            || s.history.iter().rev().find(|s| !s.is_pending()).is_none();
        if !spinner_showing && trend != MtrTrend::AllDrops {
            spans.push(Span::raw("  "));
            spans.push(trend_spark_span(trend, args.ascii, &args.theme));
        } else {
            spans.push(Span::raw("   ")); // 3 spaces - same width as "  " + spark char
        }
    }

    // ── Stats: status badge → post_badge_spans (circles in list view) → numerical stats ──
    if trailing_indicators {
        spans.push(Span::raw("  "));
    }
    spans.extend(build_status_badge_spans(s, tick, &args.theme, args.ascii));
    if !post_badge_spans.is_empty() {
        spans.extend(post_badge_spans);
    }
    spans.extend(build_stats_line(s, args.ascii, args.is_window(), shared_scale, col_widths, false, false, !skip_current_rtt, &args.theme, show_drp, show_dup, tick, gap, false, args.interval).spans);

    if !log_fmt.is_empty() {
        let tag = if args.ascii {
            format!("  [{}]", log_fmt)
        } else {
            format!("  [{} \u{25cf}]", log_fmt)
        };
        spans.push(Span::styled(tag, Style::default().fg(args.theme.log_badge).add_modifier(Modifier::BOLD)));
    }

    if trailing_indicators {
        if s.resolving {
            let frames = if args.ascii {
                ["|", "/", "-", "\\"]
            } else {
                ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"]
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                frames[(tick as usize / 2) % 4],
                Style::default().fg(args.theme.resolving).add_modifier(Modifier::BOLD),
            ));
        }
        if s.ip_changes > 1 {
            let arrow = if args.ascii { "~" } else { "\u{21bb}" };
            spans.push(Span::styled(
                format!("  {}{}", arrow, s.ip_changes),
                Style::default().fg(args.theme.ip_change),
            ));
        }
    }

    Line::from(spans)
}

/// Color a latency value using pct deviation from the recent p95 baseline.
/// Three tiers, no background:  < −30% = green  |  −30–75% = white  |  > +75% = red
pub fn latency_style(pct: f64, theme: &Theme) -> Style {
    if pct > TIER_HIGH_PCT {
        Style::default().fg(theme.drop_color)
    } else if pct < TIER_FAST_PCT {
        Style::default().fg(theme.rtt_good)
    } else {
        Style::default().fg(theme.rtt_normal)
    }
}

pub fn build_range_bar_spans(s: &TargetState, ascii: bool, scale_max: f64, theme: &Theme, bar_width: usize, show_col_keys: bool) -> Vec<Span<'static>> {
    #[allow(non_snake_case)] let BAR_WIDTH = bar_width;
    let dim    = Style::default().add_modifier(Modifier::DIM);
    let bright = Style::default().fg(Color::White);

    // Suppress the bar during calibration - same behaviour as the fullscreen graph.
    if s.calibrating.map(|(_, u)| u > crate::time::Instant::now()).unwrap_or(false) {
        let placeholder: String = if ascii { "-".repeat(BAR_WIDTH) } else { "\u{2500}".repeat(BAR_WIDTH) };
        return vec![
            Span::styled("range", dim),
            Span::styled("  ", dim),
            Span::styled("0", dim),
            Span::styled("ms [", dim),
            Span::styled(placeholder, dim),
            Span::styled("] ", dim),
            Span::styled(format!("{}", scale_max as u64), dim),
            Span::styled("ms", dim),
        ];
    }

    // "Never responded" and "sustained drop after alarm flash" show a blank dark-red bar.
    // last_rtt/bar_ema are only set on successful probes, so both <= 0 means no hit ever.
    // For the drop check we find the last *resolved* (non-pending) probe to avoid the
    // in-flight Pending entry masking the true current state.
    let never_responded    = s.last_rtt <= 0.0 && s.bar_ema <= 0.0;
    let last_resolved_drop = s.history.iter().rev()
        .find(|h| !matches!(*h, Sample::Pending))
        .map(|h| matches!(h, Sample::Drop))
        .unwrap_or(false);
    if never_responded || (last_resolved_drop && s.drop_flash == 0) {
        let dark_red = Color::Rgb(50, 10, 10);
        let style    = Style::default().fg(dark_red);
        let ch       = if ascii { "-" } else { "\u{2500}" };
        return vec![
            Span::styled("range", style),
            Span::styled("  ",    style),
            Span::styled("0",     style),
            Span::styled("ms [",  style),
            Span::styled(ch.repeat(BAR_WIDTH), style),
            Span::styled("] ",    style),
            Span::styled(format!("{}", scale_max as u64), style),
            Span::styled("ms",    style),
        ];
    }

    let mut spans = vec![
        Span::styled("range", dim),
        Span::styled("  ", dim),
        Span::styled("0", bright),
    ];
    spans.push(Span::styled("ms [", dim));

    if s.scale_anim.is_some() && !show_col_keys {
        // scale_anim counts DOWN: 5 → 4 → 3 → 2 → 1 → 0 (then None)
        // Scale-up  (anim_up=true)  sweeps LEFT→RIGHT: wavefront travels 0..BAR_WIDTH
        // Scale-down (anim_up=false) sweeps RIGHT→LEFT: wavefront travels BAR_WIDTH..0
        //
        // A travelling label rides the wavefront, e.g. "→200→" or "←100←".
        // Positions inside the label slot render label chars; all others render
        // live data under whichever scale governs that side of the boundary.
        // When show_col_keys is true the col-key header already shows this animation,
        // so the bar renders statically with the new scale.

        // Wall-clock based progress: smooth at any render rate.
        const BAR_ANIM_STEPS: usize = 40;
        let elapsed = s.bar_anim_start.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        let steps_done = ((elapsed / BAR_ANIM_SECS * BAR_ANIM_STEPS as f64) as usize).min(BAR_ANIM_STEPS);
        let old_scale  = if s.scale_anim_old > 0.0 { s.scale_anim_old } else { scale_max };

        // Build the travelling label string (arrows on both sides + new scale value)
        let label_str: String = if ascii {
            if s.scale_anim_up { format!("->{}->", scale_max as u64) }
            else               { format!("<-{}<-", scale_max as u64) }
        } else if s.scale_anim_up {
            format!("\u{2192}{}\u{2192}", scale_max as u64)
        } else {
            format!("\u{2190}{}\u{2190}", scale_max as u64)
        };
        let label_chars: Vec<char> = label_str.chars().collect();
        let label_len = label_chars.len();

        // Wavefront position: left edge of the label.
        // Scale-up:   travels from -(label_len-1) to BAR_WIDTH-1 over 8 steps
        //             so at step 5/5 the label's right edge is at BAR_WIDTH-1
        // Scale-down: travels from BAR_WIDTH to 0 (right→left), label trails left
        let wavefront: isize = if s.scale_anim_up {
            // label right-edge reaches BAR_WIDTH-1 at step 5
            let right_edge = ((steps_done * BAR_WIDTH) / 40) as isize - 1;
            right_edge - label_len as isize + 1
        } else {
            // label left-edge starts at BAR_WIDTH, ends at 0
            
            BAR_WIDTH as isize - ((steps_done * BAR_WIDTH) / 40) as isize
        };

        // boundary: the column just past the label where old/new scale split
        // Everything behind the wavefront (already swept) = new scale.
        // Everything ahead = old scale.
        let boundary: usize = if s.scale_anim_up {
            // new region is to the LEFT of wavefront
            wavefront.max(0) as usize
        } else {
            // new region is to the RIGHT of wavefront+label
            (wavefront + label_len as isize).clamp(0, BAR_WIDTH as isize) as usize
        };

        let scale_for = |pos: usize| -> f64 {
            let in_new = if s.scale_anim_up { pos < boundary } else { pos >= boundary };
            if in_new { scale_max } else { old_scale }
        };

        let is_drop  = matches!(s.history.last(), Some(Sample::Drop));
        let band_lo  = s.win_p10();
        let band_hi  = s.win_p90();
        let win_avg  = s.win_avg();
        let win_p95  = s.win_p95();
        let prev_rtt = s.history.iter().rev().filter_map(|s| s.rtt()).nth(1).unwrap_or(s.last_rtt);
        let trail_brightness = theme.range_trail;

        // Compute positions using each column's governing scale (old vs new during transition).
        let approx_pos_for = |v: f64| -> usize {
            if scale_max > 0.0 { ((v / scale_max) * (BAR_WIDTH - 1) as f64) as usize } else { 0 }
                .min(BAR_WIDTH - 1)
        };
        let governed_pos = |v: f64| -> Option<usize> {
            if v <= 0.0 || v >= f64::MAX { return None; }
            let ap = approx_pos_for(v);
            let g  = scale_for(ap);
            if g > 0.0 { Some((((v / g) * (BAR_WIDTH - 1) as f64) as usize).min(BAR_WIDTH - 1)) }
            else { None }
        };
        let cur_rtt = if s.bar_ema > 0.0 { s.bar_ema } else { s.last_rtt };
        let cur_pos = governed_pos(cur_rtt).unwrap_or(0);
        let avg_pos = if win_avg > 0.0 { governed_pos(win_avg) } else { None };
        let p95_pos = if win_p95 > 0.0 { governed_pos(win_p95) } else { None };
        let band: Option<(usize, usize)> = if band_hi > 0.0 && band_hi > band_lo {
            match (governed_pos(band_lo), governed_pos(band_hi)) {
                (Some(lo), Some(hi)) if hi > lo => Some((lo, hi)),
                _ => None,
            }
        } else { None };

        let cur_style    = latency_style(s.current_pct, theme).add_modifier(Modifier::BOLD);
        let avg_style    = Style::default().fg(theme.rtt_normal).add_modifier(Modifier::BOLD);
        let p95_style    = Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD);
        let band_style_for = |pos: usize| -> Style {
            if avg_pos.is_some_and(|a| pos < a) {
                Style::default().fg(theme.rtt_good).add_modifier(Modifier::DIM)
            } else if p95_pos.is_some_and(|p| pos >= p) {
                Style::default().fg(theme.rtt_warn).add_modifier(Modifier::DIM)
            } else {
                Style::default().fg(Color::DarkGray)
            }
        };

        let label_style = Style::default()
            .fg(Color::Black)
            .bg(theme.c(theme.range_scale))
            .add_modifier(Modifier::BOLD);

        let mut pos = 0usize;
        while pos < BAR_WIDTH {
            let wf = wavefront;
            // Is this column inside the travelling label?
            if (pos as isize) >= wf && (pos as isize) < wf + label_len as isize {
                let ch_idx = (pos as isize - wf) as usize;
                // Emit as many label chars as fit in one span (all same style)
                let chars_left  = label_len - ch_idx;
                let cols_left   = BAR_WIDTH - pos;
                let emit        = chars_left.min(cols_left);
                let slice: String = label_chars[ch_idx..ch_idx + emit].iter().collect();
                spans.push(Span::styled(slice, label_style));
                pos += emit;
                continue;
            }

            if is_drop {
                if pos.is_multiple_of(2) {
                    spans.push(Span::styled(if ascii { "." } else { "\u{00b7}" },
                        Style::default().fg(Color::Rgb(110, 22, 22))));
                } else {
                    spans.push(Span::raw(" "));
                }
                pos += 1;
                continue;
            }
            if pos == cur_pos {
                let marker = if s.last_rtt > prev_rtt * 1.10 { if ascii { "^" } else { "\u{2197}" } }
                             else if s.last_rtt < prev_rtt * 0.90 { if ascii { "v" } else { "\u{2198}" } }
                             else { if ascii { "." } else { "\u{25cf}" } };
                spans.push(Span::styled(marker, cur_style));
                pos += 1;
                continue;
            }
            if avg_pos == Some(pos) {
                spans.push(Span::styled(if ascii { "*" } else { "\u{25c6}" }, avg_style));
                pos += 1;
                continue;
            }
            if p95_pos == Some(pos) {
                spans.push(Span::styled(if ascii { ">" } else { "\u{25b8}" }, p95_style));
                pos += 1;
                continue;
            }
            if !ascii {
                let mut is_trail = false;
                for (age, &trail_frac) in s.trail.iter().enumerate().skip(1) {
                    let trail_pos = (trail_frac as f64 * (BAR_WIDTH.saturating_sub(1)) as f64).round() as usize;
                    if trail_pos == pos {
                        let br = trail_brightness[(age - 1).min(3)];
                        spans.push(Span::styled("\u{2022}",
                            Style::default().fg(Color::Rgb(br, br, br))));
                        is_trail = true;
                        break;
                    }
                }
                if is_trail { pos += 1; continue; }
            }
            let in_band = band.is_some_and(|(lo, hi)| pos >= lo && pos <= hi);
            spans.push(Span::styled(
                if ascii { if in_band { "=" } else { "-" } } else { "\u{2500}" },
                if in_band { band_style_for(pos) } else { dim },
            ));
            pos += 1;
        }

        // Right label: interpolate scale number from old→new proportional to animation progress.
        let frac = steps_done as f64 / BAR_ANIM_STEPS as f64;
        let displayed_scale = (old_scale + (scale_max - old_scale) * frac).round() as u64;
        let scale_label_style = Style::default().fg(theme.c(theme.range_scale)).add_modifier(Modifier::BOLD);
        spans.push(Span::styled("] ", dim));
        spans.push(Span::styled(format!("{}", displayed_scale), scale_label_style));
        spans.push(Span::styled("ms", dim));
        return spans;
    }

    let is_drop  = matches!(s.history.last(), Some(Sample::Drop));
    let band_lo  = s.win_p10();
    let band_hi  = s.win_p90();
    let win_avg  = s.win_avg();
    let win_p95  = s.win_p95();

    let value_to_pos = |v: f64| -> usize {
        if scale_max > 0.0 { ((v / scale_max) * (BAR_WIDTH - 1) as f64) as usize } else { 0 }
            .min(BAR_WIDTH - 1)
    };

    let cur_rtt = if s.bar_ema > 0.0 { s.bar_ema } else { s.last_rtt };
    let cur_pos = value_to_pos(cur_rtt);
    let avg_pos: Option<usize> = if win_avg > 0.0 { Some(value_to_pos(win_avg)) } else { None };
    let p95_pos: Option<usize> = if win_p95 > 0.0 { Some(value_to_pos(win_p95)) } else { None };
    let band: Option<(usize, usize)> = if band_hi > 0.0 && band_hi > band_lo {
        let (lo, hi) = (value_to_pos(band_lo), value_to_pos(band_hi));
        if hi > lo { Some((lo, hi)) } else { None }
    } else { None };

    let prev_rtt         = s.history.iter().rev().filter_map(|s| s.rtt()).nth(1).unwrap_or(s.last_rtt);
    let trail_brightness = theme.range_trail;
    let cur_style        = latency_style(s.current_pct, theme).add_modifier(Modifier::BOLD);
    let avg_style        = Style::default().fg(theme.rtt_normal).add_modifier(Modifier::BOLD);
    let p95_style        = Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD);
    let band_style_for   = |pos: usize| -> Style {
        if avg_pos.is_some_and(|a| pos < a) {
            Style::default().fg(theme.rtt_good).add_modifier(Modifier::DIM)
        } else if p95_pos.is_some_and(|p| pos >= p) {
            Style::default().fg(theme.rtt_warn).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };

    for pos in 0..BAR_WIDTH {
        if is_drop {
            if pos % 2 == 0 {
                spans.push(Span::styled(if ascii { "." } else { "\u{00b7}" },
                    Style::default().fg(Color::Rgb(110, 22, 22))));
            } else {
                spans.push(Span::raw(" "));
            }
            continue;
        }
        if pos == cur_pos {
            let marker = if s.last_rtt > prev_rtt * 1.10 { if ascii { "^" } else { "\u{2197}" } }
                         else if s.last_rtt < prev_rtt * 0.90 { if ascii { "v" } else { "\u{2198}" } }
                         else { if ascii { "." } else { "\u{25cf}" } };
            spans.push(Span::styled(marker, cur_style));
            continue;
        }
        if avg_pos == Some(pos) {
            spans.push(Span::styled(if ascii { "*" } else { "\u{25c6}" }, avg_style));
            continue;
        }
        if p95_pos == Some(pos) {
            spans.push(Span::styled(if ascii { ">" } else { "\u{25b8}" }, p95_style));
            continue;
        }
        if !ascii {
            let mut is_trail = false;
            for (age, &trail_frac) in s.trail.iter().enumerate().skip(1) {
                let trail_pos = (trail_frac as f64 * (BAR_WIDTH.saturating_sub(1)) as f64).round() as usize;
                if trail_pos == pos {
                    let br = trail_brightness[(age - 1).min(3)];
                    spans.push(Span::styled("\u{2022}",
                        Style::default().fg(Color::Rgb(br, br, br))));
                    is_trail = true;
                    break;
                }
            }
            if is_trail { continue; }
        }
        let in_band = band.is_some_and(|(lo, hi)| pos >= lo && pos <= hi);
        spans.push(Span::styled(
            if ascii { if in_band { "=" } else { "-" } } else { "\u{2500}" },
            if in_band { band_style_for(pos) } else { dim },
        ));
    }

    spans.push(Span::styled("] ", dim));
    spans.push(Span::styled(format!("{}", scale_max as u64), bright));
    spans.push(Span::styled("ms", dim));
    spans
}


pub const TARGET_SPARK_W:   usize = 10;
pub const TARGET_SPARK_MIN: usize = 5;

/// Cap on the single-target view's large recent-trend bar. Unlike
/// `TARGET_SPARK_W` (the compact per-row sparkline column), this row gets a
/// full row to itself, but a very wide terminal would otherwise stretch it
/// out to dozens of probes that are mostly at the same fully-faded age
/// brightness (see `brightness_for` below) - stretching it wider stops
/// adding useful signal past this point.
pub const SINGLE_RECENT_MAX_W: usize = 60;

/// Animated spans for the inline range-bar column-key slot during a scale transition.
pub fn build_key_scale_anim_spans(
    s:          &crate::state::TargetState,
    _anim_tick: u8,
    scale_max: f64,
    bar_w:     usize,
    ascii:     bool,
    theme:     &Theme,
) -> Vec<Span<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let label_style = Style::default()
        .fg(Color::Black)
        .bg(theme.c(theme.range_scale))
        .add_modifier(Modifier::BOLD);
    let dash_ch = if ascii { "-" } else { "\u{2500}" };
    const BAR_ANIM_STEPS: usize = 40;
    let elapsed = s.bar_anim_start.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
    let steps_done = ((elapsed / BAR_ANIM_SECS * BAR_ANIM_STEPS as f64) as usize).min(BAR_ANIM_STEPS);
    let label_str: String = if ascii {
        if s.scale_anim_up { format!("->{}->", scale_max as u64) }
        else               { format!("<-{}<-", scale_max as u64) }
    } else if s.scale_anim_up {
        format!("\u{2192}{}\u{2192}", scale_max as u64)
    } else {
        format!("\u{2190}{}\u{2190}", scale_max as u64)
    };
    let label_chars: Vec<char> = label_str.chars().collect();
    let label_len = label_chars.len();
    // Background scale = old value (before this transition); sweep paints over it as needed.
    // Only reverts to scale_max after animation ends (static path in inline_range_key).
    let bg_scale  = if s.scale_anim_old > 0.0 { s.scale_anim_old } else { scale_max };
    let scale_str = format!("{}", bg_scale as u64);
    let scale_chars: Vec<char> = scale_str.chars().collect();
    let scale_start = bar_w.saturating_sub(scale_chars.len()); // first pos covered by scale number
    let wavefront: isize = if s.scale_anim_up {
        let right_edge = ((steps_done * bar_w) / 40) as isize - 1;
        right_edge - label_len as isize + 1
    } else {
        bar_w as isize - ((steps_done * bar_w) / 40) as isize
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(" ", dim));
    spans.push(Span::styled("0", dim));
    let mut pos = 1usize;
    while pos < bar_w {
        if (pos as isize) >= wavefront && (pos as isize) < wavefront + label_len as isize {
            let ch_idx = (pos as isize - wavefront) as usize;
            let emit   = (label_len - ch_idx).min(bar_w - pos);
            let slice: String = label_chars[ch_idx..ch_idx + emit].iter().collect();
            spans.push(Span::styled(slice, label_style));
            pos += emit;
        } else if pos >= scale_start {
            // Show the scale number char at this position.
            let sc_idx = pos - scale_start;
            let ch: String = scale_chars[sc_idx].to_string();
            spans.push(Span::styled(ch, dim));
            pos += 1;
        } else {
            spans.push(Span::styled(dash_ch, dim));
            pos += 1;
        }
    }
    spans
}

/// Compute the column-key label/spans for the inline range-bar header slot.
pub fn inline_range_key(
    anim_state: Option<&crate::state::TargetState>,
    shared_scale: f64,
    bar_w: usize,
    ascii: bool,
    theme: &Theme,
) -> (String, Vec<Span<'static>>) {
    let slot = 1 + bar_w;
    if let (true, Some(s)) = (shared_scale > 0.0, anim_state) {
        if let Some(tick) = s.scale_anim {
            return (String::new(), build_key_scale_anim_spans(s, tick, shared_scale, bar_w, ascii, theme));
        }
    }
    if shared_scale > 0.0 {
        let scale_str = format!("{}", shared_scale as u64);
        let prefix    = " 0";
        let dash_ch   = if ascii { "-" } else { "\u{2500}" };
        let dashes    = slot.saturating_sub(prefix.len() + scale_str.len());
        (format!("{}{}{}", prefix, dash_ch.repeat(dashes), scale_str), vec![])
    } else {
        (" ".repeat(slot), vec![])
    }
}

fn render_target_sparkline_with_scale(slice: &[Sample], scale: f64, width: usize, args: &Args) -> Vec<Span<'static>> {
    let pending_style = Style::default();
    let (gr, gg, gb)  = args.theme.grad_low;
    let (dr, dg, db)  = args.theme.drop_color_rgb();
    let bars_u = ["\u{2581}", "\u{2582}", "\u{2583}", "\u{2584}",
                  "\u{2585}", "\u{2586}", "\u{2587}", "\u{2588}"];
    let bars_a = ["_", ".", "-", "v", "^", "+", "#", "M"];

    let brightness_for = |age: usize| -> f64 {
        match age { 0 => 1.00, 1 => 0.60, 2 => 0.38, 3 => 0.24, _ => 0.15 }
    };
    let level_for = |v: f64| -> usize {
        ((v / scale).clamp(0.0, 1.0) * 7.0).round() as usize
    };

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(width);
    let data_len = slice.len();
    for (age, sample) in slice.iter().rev().enumerate() {
        let br = brightness_for(age);
        match sample {
            Sample::Pending => spans.push(Span::styled(" ", pending_style)),
            Sample::Drop    => {
                let color = Color::Rgb(
                    (dr as f64 * br).round() as u8,
                    (dg as f64 * br).round() as u8,
                    (db as f64 * br).round() as u8,
                );
                let drop_ch = if args.ascii { "x" } else { "\u{2717}" };
                spans.push(Span::styled(drop_ch, Style::default().fg(color).add_modifier(Modifier::BOLD)));
            }
            Sample::Hit(v) => {
                let ch    = if args.ascii { bars_a[7 - level_for(*v)] } else { bars_u[7 - level_for(*v)] };
                let color = Color::Rgb(
                    (gr as f64 * br).round() as u8,
                    (gg as f64 * br).round() as u8,
                    (gb as f64 * br).round() as u8,
                );
                spans.push(Span::styled(ch, Style::default().fg(color)));
            }
        }
    }
    for _ in 0..width.saturating_sub(data_len) {
        spans.push(Span::styled(" ", pending_style));
    }
    spans
}

/// Build a trailing-fade RTT pulse indicator for the target row.
/// Each column = one raw probe; newest is leftmost and fully bright, older probes fade
/// steeply so the just-arrived probe always pops visually ("phosphor decay").
/// Height = RTT on the shared scale, enabling direct cross-target comparison.
/// Drops render as a fading "x" in the drop color; pending/unpainted slots are blank.
pub fn build_target_sparkline_spans(s: &TargetState, args: &Args, width: usize, shared_scale: f64) -> Vec<Span<'static>> {
    let pending_style = Style::default();

    if width == 0 { return vec![]; }

    let history = &s.history;
    if history.is_empty() || s.waiting {
        return (0..width).map(|_| Span::styled(" ", pending_style)).collect();
    }
    let n = history.len();
    let Some(newest) = history[..n].iter().rposition(|h| !h.is_pending()) else {
        return (0..width).map(|_| Span::styled(" ", pending_style)).collect();
    };
    let end   = newest + 1;
    let start = end.saturating_sub(width);
    let slice = &history[start..end];

    let scale = if shared_scale > 0.0 { shared_scale }
                else { slice.iter().filter_map(|h| h.rtt()).fold(0.0_f64, f64::max).max(1.0) };

    render_target_sparkline_with_scale(slice, scale, width, args)
}

#[cfg(test)]
mod recent_sparkline_tests {
    use super::*;
    use crate::cli::Args;
    use clap::Parser;
    use std::time::Duration;

    fn test_args() -> Args {
        let mut args = Args::parse_from(["vlat", "127.0.0.1"]);
        args.theme = args.theme_name.to_theme();
        args
    }

    #[test]
    fn unpainted_columns_are_blank_not_underlined() {
        let args = test_args();
        let s = TargetState::new("127.0.0.1".to_string());
        // No history at all - every column is unpainted.
        let spans = build_target_sparkline_spans(&s, &args, 10, 50.0);
        assert_eq!(spans.len(), 10);
        for span in &spans {
            assert_eq!(span.content.as_ref(), " ", "unpainted column should be blank, not an underline glyph: {span:?}");
        }
    }

    #[test]
    fn unfilled_padding_past_the_data_is_blank() {
        let args = test_args();
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Ok(10.0), 0, false);
        // Ask for a wider sparkline than there is data for - the padding columns
        // (not just a fully-empty sparkline) must also be blank. Data renders
        // newest-first, so the one real sample is spans[0] and padding trails it.
        let spans = build_target_sparkline_spans(&s, &args, 5, 50.0);
        assert_eq!(spans.len(), 5);
        for span in &spans[1..] {
            assert_eq!(span.content.as_ref(), " ", "padding past the one real sample should be blank: {spans:?}");
        }
    }

    #[test]
    fn no_rescale_animation_arrow_is_injected() {
        let args = test_args();
        let mut s = TargetState::new("127.0.0.1".to_string());
        for i in 0..8usize {
            s.record_sent(i);
            s.record_result(i, Ok(10.0 + i as f64), 0, false);
        }
        // Simulate an in-flight rescale animation exactly as trigger_scale_anim would.
        s.bar_anim_start  = Some(crate::time::Instant::now() - Duration::from_millis(50));
        s.scale_anim_old  = 20.0;
        s.scale_anim_new  = 50.0;
        s.scale_anim_up   = true;

        let spans = build_target_sparkline_spans(&s, &args, 8, 50.0);
        for span in &spans {
            let c = span.content.as_ref();
            assert!(c != "\u{25BC}" && c != "\u{25B2}", "no sweep-animation arrow should appear: {spans:?}");
        }
    }
}

#[allow(dead_code)]
pub fn build_timeline_line<'a>(s: &TargetState, args: &Args, term_width: usize, scale: f64) -> Line<'a> {
    let tl_label = "latency  ";
    let now_tag  = "now ";
    let dim      = Style::default().add_modifier(Modifier::DIM);

    let span_ms           = args.span.unwrap_or(args.window) * 1000;
    let samples_in_window = ((span_ms as f64) / args.graph_interval as f64).ceil() as usize;
    let samples_in_window = samples_in_window.max(1);
    let age_secs          = (s.graph_history.len().min(samples_in_window) as f64 * args.graph_interval as f64 / 1000.0) as u64;
    let age_tag = if age_secs >= 3600 {
        format!("  {}h{}m", age_secs / 3600, (age_secs % 3600) / 60)
    } else if age_secs >= 60 {
        format!("  {}m{}s", age_secs / 60, age_secs % 60)
    } else {
        format!("  {}s", age_secs)
    };
    let overhead    = tl_label.len() + now_tag.len() + age_tag.len();
    let spark_width = term_width.saturating_sub(overhead);

    // Slot count: braille packs 2 sub-columns per character, ASCII is 1:1.
    let out_slots = if args.ascii { spark_width } else { spark_width * 2 };

    // Update the incremental column cache (same stable-advancement logic as the
    // fullscreen graph - only the live-edge slot changes between column advances).
    {
        let mut cache = s.sparkline_col_cache.borrow_mut();
        update_sparkline_col_cache(
            &mut cache, &s.graph_history, s.graph_push_count,
            samples_in_window, out_slots,
        );
    }

    // Build display-order slots from the cache.
    // Cache is oldest-first (col 0 = oldest, data_end-1 = newest live edge).
    // Display is newest-left (adjacent to "now"), so reverse the filled portion.
    let display_slots = {
        let cache = s.sparkline_col_cache.borrow();
        let data_end = cache.data_end.min(out_slots);
        let mut slots = Vec::with_capacity(out_slots);
        slots.extend(cache.cols[..data_end].iter().rev().cloned());
        slots.resize(out_slots, Sample::Pending);
        slots
    };

    let mut spark_spans = if args.ascii {
        build_ascii_sparkline_spans(&display_slots, 0.0, scale, &args.theme)
    } else {
        build_braille_sparkline_spans(&display_slots, 0.0, scale, &args.theme)
    };

    if let Some(start) = s.graph_anim_start {
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed < GRAPH_ANIM_SECS && s.scale_anim_old > 0.0 {
            apply_sparkline_scale_anim(
                &mut spark_spans, &display_slots,
                s.scale_anim_up, s.scale_anim_old,
                elapsed, args.ascii, &args.theme,
            );
        }
    }

    let mut spans = Vec::new();
    spans.push(Span::styled(tl_label, dim));
    spans.push(Span::styled(now_tag, dim));
    spans.extend(spark_spans);
    spans.push(Span::styled(age_tag, dim));

    Line::from(spans)
}

/// Compute the (val_min, val_max) range from a slice of cached slots.
/// Returns (0.0, 1.0) when there are no Hit samples so the scale is always valid.
/// Always uses 0.0 as the minimum so the sparkline is zero-anchored, matching
/// the graph's absolute scale (not relative min-to-max).
#[allow(dead_code)]
fn sparkline_val_range(cols: &[Sample]) -> (f64, f64) {
    let mut hi = 0.0_f64;
    for s in cols {
        if let Sample::Hit(v) = s {
            hi = hi.max(*v);
        }
    }
    if hi < 0.001 { return (0.0, 1.0); }
    (0.0, hi)
}

/// Apply the sparkline rescale sweep animation to an already-built span list.
///
/// Mirrors the range-bar scale animation using the same TargetState fields:
///   scale_up = true  (graph scale grew → bars shrink):  ▼ sweeps LEFT → RIGHT
///     swept left side shows new (shorter) bars; unswept right side shows old (taller) bars
///   scale_up = false (graph scale shrank → bars grow):  ▲ sweeps RIGHT → LEFT
///     swept right side shows new (taller) bars; unswept left side shows old (shorter) bars
///
/// `spans` must already be rendered with the new `scale`; this function overwrites the
/// unswept half in-place by re-rendering with `old_scale`.
fn apply_sparkline_scale_anim(
    spans:        &mut [Span<'static>],
    display_slots: &[Sample],
    scale_up:     bool,
    old_scale:    f64,
    elapsed_secs: f64,
    ascii:        bool,
    theme:        &Theme,
) {
    let chars = spans.len();
    if chars == 0 { return; }

    let frac = (elapsed_secs / GRAPH_ANIM_SECS).clamp(0.0, 1.0);
    let pos: usize = if scale_up {
        ((frac * chars as f64) as usize).min(chars - 1)          // ▼ left → right
    } else {
        chars.saturating_sub(1 + (frac * chars as f64) as usize) // ▲ right → left
    };

    // Re-render with the old scale to fill the unswept side.
    let old_spans = if ascii {
        build_ascii_sparkline_spans(display_slots, 0.0, old_scale, theme)
    } else {
        build_braille_sparkline_spans(display_slots, 0.0, old_scale, theme)
    };

    if scale_up {
        // left[0..pos] = new (already in spans); right[pos+1..] = old
        for (dst, s) in spans.iter_mut().zip(&old_spans).take(chars).skip(pos + 1) { *dst = s.clone(); }
    } else {
        // right[pos+1..] = new (already in spans); left[0..pos] = old
        for (dst, s) in spans.iter_mut().zip(&old_spans).take(pos) { *dst = s.clone(); }
    }

    // Arrow at the boundary - direction matches bar movement.
    let arrow = if scale_up {
        if ascii { "v" } else { "\u{25BC}" }  // ▼ bars shrinking
    } else {
        if ascii { "^" } else { "\u{25B2}" }  // ▲ bars growing
    };
    spans[pos] = Span::styled(arrow, Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
}

/// Incrementally update the sparkline column cache for one target.
///
/// Mirrors update_col_cache (same per-sample peak-bucketing) but also manages a
/// locked vertical scale (val_min/val_max).  The scale is recomputed only when
/// columns actually advance, so historical column heights are frozen between
/// advances.  The live-edge (rightmost) column tracks the in-progress bucket
/// peak, so brief spikes appear immediately even before a column advance.
#[allow(dead_code)]
fn update_sparkline_col_cache(
    cache:      &mut SparklineColCache,
    hist:       &[Sample],
    push_count: usize,
    span_cols:  usize,
    out_cols:   usize,
) {
    if cache.out_cols != out_cols || cache.span_cols != span_cols || cache.cols.len() != out_cols {
        let (cols, data_end) = resample_to_cols(hist, span_cols, out_cols);
        let (val_min, val_max) = sparkline_val_range(&cols[..data_end]);
        *cache = SparklineColCache {
            cols, data_end, frac: 0.0,
            push_count, out_cols, span_cols,
            val_min, val_max,
        };
        return;
    }

    let new_samples = push_count.saturating_sub(cache.push_count);
    cache.push_count = push_count;

    if new_samples == 0 { return; }

    // Catching up from a long pause / huge backlog.
    if new_samples > out_cols.saturating_mul(2) {
        let (cols, data_end) = resample_to_cols(hist, span_cols, out_cols);
        let (val_min, val_max) = sparkline_val_range(&cols[..data_end]);
        cache.cols     = cols;
        cache.data_end = data_end;
        cache.frac     = 0.0;
        cache.val_min  = val_min;
        cache.val_max  = val_max;
        return;
    }

    let advance      = out_cols as f64 / span_cols as f64;
    let recent_start = hist.len().saturating_sub(new_samples);
    let mut advanced = false;

    for sample in hist[recent_start..].iter() {
        cache.frac += advance;
        let cols_for_sample = cache.frac.floor() as usize;

        if cols_for_sample == 0 {
            if cache.data_end == 0 {
                cache.cols[0]  = sample.clone();
                cache.data_end = 1;
            } else {
                let live = &mut cache.cols[cache.data_end - 1];
                *live = combine_peak(live, sample);
            }
            continue;
        }

        cache.frac -= cols_for_sample as f64;
        advanced    = true;
        for _ in 0..cols_for_sample {
            if cache.data_end < out_cols {
                cache.cols[cache.data_end] = sample.clone();
                cache.data_end += 1;
            } else {
                cache.cols.drain(..1);
                cache.cols.push(sample.clone());
            }
        }
    }

    // Recompute locked scale only when columns actually advanced.
    if advanced {
        let (val_min, val_max) = sparkline_val_range(&cache.cols[..cache.data_end]);
        cache.val_min = val_min;
        cache.val_max = val_max;
    }
}

/// Render pre-ordered display slots as braille spans.
/// `slots` is indexed left-to-right in display order; `slots.len()` must be even.
/// `val_min`/`val_max` come from the locked cache scale.
#[allow(dead_code)]
fn build_braille_sparkline_spans(slots: &[Sample], val_min: f64, val_max: f64, theme: &Theme) -> Vec<Span<'static>> {
    let width = slots.len() / 2;
    if width == 0 { return vec![]; }

    let dot_rows_for = |v: f64| -> usize {
        let n = ((v - val_min) / (val_max - val_min)).clamp(0.0, 1.0);
        ((n * 3.0).round() as usize).clamp(0, 3) + 1
    };

    let left_bits:  [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    let right_bits: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    let pending_style = Style::default().fg(theme.c(theme.grad_pending));

    let mut spans = Vec::new();
    for char_idx in 0..width {
        let left_sample  = &slots[char_idx * 2];
        let right_sample = &slots[char_idx * 2 + 1];

        if left_sample.is_pending() && right_sample.is_pending() {
            let ch = if char_idx % 2 == 0 { "." } else { " " };
            spans.push(Span::styled(ch, pending_style));
            continue;
        }

        let has_drop = left_sample.is_drop() || right_sample.is_drop();

        let mut bits: u32 = 0x2800;
        if let Sample::Hit(v) = left_sample {
            bits += left_bits[4 - dot_rows_for(*v)..].iter().sum::<u32>();
        }
        if let Sample::Hit(v) = right_sample {
            bits += right_bits[4 - dot_rows_for(*v)..].iter().sum::<u32>();
        }
        let ch = char::from_u32(bits).unwrap_or('\u{28FF}').to_string();

        let peak = match (left_sample.rtt(), right_sample.rtt()) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            _ => None,
        };

        let style = if has_drop {
            Style::default().fg(theme.c(theme.drop_sparkline)).add_modifier(Modifier::BOLD)
        } else {
            match peak {
                None => pending_style,
                Some(v) => {
                    let norm = ((v - val_min) / (val_max - val_min)).clamp(0.0, 1.0);
                    let (r, g, b) = theme.gradient_color(norm);
                    Style::default().fg(Color::Rgb(r, g, b))
                }
            }
        };
        spans.push(Span::styled(ch, style));
    }
    spans
}

/// Render pre-ordered display slots as ASCII spans.
/// `val_min`/`val_max` come from the locked cache scale.
#[allow(dead_code)]
fn build_ascii_sparkline_spans(slots: &[Sample], val_min: f64, val_max: f64, theme: &Theme) -> Vec<Span<'static>> {
    let levels  = ["_", ".", "-", "v", "x", "X", "#", "M"];
    let pending_style = Style::default().fg(theme.c(theme.grad_pending));

    slots.iter().enumerate().map(|(i, s)| match s {
        Sample::Pending => Span::styled(if i % 2 == 0 { "." } else { " " }, pending_style),
        Sample::Drop    => Span::styled("!", Style::default().fg(theme.c(theme.drop_sparkline)).add_modifier(Modifier::BOLD)),
        Sample::Hit(v)  => {
            let level = if (val_max - val_min).abs() < 0.001 { 0 }
                        else { (((v - val_min) / (val_max - val_min)) * 7.0) as usize };
            let norm = ((v - val_min) / (val_max - val_min)).clamp(0.0, 1.0);
            let (r, g, b) = theme.gradient_color(norm);
            Span::styled(levels[level.min(7)], Style::default().fg(Color::Rgb(r, g, b)))
        }
    }).collect()
}

/// Shared layout and animation state for both single and multi-target graphs.
struct GraphLayout {
    graph_h:        usize,
    blank_top_rows: usize,
    sub_rows:       usize,
    bg_start:       usize,
    bg_end:         usize,
    bg_opacity:     f64,
    grid_lines:     Vec<(usize, usize, u64)>, // (sub_row, cell_row, ms_value)
    grid_sub_rows:  Vec<usize>,
    effective_scale:   f64,
    anim_lerp_t:       f64,
    anim_lerp_eased:   f64,
    anim_old_scale:    f64,
    anim_is_up:        bool,
}

fn compute_graph_layout(
    area: Rect,
    states: &[TargetState],
    scale: f64,
) -> GraphLayout {
    let effective_scale = if scale > 0.0 { scale } else { 1.0 };

    // Find a state that has an active animation to use as the baseline for shared layout.
    // In multi-target mode, animations are synced across all states.
    let anim_s = states.iter().find(|s| !s.waiting && s.graph_anim_start.is_some());

    let (anim_old_scale, anim_lerp_t, anim_is_up) = if let Some(s) = anim_s {
        let lerp_t = (s.graph_anim_start.unwrap().elapsed().as_secs_f64() / GRAPH_ANIM_SECS).min(1.0);
        let old_sc = if s.scale_anim_old > 0.0 { s.scale_anim_old } else { effective_scale };
        (old_sc, lerp_t, s.scale_anim_up)
    } else {
        (effective_scale, 1.0, true)
    };
    // Smoothstep easing: slow start, fast middle, slow end - used for all position-driven values.
    let anim_lerp_eased = {
        let t = anim_lerp_t;
        t * t * (3.0 - 2.0 * t)
    };

    // The graph always fills the full area; lerped_scale drives line movement.
    // Animating graph_h independently caused lines to jump at animation start/end.
    let graph_h = area.height as usize;
    let blank_top_rows = 0usize;
    let sub_rows       = graph_h * 4;

    let sub_to_cell = |sr: usize| -> usize {
        if sr >= sub_rows { 0 } else { graph_h - 1 - sr / 4 }
    };

    // Y-axis background: 3-phase animation (shade area contracts/fades during scale change).
    let (bg_start, bg_end, bg_opacity): (usize, usize, f64) = if anim_lerp_t < 1.0 && anim_old_scale > 0.0 {
        let final_frac = if anim_is_up {
            anim_old_scale / effective_scale
        } else {
            1.0 - effective_scale / anim_old_scale
        };
        let (covered, opacity): (usize, f64) = if anim_lerp_t < 0.10 {
            (graph_h, 1.0)
        } else if anim_lerp_t < 0.80 {
            let p = (anim_lerp_t - 0.10) / 0.70;
            let phase2_t = p * p * (3.0 - 2.0 * p); // smoothstep within contraction
            let frac = 1.0 + (final_frac - 1.0) * phase2_t;
            ((graph_h as f64 * frac).round() as usize, 1.0)
        } else {
            let p = (anim_lerp_t - 0.80) / 0.20;
            ((graph_h as f64 * final_frac).round() as usize, 1.0 - p)
        };
        if covered == 0 || opacity < 0.05 {
            (0, 0, 0.0)
        } else if anim_is_up {
            (graph_h.saturating_sub(covered), graph_h, opacity)
        } else {
            (0, covered, opacity)
        }
    } else {
        (0, 0, 0.0)
    };

    // Y-axis gridlines: fixed ms values that animate to new positions on scale change.
    let grid_ms_new = [(effective_scale * 0.25) as u64, (effective_scale * 0.50) as u64, (effective_scale * 0.75) as u64];
    let grid_ms_old = [(anim_old_scale * 0.25) as u64, (anim_old_scale * 0.50) as u64, (anim_old_scale * 0.75) as u64];
    let mut all_grid_ms: Vec<u64> = grid_ms_new.to_vec();
    for &ms in &grid_ms_old {
        if !all_grid_ms.contains(&ms) { all_grid_ms.push(ms); }
    }
    let grid_lines: Vec<(usize, usize, u64)> = {
        let mut result: Vec<(usize, usize, u64)> = Vec::new();
        let mut used_cells: Vec<usize> = Vec::new();
        for &ms in &all_grid_ms {
            if ms == 0 { continue; }
            let old_sub = (ms as f64 / anim_old_scale) * sub_rows as f64;
            let new_sub = (ms as f64 / effective_scale) * sub_rows as f64;
            let lerped = (old_sub * (1.0 - anim_lerp_eased) + new_sub * anim_lerp_eased).round() as usize;
            if lerped == 0 || lerped >= sub_rows { continue; }
            let cell_row = sub_to_cell(lerped);
            if used_cells.contains(&cell_row) { continue; }
            used_cells.push(cell_row);
            result.push((lerped, cell_row, ms));
        }
        result
    };
    let grid_sub_rows: Vec<usize> = grid_lines.iter().map(|&(sr, _, _)| sr).collect();

    GraphLayout {
        graph_h, blank_top_rows, sub_rows,
        bg_start, bg_end, bg_opacity,
        grid_lines, grid_sub_rows,
        effective_scale, anim_lerp_t, anim_lerp_eased, anim_old_scale, anim_is_up,
    }
}

/// Helper to render a consistent Y-axis label with optional animation background.
fn render_y_axis_label(
    cell_row: usize,
    layout:   &GraphLayout,
    args:     &Args,
    avg_ms:   Option<f64>,
    p95_ms:   Option<f64>,
    y_label_w: u16,
    claimed:  impl Fn(usize) -> bool,
) -> Span<'static> {
    let sub_to_cell = |sr: usize| -> usize {
        if sr >= layout.sub_rows { 0 } else { layout.graph_h - 1 - sr / 4 }
    };
    let avg_cell = avg_ms.map(|ms| sub_to_cell(((ms / layout.effective_scale).clamp(0.0, 1.0) * layout.sub_rows as f64).round() as usize)).unwrap_or(usize::MAX);
    let p95_cell = p95_ms.map(|ms| sub_to_cell(((ms / layout.effective_scale).clamp(0.0, 1.0) * layout.sub_rows as f64).round() as usize)).unwrap_or(usize::MAX);

    let is_avg_row = avg_ms.is_some() && cell_row == avg_cell;
    let is_p95_row = p95_ms.is_some() && cell_row == p95_cell && p95_cell != avg_cell;
    let grid_label = layout.grid_lines.iter().find(|&&(_, gr, _)| gr == cell_row && !claimed(cell_row));

    let lerp_ylabel_ms = |old: f64, new: f64| -> u64 {
        (old + (new - old) * layout.anim_lerp_eased).round() as u64
    };

    let fmt_ylabel = |s: &str| -> String {
        let w = y_label_w as usize;
        if s.len() >= w { s[..w].to_string() } else { format!("{:<w$}", s, w = w) }
    };

    let (label, mut style) = if cell_row == 0 {
        (" ".repeat(y_label_w as usize), Style::default())
    } else if cell_row == 1 {
        let disp_ms = lerp_ylabel_ms(layout.anim_old_scale, layout.effective_scale);
        (fmt_ylabel(&format!("{:>4}ms", disp_ms)), Style::default().fg(args.theme.xaxis_now))
    } else if is_avg_row {
        (fmt_ylabel(&format!("avg {:>3}ms", avg_ms.unwrap() as u64)),
         Style::default().fg(args.theme.c(args.theme.graph_avg)))
    } else if is_p95_row {
        (fmt_ylabel(&format!("p95 {:>3}ms", p95_ms.unwrap() as u64)),
         Style::default().fg(args.theme.c(args.theme.graph_p95)))
    } else if let Some(&(_, _, ms_val)) = grid_label {
        (fmt_ylabel(&format!("{:>4}ms", ms_val)), Style::default().fg(args.theme.xaxis_now))
    } else if cell_row == layout.graph_h - 1 {
        (fmt_ylabel("0ms"), Style::default().fg(args.theme.xaxis_now))
    } else {
        (" ".repeat(y_label_w as usize), Style::default())
    };

    if cell_row >= layout.bg_start && cell_row < layout.bg_end {
        let (ar, ag, ab) = args.theme.anim_amber;
        let (rr, _, _)   = args.theme.anim_red;
        let amber_bg = Color::Rgb((layout.bg_opacity * ar as f64) as u8, (layout.bg_opacity * ag as f64) as u8, (layout.bg_opacity * ab as f64) as u8);
        if is_avg_row || is_p95_row || label.trim().is_empty() {
            style = style.bg(amber_bg);
        } else {
            style = Style::default()
                .fg(Color::Rgb(255, 255, 255))
                .bg(Color::Rgb((layout.bg_opacity * rr as f64) as u8, 0, 0));
        }
    }
    Span::styled(label, style)
}

/// Helper to render the scale-change annotation overlay.
fn render_scale_annotation(
    f: &mut Frame,
    area: Rect,
    layout: &GraphLayout,
    args: &Args,
    _states: &[TargetState],
    y_label_w: u16,
) {
    if layout.anim_lerp_t >= 1.0 { return; }

    let arrow = if layout.anim_is_up { if args.ascii { "^" } else { "\u{2191}" } }
                else                { if args.ascii { "v" } else { "\u{2193}" } };
    let text = format!(" {} {}ms ", arrow, layout.effective_scale as u64);
    let tw   = text.len() as u16;
    let data_w = area.width.saturating_sub(y_label_w);

    if data_w >= tw && area.height > 2 {
        let ox = area.x + y_label_w + data_w - tw;
        let oy = area.y + layout.blank_top_rows as u16 + 1;
        let brightness: u8 = ((1.0 - layout.anim_lerp_t) * 220.0) as u8;
        let style = Style::default()
            .fg(Color::Rgb(brightness, brightness / 2, 0))
            .add_modifier(Modifier::BOLD);
        f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), Rect::new(ox, oy, tw, 1));
    }
}

/// Render a filled-area braille graph into `area`.
pub fn render_area_graph(
    f:     &mut Frame,
    area:  Rect,
    s:     &TargetState,
    args:  &Args,
    scale: f64,
) {
    if area.height < 2 || area.width < 4 { return; }

    let y_label_w: u16 = 9;
    let layout     = compute_graph_layout(area, std::slice::from_ref(s), scale);
    let graph_w    = area.width.saturating_sub(y_label_w) as usize;
    let total_cols = graph_w;
    let span_cols  = args.graph_span_cols();

    // Update the per-target column cache (incremental scroll, stable columns).
    {
        let mut cache = s.graph_col_cache.borrow_mut();
        update_col_cache(&mut cache, &s.graph_history, s.graph_push_count, span_cols, total_cols);
    }
    let col_cache = s.graph_col_cache.borrow();
    let samples   = col_cache.cols.as_slice();
    let data_end  = col_cache.data_end;

    // Build column heights for a given vertical scale.
    let build_heights = |sc: f64| -> (Vec<Option<usize>>, Vec<bool>) {
        let mut heights = Vec::with_capacity(total_cols);
        let mut flags   = Vec::with_capacity(total_cols);
        let mut last_h: Option<usize> = None;
        for (col, smp) in samples.iter().enumerate() {
            let h = smp.rtt().map(|ms| {
                let norm = (ms / sc).clamp(0.0, 0.95);
                ((norm * layout.sub_rows as f64).round() as usize).min(layout.sub_rows)
            });
            if h.is_some() { last_h = h; }
            if smp.is_pending() && col < data_end {
                heights.push(last_h); // hold last known; None if no prior hit yet
                flags.push(true);
            } else {
                heights.push(h);
                flags.push(false);
            }
        }
        (heights, flags)
    };

    // Heights use the eased lerped scale so the step-line moves with the animation.
    let lerped_scale = layout.anim_old_scale + (layout.effective_scale - layout.anim_old_scale) * layout.anim_lerp_eased;
    let (col_heights, col_is_pending) = build_heights(lerped_scale);
    let zone_heights: &[Option<usize>] = &col_heights;

    // For each column, the cell_row of the most recent hit (used to place ✕ on drop columns)
    let last_known_cell: Vec<Option<usize>> = {
        let mut result = Vec::with_capacity(total_cols);
        let mut last: Option<usize> = None;
        for h in col_heights.iter().take(total_cols) {
            if let Some(h) = h.filter(|&h| h > 0) {
                let sr = h - 1;
                last = Some(if sr >= layout.sub_rows { 0 } else { layout.graph_h - 1 - sr / 4 });
            }
            result.push(last);
        }
        result
    };

    // For each column, the height of the most recent prior column with data -
    // used by the step-line renderer.  Precomputed in O(N) so the inner loop
    // doesn't re-scan back through earlier columns for every cell.
    let prev_heights: Vec<Option<usize>> = {
        let mut result = Vec::with_capacity(total_cols);
        let mut last: Option<usize> = None;
        for h in col_heights.iter().take(total_cols) {
            result.push(last);
            if h.is_some() { last = *h; }
        }
        result
    };

    // Build output: blank_top_rows spacer (height animation) + graph_h rows of content.
    let mut lines: Vec<Line> = Vec::with_capacity(area.height as usize + 1);
    for _ in 0..layout.blank_top_rows { lines.push(Line::from("")); }

    let sub_to_cell = |sr: usize| -> usize {
        if sr >= layout.sub_rows { 0 } else { layout.graph_h - 1 - sr / 4 }
    };

    // Reference lines for avg and p95
    let avg_ms  = s.win_avg();
    let p95_ms  = s.win_p95();
    let has_ref = avg_ms > 0.0;

    let ms_to_sub = |ms: f64| -> usize {
        ((ms / layout.effective_scale).clamp(0.0, 1.0) * layout.sub_rows as f64).round() as usize
    };
    let avg_sub = if has_ref { ms_to_sub(avg_ms) } else { usize::MAX };
    let p95_sub = if has_ref { ms_to_sub(p95_ms) } else { usize::MAX };
    let avg_cell = if has_ref { sub_to_cell(avg_sub) } else { usize::MAX };
    let p95_cell = if has_ref { sub_to_cell(p95_sub) } else { usize::MAX };

    // Braille dot-bit for a single sub-row within a cell (left column only = dashed look)
    let ref_left_bit: [u32; 4] = [0x40, 0x04, 0x02, 0x01];

    let claimed = |row: usize| -> bool {
        row <= 1 || row == layout.graph_h - 1
            || (has_ref && row == avg_cell)
            || (has_ref && row == p95_cell && p95_cell != avg_cell)
    };

    for cell_row in 0..layout.graph_h {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(render_y_axis_label(cell_row, &layout, args, Some(avg_ms), Some(p95_ms), y_label_w, claimed));

        let base_sub = (layout.graph_h - 1 - cell_row) * 4;
        let avg_dot = if avg_sub >= base_sub && avg_sub < base_sub + 4 { Some(avg_sub - base_sub) } else { None };
        let p95_dot = if p95_sub >= base_sub && p95_sub < base_sub + 4 { Some(p95_sub - base_sub) } else { None };
        for col in 0..total_cols {
            let filled_sub = zone_heights[col];

            // Real drop - subtle column shading with ✕ marker at last known line position
            if samples[col].is_drop() && col < data_end {
                let marker_row = last_known_cell[col].unwrap_or(layout.graph_h - 1);
                let at_marker  = cell_row == marker_row;
                let show_x     = (col_cache.drain_count + col).is_multiple_of(2);
                let x_color    = args.theme.c(args.theme.drop_marker);
                let span = if args.ascii {
                    if at_marker && show_x {
                        Span::styled("X", Style::default().fg(x_color).add_modifier(Modifier::BOLD))
                    } else {
                        let c = if cell_row % 2 == 0 { "." } else { " " };
                        Span::styled(c, Style::default().fg(args.theme.c(args.theme.drop_bg_ascii)))
                    }
                } else if at_marker && show_x {
                    Span::styled("\u{2715}", Style::default().fg(x_color).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled("\u{2591}", Style::default().fg(args.theme.c(args.theme.drop_bg_unicode)))
                };
                spans.push(span);
                continue;
            }

            let filled = match filled_sub {
                None => { spans.push(Span::raw(" ")); continue; }
                Some(h) => h,
            };

            // Step-line chart: determine box-drawing character for this (cell_row, col)
            let curr_cell_pos = if filled > 0 { sub_to_cell(filled - 1) } else { layout.graph_h - 1 };
            let prev_cell_pos = prev_heights[col].map(|h| if h > 0 { sub_to_cell(h - 1) } else { layout.graph_h - 1 });

            let data_char: Option<char> = if col >= data_end {
                None
            } else if cell_row == curr_cell_pos {
                Some(match prev_cell_pos {
                    None => '─', Some(p) if p == curr_cell_pos => '─', Some(p) if p > curr_cell_pos => '╭', _ => '╰',
                })
            } else if let Some(pc) = prev_cell_pos {
                let (top_row, bot_row) = (curr_cell_pos.min(pc), curr_cell_pos.max(pc));
                if cell_row > top_row && cell_row < bot_row { Some('│') }
                else if cell_row == pc { if pc > curr_cell_pos { Some('╯') } else { Some('╮') } }
                else { None }
            } else { None };

            let (ref_bits, ref_color) = if col < data_end {
                let mut rb = 0u32;
                let mut rc: Option<(u8,u8,u8)> = None;
                if let Some(dot) = avg_dot { rb |= ref_left_bit[dot]; rc = Some(args.theme.graph_avg); }
                if let Some(dot) = p95_dot { rb |= ref_left_bit[dot]; rc = Some(args.theme.graph_p95); }
                (rb, rc)
            } else { (0, None) };

            let line_color = if col_is_pending[col] { args.theme.c(args.theme.graph_pending) } else { args.theme.c(args.theme.targets[0]) };

            let ch = if args.ascii {
                let is_ref = col < data_end && (avg_dot.is_some() || p95_dot.is_some());
                let ref_color_ascii = if p95_dot.is_some() { args.theme.graph_p95 } else { args.theme.graph_avg };
                if is_ref && data_char.is_some() {
                    Span::styled("-", Style::default().fg(Color::Rgb(ref_color_ascii.0, ref_color_ascii.1, ref_color_ascii.2)))
                } else if let Some(dc) = data_char {
                    let s = match dc { '│' => "|", _ => "-" };
                    Span::styled(s, Style::default().fg(line_color))
                } else {
                    let is_grid = col < data_end && layout.grid_sub_rows.iter().any(|&gs| gs >= base_sub && gs < base_sub + 4);
                    if is_grid { Span::styled("-", Style::default().fg(args.theme.c(args.theme.graph_grid))) }
                    else       { Span::styled(" ", Style::default()) }
                }
            } else if let Some(dc) = data_char {
                Span::styled(dc.to_string(), Style::default().fg(line_color))
            } else if ref_bits > 0 {
                let bits_with_ref = 0x2800 + ref_bits;
                let color = ref_color.unwrap_or(args.theme.graph_avg);
                let ch = char::from_u32(bits_with_ref).unwrap_or('\u{2508}').to_string();
                Span::styled(ch, Style::default().fg(Color::Rgb(color.0, color.1, color.2)))
            } else {
                let is_grid = col < data_end && layout.grid_sub_rows.iter().any(|&gs| gs >= base_sub && gs < base_sub + 4);
                if is_grid { Span::styled("\u{2508}", Style::default().fg(args.theme.c(args.theme.graph_grid))) }
                else       { Span::styled(" ", Style::default()) }
            };
            spans.push(ch);
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines), Rect::new(area.x, area.y, area.width, area.height));
    render_scale_annotation(f, area, &layout, args, std::slice::from_ref(s), y_label_w);
}

struct MultiGraphColCache {
    area:            Rect,
    effective_scale: u64,
    span_cols:       usize,
    num_targets:     usize,
    drain_counts:    Vec<usize>,
    push_counts:     Vec<usize>,
    data_ends:       Vec<usize>,
    graph_h:         usize,
    sub_rows:        usize,
    blank_top_rows:  usize,
    paint_order:     Vec<usize>,                          // global back-to-front z-order
    col_spans:       Vec<Vec<Option<Span<'static>>>>,   // [col][row]
    col_heights:     Vec<Vec<Option<usize>>>,            // [col][target]
    theme_name:      &'static str,
}

thread_local! {
    static MULTI_GRAPH_CACHE: RefCell<Option<MultiGraphColCache>> = const { RefCell::new(None) };
}

struct MultiTargetInfo {
    state_idx:   usize,
    samples:     Vec<Sample>,
    data_end:    usize,
    drain_count: usize,
    push_count:  usize,
    cr: u8, cg: u8, cb: u8,
    line_color:  Color,
}

#[allow(clippy::too_many_arguments)]
fn render_multi_column(
    col:              usize,
    targets:          &[MultiTargetInfo],
    paint_order:      &[usize],
    prev_col_heights: &[Vec<Option<usize>>],
    graph_h:          usize,
    sub_rows:         usize,
    render_scale:     f64,
    ascii:            bool,
) -> (Vec<Option<Span<'static>>>, Vec<Option<usize>>) {
    let sub_to_cell = |sr: usize| -> usize {
        if sr >= sub_rows { 0 } else { graph_h - 1 - sr / 4 }
    };
    let nt = targets.len();

    let mut heights: Vec<Option<usize>> = Vec::with_capacity(nt);
    let mut drops:   Vec<bool>           = Vec::with_capacity(nt);
    let mut pending: Vec<bool>           = Vec::with_capacity(nt);
    let mut prev_h:  Vec<Option<usize>>  = Vec::with_capacity(nt);

    for (ti, t) in targets.iter().enumerate() {
        let smp      = &t.samples[col];
        let in_range = col < t.data_end;
        drops.push(smp.is_drop() && in_range);
        let rtt = smp.rtt();

        let h = rtt.map(|ms| {
            let norm = (ms / render_scale).clamp(0.0, 0.95);
            ((norm * sub_rows as f64).round() as usize).min(sub_rows)
        });
        let last_h = (0..prev_col_heights.len()).rev().find_map(|c| prev_col_heights[c][ti]);
        prev_h.push(last_h);

        if smp.is_pending() && in_range {
            heights.push(last_h);
            pending.push(true);
        } else {
            heights.push(h);
            pending.push(false);
        }
    }

    // Paint order is uniform for the whole graph (back-to-front): the caller
    // ranks targets once per frame so the z-layering is identical in every
    // column.  Positions come from `heights` (data only), so the layering never
    // moves a line - it only decides which line wins a shared cell and which
    // crossover glyph is drawn.
    let mut spans: Vec<Option<Span<'static>>> = vec![None; graph_h];
    for &ti in paint_order {
        let t = &targets[ti];
        if drops[ti] {
            let span = if ascii {
                Span::styled("v", Style::default().fg(t.line_color).add_modifier(Modifier::BOLD))
            } else {
                Span::styled("\u{25be}", Style::default().fg(t.line_color).add_modifier(Modifier::BOLD))
            };
            spans[graph_h - 1] = Some(span);
            continue;
        }
        let filled = match heights[ti] { None => continue, Some(h) => h };
        if col >= t.data_end { continue; }

        let curr_cell = if filled > 0 { sub_to_cell(filled - 1) } else { graph_h - 1 };
        let prev_cell = prev_h[ti].map(|h| if h > 0 { sub_to_cell(h - 1) } else { graph_h - 1 });
        let color = if pending[ti] { Color::Rgb(t.cr / 4, t.cg / 4, t.cb / 4) } else { t.line_color };

        for (cell_row, span_slot) in spans.iter_mut().enumerate().take(graph_h) {
            let data_char: Option<&'static str> = if cell_row == curr_cell {
                Some(match prev_cell {
                    None                      => if ascii { "-" } else { "─" },
                    Some(p) if p == curr_cell => if ascii { "-" } else { "─" },
                    Some(p) if p > curr_cell  => if ascii { "-" } else { "╭" },
                    _                         => if ascii { "-" } else { "╰" },
                })
            } else if let Some(pc) = prev_cell {
                let (top, bot) = (curr_cell.min(pc), curr_cell.max(pc));
                if cell_row > top && cell_row < bot { Some(if ascii { "|" } else { "│" }) }
                else if cell_row == pc { if pc > curr_cell { Some(if ascii { "-" } else { "╯" }) } else { Some(if ascii { "-" } else { "╮" }) } }
                else { None }
            } else { None };

            if let Some(ch) = data_char {
                *span_slot = Some(Span::styled(ch, Style::default().fg(color)));
            }
        }
    }

    (spans, heights)
}

/// Render an overlaid step-line graph for multiple targets.
pub fn render_area_graph_multi(
    f:          &mut Frame,
    area:       Rect,
    states:     &[TargetState],
    args:       &Args,
    scale:      f64,
    sort_order: &[usize],
) {
    if area.height < 2 || area.width < 4 || states.is_empty() { return; }

    let y_label_w: u16 = 9;
    let layout     = compute_graph_layout(area, states, scale);
    let graph_w    = area.width.saturating_sub(y_label_w) as usize;
    let total_cols = graph_w;
    let span_cols  = args.graph_span_cols();
    let render_scale    = layout.effective_scale;
    let cache_key_scale = render_scale.to_bits();

    let mut targets: Vec<MultiTargetInfo> = Vec::new();
    for (ti, s) in states.iter().enumerate() {
        if s.waiting { continue; }
        let (cr, cg, cb) = args.theme.target_color(ti);
        {
            let mut cache = s.graph_col_cache.borrow_mut();
            update_col_cache(&mut cache, &s.graph_history, s.graph_push_count, span_cols, total_cols);
        }
        let col_cache = s.graph_col_cache.borrow();
        targets.push(MultiTargetInfo {
            state_idx: ti,
            samples: col_cache.cols.clone(),
            data_end: col_cache.data_end,
            drain_count: col_cache.drain_count,
            push_count: s.graph_push_count,
            cr, cg, cb,
            line_color: Color::Rgb(cr, cg, cb),
        });
    }

    // Global z-order (back-to-front), uniform across every column.  Ranked by
    // the active sort order: the target at sort_order[0] (top of the list -
    // lowest MTR when sorted by mtr) is painted LAST so it sits in front.  When
    // the sort order changes the layering changes with it; positions are
    // unaffected because they derive solely from each target's cached samples.
    let paint_order: Vec<usize> = {
        let rank_of = |state_idx: usize| -> usize {
            sort_order.iter().position(|&s| s == state_idx).unwrap_or(usize::MAX)
        };
        let mut po: Vec<usize> = (0..targets.len()).collect();
        // Descending by rank so rank 0 (top of the list) is painted last = in front.
        po.sort_by(|&a, &b| rank_of(targets[b].state_idx).cmp(&rank_of(targets[a].state_idx)));
        po
    };

    MULTI_GRAPH_CACHE.with(|cell| {
        let mut borrow = cell.borrow_mut();

        let needs_rebuild = match borrow.as_ref() {
            None => true,
            Some(c) => c.area != area || c.effective_scale != cache_key_scale
                     || c.span_cols != span_cols || c.num_targets != targets.len()
                     || c.graph_h != layout.graph_h || c.sub_rows != layout.sub_rows
                     || c.paint_order != paint_order
                     || c.theme_name != args.theme.name,
        };

        if needs_rebuild {
            let mut col_spans   = Vec::with_capacity(total_cols);
            let mut col_heights: Vec<Vec<Option<usize>>> = Vec::with_capacity(total_cols);
            for col in 0..total_cols {
                let (sp, ht) = render_multi_column(col, &targets, &paint_order, &col_heights, layout.graph_h, layout.sub_rows, render_scale, args.ascii);
                col_spans.push(sp);
                col_heights.push(ht);
            }
            *borrow = Some(MultiGraphColCache {
                area, effective_scale: cache_key_scale, span_cols,
                num_targets: targets.len(),
                drain_counts: targets.iter().map(|t| t.drain_count).collect(),
                push_counts:  targets.iter().map(|t| t.push_count).collect(),
                data_ends:    targets.iter().map(|t| t.data_end).collect(),
                graph_h: layout.graph_h, sub_rows: layout.sub_rows,
                blank_top_rows: layout.blank_top_rows,
                paint_order: paint_order.clone(),
                col_spans, col_heights,
                theme_name: args.theme.name,
            });
        } else {
            let c = borrow.as_mut().unwrap();

            let mut scroll_n = 0usize;
            let mut ok = true;
            for (i, t) in targets.iter().enumerate() {
                let delta = t.drain_count.saturating_sub(c.drain_counts[i]);
                if i == 0 { scroll_n = delta; }
                else if delta != scroll_n { ok = false; break; }
            }
            if !ok || scroll_n > total_cols {
                let mut col_spans   = Vec::with_capacity(total_cols);
                let mut col_heights: Vec<Vec<Option<usize>>> = Vec::with_capacity(total_cols);
                for col in 0..total_cols {
                    let (sp, ht) = render_multi_column(col, &targets, &paint_order, &col_heights, c.graph_h, c.sub_rows, render_scale, args.ascii);
                    col_spans.push(sp);
                    col_heights.push(ht);
                }
                c.col_spans   = col_spans;
                c.col_heights = col_heights;
            } else if scroll_n > 0 {
                c.col_spans.drain(..scroll_n);
                c.col_heights.drain(..scroll_n);
                for _ in 0..scroll_n {
                    let col = c.col_spans.len();
                    let (sp, ht) = render_multi_column(col, &targets, &paint_order, &c.col_heights, c.graph_h, c.sub_rows, render_scale, args.ascii);
                    c.col_spans.push(sp);
                    c.col_heights.push(ht);
                }
            } else {
                let data_end_changed = targets.iter().enumerate().any(|(i, t)| t.data_end != c.data_ends[i]);
                let push_changed     = targets.iter().enumerate().any(|(i, t)| t.push_count != c.push_counts[i]);
                if data_end_changed {
                    let mut col_spans   = Vec::with_capacity(total_cols);
                    let mut col_heights: Vec<Vec<Option<usize>>> = Vec::with_capacity(total_cols);
                    for col in 0..total_cols {
                        let (sp, ht) = render_multi_column(col, &targets, &paint_order, &col_heights, c.graph_h, c.sub_rows, render_scale, args.ascii);
                        col_spans.push(sp);
                        col_heights.push(ht);
                    }
                    c.col_spans   = col_spans;
                    c.col_heights = col_heights;
                } else if push_changed && !c.col_spans.is_empty() {
                    let last = c.col_spans.len() - 1;
                    c.col_heights.truncate(last);
                    let (sp, ht) = render_multi_column(last, &targets, &paint_order, &c.col_heights, c.graph_h, c.sub_rows, render_scale, args.ascii);
                    c.col_spans[last] = sp;
                    c.col_heights.push(ht);
                }
            }

            c.drain_counts = targets.iter().map(|t| t.drain_count).collect();
            c.push_counts  = targets.iter().map(|t| t.push_count).collect();
            c.data_ends    = targets.iter().map(|t| t.data_end).collect();
            c.blank_top_rows = layout.blank_top_rows;
        }

        let c = borrow.as_ref().unwrap();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(area.height as usize);
        for _ in 0..c.blank_top_rows { lines.push(Line::from("")); }

        for cell_row in 0..c.graph_h {
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(render_y_axis_label(cell_row, &layout, args, None, None, y_label_w, |r| r <= 1 || r == c.graph_h - 1));

            let base_sub = (c.graph_h - 1 - cell_row) * 4;
            let is_grid_row = layout.grid_sub_rows.iter().any(|&gs| gs >= base_sub && gs < base_sub + 4);

            for col in 0..total_cols {
                if let Some(ref span) = c.col_spans[col][cell_row] {
                    spans.push(span.clone());
                } else if is_grid_row {
                    let ch = if args.ascii { "-" } else { "\u{2508}" };
                    spans.push(Span::styled(ch, Style::default().fg(args.theme.c(args.theme.graph_grid))));
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            lines.push(Line::from(spans));
        }

        f.render_widget(Paragraph::new(lines), Rect::new(area.x, area.y, area.width, area.height));
    });

    render_scale_annotation(f, area, &layout, args, states, y_label_w);
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::state::GraphColCache;
    use crate::types::Sample;

    fn hit(v: f64) -> Sample { Sample::Hit(v) }

    // ── combine_peak ────────────────────────────────────────────────────

    #[test]
    fn combine_peak_takes_max_rtt() {
        assert_eq!(combine_peak(&hit(10.0), &hit(20.0)), Sample::Hit(20.0));
        assert_eq!(combine_peak(&hit(20.0), &hit(10.0)), Sample::Hit(20.0));
    }

    #[test]
    fn combine_peak_drop_dominates() {
        assert_eq!(combine_peak(&Sample::Drop, &hit(10.0)), Sample::Drop);
        assert_eq!(combine_peak(&hit(10.0), &Sample::Drop), Sample::Drop);
    }

    #[test]
    fn combine_peak_pending_with_hit_returns_hit() {
        assert_eq!(combine_peak(&Sample::Pending, &hit(15.0)), Sample::Hit(15.0));
        assert_eq!(combine_peak(&hit(15.0), &Sample::Pending), Sample::Hit(15.0));
    }

    #[test]
    fn combine_peak_pending_with_pending_stays_pending() {
        assert_eq!(combine_peak(&Sample::Pending, &Sample::Pending), Sample::Pending);
    }

    // ── bucket_peak ─────────────────────────────────────────────────────

    #[test]
    fn bucket_peak_empty_is_pending() {
        assert_eq!(bucket_peak(&[]), Sample::Pending);
    }

    #[test]
    fn bucket_peak_picks_max_rtt() {
        let chunk = vec![hit(10.0), hit(50.0), hit(20.0), hit(100.0), hit(30.0)];
        assert_eq!(bucket_peak(&chunk), Sample::Hit(100.0));
    }

    #[test]
    fn bucket_peak_drop_dominates_over_hits() {
        let chunk = vec![hit(10.0), hit(200.0), Sample::Drop, hit(50.0)];
        assert_eq!(bucket_peak(&chunk), Sample::Drop);
    }

    #[test]
    fn bucket_peak_all_pending_is_pending() {
        let chunk = vec![Sample::Pending, Sample::Pending];
        assert_eq!(bucket_peak(&chunk), Sample::Pending);
    }

    // ── resample_to_cols compression ────────────────────────────────────

    #[test]
    fn resample_compression_preserves_spike_peak() {
        // 10 samples → 5 cols (compression 2:1).  A single spike anywhere should
        // surface as the peak of its bucket rather than be averaged or dropped.
        let mut hist = vec![hit(10.0); 10];
        hist[3] = hit(500.0);  // spike in bucket [2,4)
        let (cols, data_end) = resample_to_cols(&hist, 10, 5);
        assert_eq!(data_end, 5);
        assert_eq!(cols.len(), 5);
        assert_eq!(cols[1], hit(500.0), "bucket containing index 3 must show 500");
        assert!(cols.iter().any(|s| s == &hit(500.0)), "spike must survive compression");
    }

    #[test]
    fn resample_compression_drop_overrides_bucket() {
        let mut hist = vec![hit(10.0); 10];
        hist[5] = Sample::Drop;
        let (cols, _) = resample_to_cols(&hist, 10, 5);
        assert_eq!(cols[2], Sample::Drop, "drop in bucket [4,6) must dominate");
    }

    #[test]
    fn resample_compression_right_anchored() {
        // newest sample (index 9) must occupy the rightmost bucket
        let mut hist = vec![hit(10.0); 10];
        hist[9] = hit(999.0);
        let (cols, _) = resample_to_cols(&hist, 10, 5);
        assert_eq!(cols[4], hit(999.0));
    }

    // ── resample_to_cols stretch ────────────────────────────────────────

    #[test]
    fn resample_stretch_left_anchored_fillup() {
        // 3 samples, span_cols=10, out_cols=5 - partial fill, oldest at col 0.
        let hist = vec![hit(10.0), hit(20.0), hit(30.0)];
        let (cols, data_end) = resample_to_cols(&hist, 10, 5);
        assert!(data_end <= 5, "should not exceed out_cols");
        assert!(data_end >= 1, "should fill at least 1 col");
        // the rightmost real column should reflect the newest sample
        let last = cols[..data_end].last().unwrap();
        assert_eq!(*last, hit(30.0));
        // padding to the right is Pending
        for c in cols.iter().skip(data_end) {
            assert_eq!(*c, Sample::Pending);
        }
    }

    // ── update_col_cache spike preservation ─────────────────────────────

    fn fill_cache(cache: &mut GraphColCache, hist: &mut Vec<Sample>, push_count: &mut usize,
                  span_cols: usize, out_cols: usize, samples: &[Sample]) {
        for s in samples {
            hist.push(s.clone());
            *push_count += 1;
            update_col_cache(cache, hist, *push_count, span_cols, out_cols);
        }
    }

    #[test]
    fn update_col_cache_preserves_spike_within_bucket() {
        // span_cols=10, out_cols=5 → advance=0.5, 2 samples per col.
        // A spike at sample 1 (within bucket containing samples 0,1) must NOT
        // be discarded when sample 2 (lower RTT) advances the column.
        let mut cache = GraphColCache::new();
        cache.cols = vec![Sample::Pending; 5];
        let mut hist = Vec::new();
        let mut pc   = 0;
        fill_cache(&mut cache, &mut hist, &mut pc, 10, 5,
                   &[hit(10.0), hit(500.0), hit(20.0)]);
        // After 3 samples (frac 0.5, 1.0, 1.5): bucket 0 = peak(s0, s1) = 500,
        // bucket 1 = s2 = 20 (live, possibly fed forward).
        // The spike must be visible somewhere in the cached cols.
        assert!(cache.cols[..cache.data_end].iter().any(|s| s == &hit(500.0)),
                "spike must be visible in cached cols: got {:?}", &cache.cols[..cache.data_end]);
    }

    #[test]
    fn update_col_cache_live_edge_updates_between_advances() {
        // advance=0.25 (1 col per 4 samples).  Successive non-advancing samples
        // must update the live edge as a peak so spikes appear immediately.
        let mut cache = GraphColCache::new();
        cache.cols = vec![Sample::Pending; 4];
        let mut hist = Vec::new();
        let mut pc   = 0;
        fill_cache(&mut cache, &mut hist, &mut pc, 16, 4,
                   &[hit(10.0)]);
        assert_eq!(cache.cols[0], hit(10.0));
        // Spike arrives BEFORE the bucket completes - it must show on the live edge.
        fill_cache(&mut cache, &mut hist, &mut pc, 16, 4,
                   &[hit(200.0)]);
        assert_eq!(cache.cols[cache.data_end - 1], hit(200.0),
                   "live edge must reflect in-progress peak");
        // A subsequent lower sample within the same bucket must NOT erase the peak.
        fill_cache(&mut cache, &mut hist, &mut pc, 16, 4,
                   &[hit(15.0)]);
        assert_eq!(cache.cols[cache.data_end - 1], hit(200.0),
                   "peak must survive a lower follow-up sample within the bucket");
    }

    #[test]
    fn update_col_cache_steady_state_scrolls_left() {
        // span_cols=4, out_cols=4 → advance=1, every sample shifts the cache by 1.
        let mut cache = GraphColCache::new();
        cache.cols = vec![Sample::Pending; 4];
        let mut hist = Vec::new();
        let mut pc   = 0;
        fill_cache(&mut cache, &mut hist, &mut pc, 4, 4,
                   &[hit(1.0), hit(2.0), hit(3.0), hit(4.0)]);
        assert_eq!(cache.data_end, 4);
        assert_eq!(&cache.cols, &[hit(1.0), hit(2.0), hit(3.0), hit(4.0)]);
        // One more sample should drop the leftmost and append on the right.
        fill_cache(&mut cache, &mut hist, &mut pc, 4, 4, &[hit(5.0)]);
        assert_eq!(&cache.cols, &[hit(2.0), hit(3.0), hit(4.0), hit(5.0)]);
    }

    #[test]
    fn update_col_cache_drop_propagates_into_bucket() {
        // 2 samples per col (advance=0.5).  A Drop within the bucket dominates.
        let mut cache = GraphColCache::new();
        cache.cols = vec![Sample::Pending; 5];
        let mut hist = Vec::new();
        let mut pc   = 0;
        fill_cache(&mut cache, &mut hist, &mut pc, 10, 5,
                   &[hit(10.0), Sample::Drop, hit(20.0)]);
        assert!(cache.cols[..cache.data_end].iter().any(|s| s.is_drop()),
                "drop must surface in cached cols");
    }
}
