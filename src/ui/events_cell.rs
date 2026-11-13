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

//! The `events` column: what happened to this target in the last
//! `EVENTS_EVENT_SECS`, in at most `EVENTS_CELL_W` glyphs - no words, no numbers
//! (the UP column already says up/down, and ticking ages are noise).
//!
//!   glyph = what happened    ✕ drop · ⤴ back after an outage · ⇄ new IP
//!   count = how bad          ✕ one drop event · ✕✕ two or more · ✕✕✕ outage
//!   style = how new          flash → bold → normal → muted (stepped)
//!
//! The style tracks how long each glyph group has shown what it shows now
//! (`TargetState::events_cell_marks`), not the age of the latest event: a
//! glyph pops right after it changes and goes quiet once steady, even while
//! events keep feeding the same state (an ongoing outage, a lasting trend).
//!
//! Severity is the worst of the whole span, so a later blip doesn't downgrade
//! an outage, and the cell only changes when something happens or a fade step
//! is crossed. A recovery replaces the outage's last ✕ (`✕✕⤴`: had an outage,
//! back now). With nothing to report it shows the MTR trend (`↘ → ↗`, bold
//! when steep), or `◷` (`.` in ASCII) while that is still warming up.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::constants::{
    GRADE_OUTAGE_MIN_DROPS, EVENTS_EVENT_SECS, EVENTS_FLASH_SECS, EVENTS_FRESH_SECS,
    EVENTS_IP_EVENT_SECS, EVENTS_RECENT_SECS,
};
// (EVENTS_IP_EVENT_SECS only bounds how long the ⇄ shows; its look fades like the rest.)
use crate::state::{MtrTrend, TargetState};
use crate::time::Instant;

use super::Theme;

/// Widest the cell ever gets, in columns.
pub const EVENTS_CELL_W: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Part {
    /// No probe resolved yet, or the trend is still warming up.
    Empty,
    /// One drop glyph. Every `age` here is how long the glyph's group has
    /// looked the way it does now, not how old the underlying event is.
    Drop { age: f64 },
    Recovered { age: f64 },
    IpChanged { age: f64 },
    Trend { trend: MtrTrend, age: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventsCell {
    pub parts: Vec<Part>,
    ascii: bool,
}

// Glyph groups tracked in `events_cell_marks`.
const G_DROPS: usize = 0;
const G_RECOVERED: usize = 1;
const G_IP: usize = 2;
const G_TREND: usize = 3;

fn trend_key(t: MtrTrend) -> u8 {
    match t {
        MtrTrend::WarmingUp => 1, MtrTrend::Flat => 2,
        MtrTrend::GentleUp => 3, MtrTrend::SteepUp => 4,
        MtrTrend::GentleDown => 5, MtrTrend::SteepDown => 6,
        MtrTrend::AllDrops => 7,
    }
}

pub fn events_cell(s: &TargetState, now: Instant, trend: MtrTrend, ascii: bool) -> EventsCell {
    let within = |t: Option<Instant>, span: f64| {
        t.is_some_and(|t| now.saturating_duration_since(t).as_secs_f64() < span)
    };

    if s.history.iter().all(|x| x.is_pending()) {
        return EventsCell { parts: vec![Part::Empty], ascii };
    }

    let in_outage = s.cur_drop_streak >= GRADE_OUTAGE_MIN_DROPS;
    let recovered = !in_outage && within(s.recovered_at, EVENTS_EVENT_SECS);
    let outage    = in_outage || recovered || within(s.last_outage_at, EVENTS_EVENT_SECS);
    let severity: u8 = if outage { 3 }
        else if within(s.last_drop_events[1], EVENTS_EVENT_SECS) { 2 }
        else if within(s.last_drop_at, EVENTS_EVENT_SECS) { 1 }
        else { 0 };
    let ip = within(s.ip_changed_at, EVENTS_IP_EVENT_SECS);
    let quiet = severity == 0 && !ip;

    // Note what each group shows now; restart its clock when that changed.
    // Keys are 0 while a group is hidden, so reappearing counts as a change.
    // Recovery keeps the drop key at 3, so ✕✕✕ → ✕✕⤴ pops only the ⤴.
    let keys = [severity, recovered as u8, ip as u8, if quiet { trend_key(trend) } else { 0 }];
    let mut ages = [0.0f64; 4];
    {
        let mut marks = s.events_cell_marks.borrow_mut();
        for (g, key) in keys.iter().enumerate() {
            let (k, since) = &mut marks[g];
            if *k != *key || since.is_none() { *k = *key; *since = Some(now); }
            ages[g] = since.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        }
    }

    let mut parts = Vec::with_capacity(EVENTS_CELL_W);
    let drop = Part::Drop { age: ages[G_DROPS] };
    if recovered {
        parts.extend([drop; 2]);
        parts.push(Part::Recovered { age: ages[G_RECOVERED] });
    } else {
        parts.extend(std::iter::repeat_n(drop, severity as usize));
    }
    if ip && parts.len() < EVENTS_CELL_W { parts.push(Part::IpChanged { age: ages[G_IP] }); }
    if parts.is_empty() {
        parts.push(match trend {
            MtrTrend::WarmingUp => Part::Empty,
            t => Part::Trend { trend: t, age: ages[G_TREND] },
        });
    }
    EventsCell { parts, ascii }
}

impl Part {
    fn glyph(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (Part::Empty, false) => "\u{25f7}", // ◷ clock face: "please wait"
            (Part::Empty, true)  => ".",
            (Part::Drop { .. }, false) => "\u{2715}",
            (Part::Drop { .. }, true)  => "x",
            (Part::Recovered { .. }, false) => "\u{2934}",
            (Part::Recovered { .. }, true)  => "+",
            (Part::IpChanged { .. }, false) => "\u{21c4}",
            (Part::IpChanged { .. }, true)  => "*",
            (Part::Trend { trend, .. }, _) => match (trend, ascii) {
                (MtrTrend::SteepUp | MtrTrend::GentleUp, false)     => "\u{2198}",
                (MtrTrend::SteepDown | MtrTrend::GentleDown, false) => "\u{2197}",
                (MtrTrend::AllDrops, false)                         => "\u{2715}",
                (MtrTrend::SteepUp | MtrTrend::GentleUp, true)      => "v",
                (MtrTrend::SteepDown | MtrTrend::GentleDown, true)  => "^",
                (MtrTrend::AllDrops, true)                          => "x",
                (MtrTrend::Flat | MtrTrend::WarmingUp, false)       => "\u{2192}",
                (MtrTrend::Flat | MtrTrend::WarmingUp, true)        => "-",
            },
        }
    }

    fn style(self, theme: &Theme) -> Style {
        let dim = Style::default().add_modifier(Modifier::DIM);
        // Stepped fade by time since the glyph last changed: events flash
        // inverse at first; everything settles to a muted, dimmed color.
        let fade = |age: f64, c: Color, flash: bool| {
            if flash && age < EVENTS_FLASH_SECS { Style::default().fg(Color::White).bg(c).add_modifier(Modifier::BOLD) }
            else if age < EVENTS_FRESH_SECS { Style::default().fg(c).add_modifier(Modifier::BOLD) }
            else if age < EVENTS_RECENT_SECS { Style::default().fg(c) }
            else { Style::default().fg(c).add_modifier(Modifier::DIM) }
        };
        match self {
            Part::Empty => dim,
            Part::Drop { age } => fade(age, theme.drop_color, true),
            Part::Recovered { age } => fade(age, theme.rtt_good, true),
            Part::IpChanged { age } => fade(age, theme.ip_change, true),
            Part::Trend { trend, age } => match trend {
                MtrTrend::WarmingUp | MtrTrend::Flat => dim,
                MtrTrend::SteepUp | MtrTrend::GentleUp     => fade(age, theme.rtt_warn, false),
                MtrTrend::SteepDown | MtrTrend::GentleDown => fade(age, theme.rtt_good, false),
                MtrTrend::AllDrops => fade(age, theme.drop_color, false),
            },
        }
    }
}

impl EventsCell {
    pub fn text(&self) -> String {
        self.parts.iter().map(|p| p.glyph(self.ascii)).collect()
    }

    /// One span per glyph, so each fades on its own clock.
    pub fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        self.parts.iter().map(|p| Span::styled(p.glyph(self.ascii), p.style(theme))).collect()
    }

    /// Style of the leading glyph, for single-style cells (cards view).
    pub fn lead_style(&self, theme: &Theme) -> Style {
        self.parts.first().map_or_else(Style::default, |p| p.style(theme))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// One probe per second; '.' = reply, 'x' = drop. Returns the state and
    /// the time one second after the last probe.
    fn run(pattern: &str) -> (TargetState, Instant) {
        let mut s = TargetState::new("t".into());
        let t0 = Instant::now();
        for (i, c) in pattern.chars().enumerate() {
            let at = t0 + Duration::from_secs(i as u64);
            s.record_sent_at(at, i);
            s.record_result_at(at, i, if c == '.' { Ok(10.0) } else { Err(()) }, 3_600, false);
        }
        (s, t0 + Duration::from_secs(pattern.len() as u64))
    }

    fn text(pattern: &str, after: u64) -> String {
        let (s, now) = run(pattern);
        events_cell(&s, now + Duration::from_secs(after), MtrTrend::Flat, true).text()
    }

    #[test]
    fn dot_until_anything_resolves_and_while_trend_warms_up() {
        let s = TargetState::new("t".into());
        assert_eq!(events_cell(&s, Instant::now(), MtrTrend::Flat, true).text(), ".");
        let (s, now) = run("....");
        assert_eq!(events_cell(&s, now, MtrTrend::WarmingUp, true).text(), ".");
    }

    #[test]
    fn quiet_shows_the_trend() {
        assert_eq!(text("..........", 0), "-");
    }

    #[test]
    fn one_drop_then_expires() {
        assert_eq!(text(".....x....", 0), "x");
        assert_eq!(text(".....x....", 30), "-");
    }

    #[test]
    fn repeated_drops_are_two() {
        assert_eq!(text("..x...x...", 0), "xx");
        assert_eq!(text("..xx...x..", 0), "xx"); // a short streak is one event
    }

    #[test]
    fn outage_is_three_while_down() {
        assert_eq!(text(".....xxxxx", 0), "xxx");
    }

    #[test]
    fn recovery_replaces_the_last_glyph() {
        assert_eq!(text("..xxxx..", 0), "xx+");
    }

    #[test]
    fn severity_holds_against_a_later_blip() {
        // Outage, recovery, then a lone drop: still reads as the outage story.
        assert_eq!(text("..xxx....x..", 0), "xx+");
    }

    #[test]
    fn down_again_drops_the_recovery_mark() {
        assert_eq!(text("..xxx...xxx", 0), "xxx");
    }

    #[test]
    fn ip_change_fits_beside_drops_and_lingers() {
        let (mut s, now) = run(".....x....");
        s.ip_changed_at = Some(now);
        assert_eq!(events_cell(&s, now, MtrTrend::Flat, true).text(), "x*");
        assert_eq!(events_cell(&s, now + Duration::from_secs(45), MtrTrend::Flat, true).text(), "*");
    }

    fn style_at(s: &TargetState, now: Instant) -> Style {
        events_cell(s, now, MtrTrend::Flat, true).spans(&Theme::default())[0].style
    }

    #[test]
    fn steady_outage_goes_quiet_while_drops_keep_coming() {
        let (s, now) = run(".....xxx");
        assert!(style_at(&s, now).bg.is_some(), "a new outage flashes");
        // Keep dropping for 20s more: same state, so it mutes.
        let (s2, now2) = run(&(".....xxx".to_string() + &"x".repeat(20)));
        *s2.events_cell_marks.borrow_mut() = *s.events_cell_marks.borrow();
        let st = style_at(&s2, now2);
        assert!(st.bg.is_none() && st.add_modifier.contains(Modifier::DIM), "{st:?}");
    }

    #[test]
    fn a_change_pops_again() {
        let (s, now) = run("..x.....");
        style_at(&s, now); // first sight: x
        let later = now + Duration::from_secs(20);
        assert!(style_at(&s, later).add_modifier.contains(Modifier::DIM));
        // A second drop event changes x -> xx: pops.
        let mut s = s;
        s.record_sent_at(later, 99);
        s.record_result_at(later, 99, Err(()), 3_600, false);
        assert!(style_at(&s, later).bg.is_some());
    }

    #[test]
    fn never_wider_than_the_cell() {
        let (mut s, now) = run("..xxxx..");
        s.ip_changed_at = Some(now);
        assert_eq!(events_cell(&s, now, MtrTrend::Flat, true).text().chars().count(), EVENTS_CELL_W);
    }
}
