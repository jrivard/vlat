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

//! No-data targets in the plot views (fullscreen graph, worm, bubble).
//!
//! A target with nothing to plot - every probe in the window dropped, or its
//! name never resolved - is left out of the plot entirely. While the
//! per-target stats rows are shown they already say it's down; while `i`
//! hides them, a one-row strip at the bottom of the plot names the no-data
//! targets instead: ` ✕ office-vpn   ✕ branch-relay   no data`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use crate::cli::Args;
use crate::state::TargetState;

/// True when a target has nothing to plot: its window holds drops but no
/// replies, or its name failed to resolve (so it is never probed). A target
/// still waiting for its first result doesn't count.
pub fn is_no_data(s: &TargetState) -> bool {
    s.resolve_error.is_some() || (!s.waiting && s.window.is_empty() && s.win_drops > 0)
}

/// Whether a target is left out of the plot views: it has no data, or it is
/// still waiting for its first result (so there's nothing to draw yet).
pub fn hidden_from_plot(s: &TargetState) -> bool {
    s.waiting || is_no_data(s)
}

/// Whether the plot views reserve their bottom row for the no-data strip:
/// only while the stats rows are hidden and some target has no data.
pub fn no_data_row_shown(states: &[TargetState], show_headers: bool) -> bool {
    !show_headers && states.iter().any(is_no_data)
}

/// Blend toward the drop color for the strip's ✕ marks: a slow triangle over
/// 8 ticks (0 → 0.75 → 0), so it reads as an alert without flashing.
fn pulse_t(tick: u64) -> f64 {
    let step: u8 = match tick % 8 {
        0 | 7 => 0,
        1 | 6 => 1,
        2 | 5 => 2,
        _     => 3,
    };
    step as f64 / 3.0 * 0.75
}

const SEP: &str = "   ";
const SUFFIX: &str = "no data";

/// Lays out the strip's items within `budget` columns: (slot, name) pairs in
/// display order, plus how many targets didn't fit. Names are shortened first
/// (see `labels::condense_for_width`); targets that still don't fit are
/// dropped from the end and counted in a trailing "+N more".
fn fit_items(names: &[(usize, String)], budget: usize, condense: bool) -> (Vec<(usize, String)>, usize) {
    let mark_w = 2; // "✕ "
    let raw: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
    let shortened = if condense {
        // Leave room for the trailing "no data".
        let room = budget.saturating_sub(SUFFIX.chars().count());
        super::labels::condense_for_width(&raw, room, mark_w + SEP.chars().count())
    } else {
        raw.iter().map(|s| s.to_string()).collect()
    };
    let item_w = |n: &str| mark_w + n.chars().count();
    let mut used = 0usize;
    let mut out: Vec<(usize, String)> = Vec::new();
    for (k, ((slot, _), name)) in names.iter().zip(shortened).enumerate() {
        let left = names.len() - k - 1;
        let more_w = if left > 0 { SEP.chars().count() + format!("+{left} more").chars().count() } else { 0 };
        let w = if out.is_empty() { 0 } else { SEP.chars().count() } + item_w(&name);
        if used + w + more_w > budget { break; }
        used += w;
        out.push((*slot, name));
    }
    let hidden = names.len() - out.len();
    (out, hidden)
}

/// Draws the no-data strip into `row` (one line): each no-data target as a
/// pulsing ✕ and its name in the target color, in display order, then a dim
/// "no data". `avoid` is a rect already used on that line (the window
/// label); the strip takes the free side of the row next to it.
pub fn render_no_data_row(
    buf:        &mut Buffer,
    row:        Rect,
    states:     &[TargetState],
    sort_order: &[usize],
    args:       &Args,
    tick:       u64,
    avoid:      Option<Rect>,
) {
    if row.width == 0 || row.height == 0 { return; }
    // Free span on this row, leaving a 2-column gap beside `avoid`.
    let (x0, x1) = match avoid.filter(|a| a.y == row.y && a.width > 0) {
        Some(a) if a.x >= row.x + row.width / 2 => (row.x, a.x.saturating_sub(2).max(row.x)),
        Some(a) => ((a.x + a.width + 2).min(row.x + row.width), row.x + row.width),
        None    => (row.x, row.x + row.width),
    };
    let x0 = (x0 + 1).min(x1); // one column of margin from the edge/gap
    let budget = (x1 - x0) as usize;

    let names: Vec<(usize, String)> = sort_order.iter().copied()
        .filter(|&slot| slot < states.len() && is_no_data(&states[slot]))
        .map(|slot| (slot, super::labels::display_label(&states[slot].label).to_string()))
        .collect();
    if names.is_empty() { return; }
    let (items, hidden) = fit_items(&names, budget, !args.no_condense_labels);

    let theme = &args.theme;
    let drop_rgb = match theme.drop_color { Color::Rgb(r, g, b) => (r, g, b), _ => (255, 60, 60) };
    let t = pulse_t(tick);
    let dim = Style::default().fg(theme.c(theme.uptime));
    let mark = if args.ascii { "x" } else { "\u{2715}" };

    let mut x = x0;
    let put = |buf: &mut Buffer, x: &mut u16, text: &str, style: Style| {
        for ch in text.chars() {
            if *x >= x1 { return; }
            let cell = &mut buf[(*x, row.y)];
            cell.set_symbol(&ch.to_string());
            cell.set_style(style);
            *x += 1;
        }
    };
    for (k, (slot, name)) in items.iter().enumerate() {
        if k > 0 { put(buf, &mut x, SEP, dim); }
        let (r, g, b) = theme.target_color(*slot);
        let (pr, pg, pb) = super::lerp_rgb(r, g, b, drop_rgb.0, drop_rgb.1, drop_rgb.2, t);
        put(buf, &mut x, mark, Style::default().fg(Color::Rgb(pr, pg, pb)).add_modifier(Modifier::BOLD));
        put(buf, &mut x, " ", dim);
        put(buf, &mut x, name, Style::default().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD));
    }
    if hidden > 0 {
        let more = format!("+{hidden} more");
        if !items.is_empty() { put(buf, &mut x, SEP, dim); }
        put(buf, &mut x, &more, dim);
    }
    let tail = format!("{SEP}{SUFFIX}");
    if (x as usize) + tail.chars().count() <= x1 as usize {
        put(buf, &mut x, &tail, dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<(usize, String)> {
        v.iter().enumerate().map(|(i, s)| (i, s.to_string())).collect()
    }

    #[test]
    fn no_data_needs_drops_and_no_replies_or_a_resolve_error() {
        let mut s = TargetState::new("gw".to_string());
        assert!(!is_no_data(&s), "still waiting for a first result");
        s.waiting = false;
        assert!(!is_no_data(&s), "nothing in the window yet");
        s.win_drops = 3;
        assert!(is_no_data(&s), "only drops in the window");
        let mut r = TargetState::new("bad.example".to_string());
        r.resolve_error = Some("nxdomain".to_string());
        assert!(is_no_data(&r), "unresolvable names never get data");
    }

    #[test]
    fn waiting_targets_are_not_plotted_but_not_listed_as_no_data() {
        let s = TargetState::new("gw".to_string());
        assert!(hidden_from_plot(&s));
        assert!(!no_data_row_shown(std::slice::from_ref(&s), false));
    }

    #[test]
    fn row_only_while_stats_hidden_and_something_is_down() {
        let up = TargetState::new("a".to_string());
        let mut down = TargetState::new("b".to_string());
        down.waiting = false;
        down.win_drops = 1;
        assert!(no_data_row_shown(&[up.clone(), down.clone()], false));
        assert!(!no_data_row_shown(&[up.clone(), down], true));
        assert!(!no_data_row_shown(&[up], false));
    }

    #[test]
    fn fit_keeps_full_names_when_they_fit() {
        let (items, hidden) = fit_items(&names(&["office-vpn", "branch-relay"]), 80, true);
        assert_eq!(items, names(&["office-vpn", "branch-relay"]));
        assert_eq!(hidden, 0);
    }

    #[test]
    fn fit_shortens_then_counts_the_rest() {
        // Shared "-relay" is stripped first.
        let (items, _) = fit_items(&names(&["east-relay", "west-relay"]), 26, true);
        assert_eq!(items, names(&["east", "west"]));
        // Too many for the space: the tail collapses into "+N more".
        let many = names(&["alpha", "bravo", "charlie", "delta", "echo"]);
        let (items, hidden) = fit_items(&many, 24, true);
        assert!(!items.is_empty() && hidden > 0, "{items:?} {hidden}");
        assert_eq!(items.len() + hidden, 5);
    }
}
