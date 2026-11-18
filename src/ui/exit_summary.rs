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

//! The end-of-run summary for a single target, in every view: three lines
//! shaped like the single view's own block, so a run ends looking like it ran.
//!
//! ```text
//! vlat: tcp › example.org  93.184.216.34  ran 2h 14m 05s
//!       avg 12.4  range 10-88  jitter 1.58  loss 9·8,040 (0.11%)  quality good  p95 15.0
//!       sent 8,040  received 8,031  address changes 2
//! ```
//!
//! - Line 1 is the single view's header line (`build_header_line`: mode badge,
//!   name, address or command), with the run time where
//!   the live UP badge and status text were - neither means anything once the
//!   run is over.
//! - Line 2 is the single view's stats line, word labels and all, over the
//!   whole run: the base stats not hidden with `x`, then whichever numeric
//!   columns are on (p95 by default) and the quality word (on by default),
//!   graded over the whole run rather than the live column's last 5 minutes
//!   (`run_quality_cell`). Widgets (recent, bar, events), the live status text
//!   and ages (seen, lost) are left out. It wraps between label/value pairs
//!   only when the terminal is too narrow.
//! - Line 3 is the counts: sent, received, then dup and address changes
//!   when there were any. (Address changes live here rather than as line 1's
//!   `↻N` counter, which only appears from 2 changes on.)

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::cli::{Args, BaseStat, ExtraStat};
use crate::state::TargetState;

use super::widgets::build_header_line;
use super::{fmt_cv, fmt_jitpct, fmt_rtt, fmt_rtt_nodec};
use super::status_text::{run_quality_cell, Quality, QualityCell};

/// Columns of the `vlat: ` wordmark; lines 2-3 are indented to match.
const INDENT: usize = 6;
/// Gap between label/value pairs, as in the single view's stats line.
const GAP: usize = 2;

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) { out.push(','); }
        out.push(ch);
    }
    out
}

fn pct(n: u64, of: u64) -> String {
    let p = if of == 0 { 0.0 } else { n as f64 / of as f64 * 100.0 };
    if p == 0.0 || p >= 10.0 { format!("{:.0}%", p) }
    else if p >= 1.0 { format!("{:.1}%", p) }
    else { format!("{:.2}%", p) }
}

/// One label/value pair (or a bare value when `label` is empty).
struct Item { spans: Vec<Span<'static>> }

impl Item {
    fn width(&self) -> usize { self.spans.iter().map(|s| s.content.chars().count()).sum() }
}

fn item(label: &str, value: Vec<Span<'static>>) -> Item {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans = Vec::with_capacity(value.len() + 1);
    if !label.is_empty() { spans.push(Span::styled(format!("{} ", label), dim)); }
    spans.extend(value);
    Item { spans }
}

fn val(s: String) -> Vec<Span<'static>> { vec![Span::raw(s)] }

/// Lay `items` out on lines indented by `INDENT`, `GAP` apart, starting a
/// new line only where the next pair wouldn't fit in `term_w`.
fn flow(items: Vec<Item>, term_w: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for it in items {
        let w = it.width();
        if cur_w > 0 && INDENT + cur_w + GAP + w > term_w {
            lines.push(Line::from(std::mem::take(&mut cur)));
            cur_w = 0;
        }
        cur.push(Span::raw(if cur_w == 0 { " ".repeat(INDENT) } else { " ".repeat(GAP) }));
        cur_w += if cur_w == 0 { 0 } else { GAP } + w;
        cur.extend(it.spans);
    }
    if !cur.is_empty() { lines.push(Line::from(cur)); }
    lines
}

/// The summary lines. `ran` is the formatted run time; `term_w` bounds line 1
/// (exec commands are truncated to fit) and decides where line 2 wraps.
pub fn single_exit_lines(state: &TargetState, mode_label: &str, args: &Args, ran: &str, term_w: usize) -> Vec<Line<'static>> {
    let theme = &args.theme;
    let dim   = Style::default().add_modifier(Modifier::DIM);
    let hidden = |b: BaseStat| args.hidden_base_stats.contains(&b);
    let mut lines = Vec::with_capacity(3);

    // ── Line 1: wordmark + the single view's header line + run time ──────────
    // The wordmark in the theme's dark muted tint: it marks whose output this
    // is without competing with the target name beside it.
    let mut head: Vec<Span<'static>> = vec![
        Span::styled("vlat:", Style::default().fg(theme.c(theme.xaxis_dim))),
        Span::raw(" "),
    ];
    {
        // A mid-flash hostname or an in-flight re-resolve would freeze into the
        // printed line; the summary wants the plain resting look.
        let mut s = state.clone();
        s.threshold_flash = 0;
        s.resolving = false;
        let show_badge = super::mode_badge_visible(args.column_vis.mode, &[mode_label.to_string()]);
        let header = build_header_line(
            &s, args, false, mode_label, "", 0, None, show_badge, mode_label.len(),
            term_w.saturating_sub(INDENT) as u16, None, false,
        );
        // Drop the sort-arrow slot it leads with (the wordmark's space already
        // separates) and the `↻N` IP-change counter, which line 3 says in words.
        // It's the ip_change-colored span opening with the counter's glyph.
        let arrow = if args.ascii { "~" } else { "\u{21bb}" };
        let is_counter = |sp: &Span| sp.style.fg == Some(theme.ip_change) && sp.content.trim_start().starts_with(arrow);
        head.extend(header.spans.into_iter().skip(1)
            .filter(|sp| !is_counter(sp))
            .map(|sp| Span::styled(sp.content.into_owned(), sp.style)));
    }
    head.push(Span::styled("  ran ", dim));
    head.push(Span::raw(ran.to_string()));
    lines.push(Line::from(head));

    // ── Line 2: the stats line, over the whole run ────────────────────────────
    let sent     = state.total_sent;
    let lost     = state.drops as u64;
    let received = sent.saturating_sub(lost);
    let replied  = received > 0 && state.has_life_rtts();
    let mut items = Vec::new();
    if replied {
        if !hidden(BaseStat::Avg) { items.push(item("avg", val(fmt_rtt(state.avg_latency())))); }
        if !hidden(BaseStat::Range) {
            items.push(item("range", val(format!("{}-{}", fmt_rtt_nodec(state.life_min()), fmt_rtt_nodec(state.life_max())))));
        }
        if !hidden(BaseStat::Jitter) {
            let j = state.avg_jitter();
            items.push(item("jitter", if j > 0.0 { val(fmt_rtt(j)) } else { vec![Span::styled("\u{2014}", dim)] }));
        }
    } else if sent > 0 {
        items.push(item("", vec![Span::styled("no replies", dim)]));
    }
    if !hidden(BaseStat::Drops) && sent > 0 {
        let num_style = if lost > 0 {
            Style::default().fg(theme.drop_num_color(lost as f64 / sent as f64)).add_modifier(Modifier::BOLD)
        } else { dim };
        let sep = if args.ascii { "/" } else { "\u{00b7}" };
        let mut v = vec![
            Span::styled(thousands(lost), num_style),
            Span::styled(sep, dim),
            Span::styled(thousands(sent), dim),
        ];
        if lost > 0 { v.push(Span::styled(format!(" ({})", pct(lost, sent)), dim)); }
        items.push(item("loss", v));
    }
    if replied {
        for stat in &args.extra_stats {
            if *stat == ExtraStat::Quality {
                // Styled like the single view's quality cell.
                let cell = run_quality_cell(state, args.warn_rtt);
                let style = match cell {
                    QualityCell::NoData => dim,
                    QualityCell::NoReply => Style::default().fg(theme.drop_color),
                    QualityCell::Good => Style::default().fg(theme.rtt_good).add_modifier(Modifier::DIM),
                    QualityCell::Word(Quality::Slow | Quality::Degraded, _) => Style::default().fg(theme.rtt_warn),
                    QualityCell::Word(_, _) => Style::default().fg(theme.ip_change),
                };
                items.push(item("quality", vec![Span::styled(cell.text(args.ascii), style)]));
                continue;
            }
            let it = match stat {
                ExtraStat::Mtr => Some(("mtr", state.life_mtr().map_or_else(|| "\u{221e}".to_string(), fmt_rtt))),
                ExtraStat::Std => Some(("std", fmt_rtt(state.life_stddev()))),
                ExtraStat::P01 => Some(("p01", fmt_rtt(state.life_p01()))),
                ExtraStat::P10 => Some(("p10", fmt_rtt(state.life_p10()))),
                ExtraStat::P50 => Some(("p50", fmt_rtt(state.life_median()))),
                ExtraStat::P95 => Some(("p95", fmt_rtt(state.life_p95()))),
                ExtraStat::P99 => Some(("p99", fmt_rtt(state.life_p99()))),
                ExtraStat::Cv  => Some(("cv", fmt_cv(state.life_cv()))),
                ExtraStat::Jitpct => Some(("jitpct", fmt_jitpct(state.life_jitter_pct()))),
                ExtraStat::Srtt if state.srtt > 0.0 => Some(("srtt", fmt_rtt(state.srtt))),
                // Widgets, the live status text and ages: nothing to say after the run.
                _ => None,
            };
            if let Some((label, v)) = it { items.push(item(label, val(v))); }
        }
    }
    lines.extend(flow(items, term_w));

    // ── Line 3: the counts ────────────────────────────────────────────────────
    let recv_style = if sent > 0 && received == 0 {
        Style::default().fg(theme.drop_color).add_modifier(Modifier::BOLD)
    } else { Style::default() };
    let mut counts = vec![
        item("sent", val(thousands(sent))),
        item("received", vec![Span::styled(thousands(received), recv_style)]),
    ];
    if state.dups > 0 {
        let dup_style = Style::default().fg(theme.rtt_warn).add_modifier(Modifier::BOLD);
        counts.push(item("dup", vec![
            Span::styled(thousands(state.dups as u64), dup_style),
            Span::styled(format!(" ({})", pct(state.dups as u64, sent)), dim),
        ]));
    }
    if state.ip_changes > 0 {
        counts.push(item("address changes", vec![
            Span::styled(thousands(state.ip_changes as u64), Style::default().fg(theme.ip_change)),
        ]));
    }
    lines.extend(flow(counts, term_w));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(extra: &[&str]) -> Args {
        let mut argv = vec!["vlat", "example.org"];
        argv.extend_from_slice(extra);
        let mut a = Args::parse_from(argv);
        a.theme = a.theme_name.to_theme();
        let (stats, vis) = crate::cli::resolve_columns(&a.extra_stats).unwrap();
        a.extra_stats = stats;
        a.column_vis = vis;
        a
    }

    /// 40 probes, one drop, 10-14ms replies.
    fn state() -> TargetState {
        let mut s = TargetState::new("example.org".to_string());
        s.current_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34)));
        for i in 0..40usize {
            s.record_sent(i);
            s.record_result(i, if i == 7 { Err(()) } else { Ok(10.0 + (i % 5) as f64) }, 0, false);
        }
        s
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn three_lines_shaped_like_the_single_view() {
        let t = text(&single_exit_lines(&state(), "icmp", &args(&[]), "40s", 120));
        assert_eq!(t, vec![
            "vlat: example.org  93.184.216.34  ran 40s".to_string(),
            "      avg 12.0  range 10-14  jitter 1.58  loss 1\u{00b7}40 (2.5%)  quality good  p95 14.0".to_string(),
            "      sent 40  received 39".to_string(),
        ]);
    }

    #[test]
    fn mode_badge_and_ascii_follow_the_single_view() {
        let t = text(&single_exit_lines(&state(), "tcp", &args(&["--ascii"]), "40s", 120));
        assert!(t[0].starts_with("vlat: tcp > example.org"), "{t:?}");
        assert!(t[1].contains("loss 1/40"), "{t:?}");
    }

    #[test]
    fn enabled_columns_are_added_and_non_numeric_ones_never_are() {
        let a = args(&["--columns", "default,mtr,p99,status,seen,lost,recent,bar"]);
        let t = text(&single_exit_lines(&state(), "icmp", &a, "40s", 200));
        assert_eq!(t.len(), 3, "{t:?}");
        for want in ["mtr ", "p95 ", "p99 ", "quality "] { assert!(t[1].contains(want), "{want:?} missing: {t:?}"); }
        for never in ["status", "seen", "lost ", "recent", "bar"] {
            assert!(!t[1].contains(never), "{never:?} leaked: {t:?}");
        }
    }

    #[test]
    fn hidden_base_stats_stay_hidden() {
        let mut a = args(&[]);
        a.hidden_base_stats = vec![BaseStat::Avg, BaseStat::Jitter];
        let t = text(&single_exit_lines(&state(), "icmp", &a, "40s", 120));
        assert!(!t[1].contains("avg") && !t[1].contains("jitter"), "{t:?}");
        assert!(t[1].trim_start().starts_with("range"), "{t:?}");
    }

    #[test]
    fn never_replied() {
        let mut s = TargetState::new("example.org".to_string());
        for i in 0..12usize { s.record_sent(i); s.record_result(i, Err(()), 0, false); }
        let t = text(&single_exit_lines(&s, "icmp", &args(&[]), "12s", 120));
        assert_eq!(t[1], "      no replies  loss 12\u{00b7}12 (100%)");
        assert_eq!(t[2], "      sent 12  received 0");
    }

    #[test]
    fn nothing_sent() {
        let s = TargetState::new("example.org".to_string());
        let t = text(&single_exit_lines(&s, "icmp", &args(&[]), "0s", 120));
        assert_eq!(t.len(), 2, "no stats line when nothing was sent: {t:?}");
        assert_eq!(t[1], "      sent 0  received 0");
    }

    #[test]
    fn dup_only_when_there_were_some() {
        let mut s = state();
        assert!(!text(&single_exit_lines(&s, "icmp", &args(&[]), "40s", 120))[2].contains("dup"));
        s.dups = 2;
        assert_eq!(text(&single_exit_lines(&s, "icmp", &args(&[]), "40s", 120))[2], "      sent 40  received 39  dup 2 (5.0%)");
    }

    #[test]
    fn address_changes_only_when_there_were_some() {
        let mut s = state();
        assert!(!text(&single_exit_lines(&s, "icmp", &args(&[]), "40s", 120))[2].contains("address"));
        s.ip_changes = 1;
        let t = text(&single_exit_lines(&s, "icmp", &args(&[]), "40s", 120));
        assert_eq!(t[2], "      sent 40  received 39  address changes 1");
        // Said once: line 1 drops the header's counter, even where the live view
        // would show it (2+ changes, or forced on with --columns resolve).
        s.ip_changes = 3;
        for extra in [&[][..], &["--columns", "default,resolve"][..]] {
            let t = text(&single_exit_lines(&s, "icmp", &args(extra), "40s", 120));
            assert_eq!(t[0], "vlat: example.org  93.184.216.34  ran 40s", "{extra:?}");
            assert!(t[2].ends_with("address changes 3"), "{t:?}");
        }
    }

    #[test]
    fn exec_target_shows_the_command() {
        let mut s = state();
        s.exec_cmd = "curl -s https://example.org/health".to_string();
        s.custom_label = true;
        s.label = "health".to_string();
        let t = text(&single_exit_lines(&s, "exec", &args(&[]), "40s", 120));
        assert!(t[0].contains("health  curl -s https://example.org/health"), "{t:?}");
    }

    #[test]
    fn stats_wrap_between_pairs_only_when_too_narrow() {
        let a = args(&["--columns", "all"]);
        let t = text(&single_exit_lines(&state(), "icmp", &a, "40s", 50));
        assert!(t.len() > 3, "{t:?}");
        for l in &t[1..] {
            assert!(l.starts_with("      ") && !l.starts_with("       "), "continuations line up: {l:?}");
            assert!(l.chars().count() <= 50, "{l:?}");
        }
    }

    /// A run that replied with each of `rtts` in turn.
    fn run_of(rtts: impl IntoIterator<Item = f64>) -> TargetState {
        let mut s = TargetState::new("example.org".to_string());
        for (i, r) in rtts.into_iter().enumerate() { s.record_sent(i); s.record_result(i, Ok(r), 0, false); }
        s
    }

    fn quality_of(s: &TargetState, extra: &[&str]) -> String {
        let t = text(&single_exit_lines(s, "icmp", &args(extra), "1h", 200));
        t[1].split("quality ").nth(1).map(|r| r.split("  ").next().unwrap().to_string()).unwrap_or_default()
    }

    #[test]
    fn quality_judges_the_whole_run() {
        // Steady 20ms.
        assert_eq!(quality_of(&run_of(std::iter::repeat_n(20.0, 60)), &[]), "good");
        // Alternating 10/30ms: median jitter 20ms on a 20ms median - jittery.
        assert_eq!(quality_of(&run_of((0..60).map(|i| if i % 2 == 0 { 10.0 } else { 30.0 })), &[]), "jittery");
        // Median over --warn-rtt: slow (first in the order, like the live grade).
        assert_eq!(quality_of(&run_of(std::iter::repeat_n(80.0, 60)), &["--warn-rtt", "50"]), "slow");
        // A noisy start long ago still counts: the live grade (last 5 minutes)
        // would have forgotten it, the run's grade doesn't.
        let mut rtts: Vec<f64> = (0..200).map(|i| if i % 2 == 0 { 10.0 } else { 30.0 }).collect();
        rtts.extend(std::iter::repeat_n(20.0, 150));
        assert_eq!(quality_of(&run_of(rtts), &[]), "jittery");
        // Too short to judge.
        assert_eq!(quality_of(&run_of([20.0, 21.0, 19.0]), &[]), "~");
    }

    #[test]
    fn quality_follows_the_column_setting() {
        let s = run_of(std::iter::repeat_n(20.0, 60));
        let t = text(&single_exit_lines(&s, "icmp", &args(&["--columns", "none,p95"]), "1m", 200));
        assert!(!t[1].contains("quality"), "{t:?}");
    }

    #[test]
    fn wordmark_is_one_dark_theme_color() {
        for name in ["default", "nord", "phosphor"] {
            let mut a = args(&["--theme", name]);
            a.theme = a.theme_name.to_theme();
            let lines = single_exit_lines(&state(), "icmp", &a, "40s", 120);
            let mark = &lines[0].spans[0];
            assert_eq!(mark.content, "vlat:");
            assert_eq!(mark.style.fg, Some(a.theme.c(a.theme.xaxis_dim)), "{name}");
        }
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(8_040), "8,040");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
