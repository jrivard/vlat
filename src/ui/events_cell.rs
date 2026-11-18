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
//!   look  = how new          one inverse blink, then steps on a shared beat:
//!                            bold/plain pulse → plain → lighter shape → lightest, dim
//!                            (✕ → × → ˣ · ⤴ → ↑ → ˄ · ⇄ → ↔)
//!
//! The beat is global (`EVENTS_BEAT_SECS`, on one fixed grid), so every row
//! steps in the same frame instead of the table shimmering row by row. The
//! look tracks how long each glyph group has shown what it shows now
//! (`TargetState::events_cell_marks`), not the age of the latest event: a
//! glyph pops right after it changes and goes quiet once steady, even while
//! events keep feeding the same state (an ongoing outage, a lasting trend).
//!
//! Severity is the worst of the whole span, so a later blip doesn't downgrade
//! an outage, and the cell only changes when something happens or a fade step
//! is crossed. A recovery replaces the outage's last ✕ (`✕✕⤴`: had an outage,
//! back now). With nothing to report it shows a steady-state `·`, or the MTR
//! trend arrow (`↘ ↗`, bold when steep) for `EVENTS_EVENT_SECS` after the trend
//! changes. An outage that has outlasted `EVENTS_EVENT_SECS` is steady too: the
//! UP column already says it is down. `◷` (`.` in ASCII) shows only during the
//! same short warm-up as the fullscreen graph (its calibration window).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::constants::{
    GRADE_OUTAGE_MIN_DROPS, EVENTS_BEAT_SECS, EVENTS_EVENT_SECS, EVENTS_FLASH_SECS,
    EVENTS_IP_EVENT_SECS, EVENTS_LIGHT_BEAT, EVENTS_MID_BEAT, EVENTS_PULSE_BEATS,
};
// (EVENTS_IP_EVENT_SECS only bounds how long the ⇄ shows; its look fades like the rest.)
use crate::state::{MtrTrend, TargetState};
use crate::time::Instant;
use std::sync::OnceLock;
use std::time::Duration;

use super::Theme;

/// Widest the cell ever gets, in columns.
pub const EVENTS_CELL_W: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Part {
    /// No probe resolved yet, or the trend is still warming up.
    Empty,
    /// One drop glyph. Every `Clock` here measures how long the glyph's
    /// group has looked the way it does now, not how old the underlying event is.
    Drop(Clock),
    Recovered(Clock),
    IpChanged(Clock),
    Trend { trend: MtrTrend, clock: Clock },
    /// Nothing changing: no recent events, latency flat (or a long outage).
    Steady,
}

/// How long a glyph group has shown what it shows now: seconds (for the
/// blink) and shared-grid beat boundaries crossed (for every later step).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub age: f64,
    pub beats: u64,
}

impl Clock {
    /// A clock that never started: the group has looked this way from the start.
    const SETTLED: Clock = Clock { age: f64::INFINITY, beats: u64::MAX };

    fn since(t: Instant, now: Instant) -> Clock {
        Clock {
            age: now.saturating_duration_since(t).as_secs_f64(),
            beats: beat_index(now).saturating_sub(beat_index(t)),
        }
    }

    fn flashing(self) -> bool { self.age < EVENTS_FLASH_SECS }

    /// Glyph shape step: 0 full, 1 lighter, 2 lightest.
    fn shape(self) -> usize {
        if self.beats < EVENTS_MID_BEAT { 0 } else if self.beats < EVENTS_LIGHT_BEAT { 1 } else { 2 }
    }
}

/// Fixed origin of the beat grid, shared by every target, so all rows step together.
fn beat_anchor() -> Instant {
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    *ANCHOR.get_or_init(|| {
        // Set back a little so instants from just before first use still land on the grid.
        let now = Instant::now();
        now.checked_sub(Duration::from_secs(3_600)).unwrap_or(now)
    })
}

fn beat_index(t: Instant) -> u64 {
    (t.saturating_duration_since(beat_anchor()).as_secs_f64() / EVENTS_BEAT_SECS) as u64
}

/// Whether any event glyph of `s` is in its change blink right now: the UI
/// runs at the fast tick meanwhile, so the blink gets a few crisp frames
/// rather than whatever the 1 Hz tick happens to catch.
pub fn flashing(s: &TargetState, now: Instant) -> bool {
    s.events_cell_marks.borrow().iter().enumerate().any(|(g, (key, since))| {
        g != G_TREND && *key != 0
            && since.is_some_and(|t| now.saturating_duration_since(t).as_secs_f64() < EVENTS_FLASH_SECS)
    })
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

    // Warm-up matches the plot views: until the first result, then the same
    // calibration window the fullscreen graph waits out.
    let calibrating = s.calibrating.is_some_and(|(_, until)| until > now);
    if calibrating || s.history.iter().all(|x| x.is_pending()) {
        return EventsCell { parts: vec![Part::Empty], ascii };
    }

    let in_outage = s.cur_drop_streak >= GRADE_OUTAGE_MIN_DROPS;
    let recovered = !in_outage && within(s.recovered_at, EVENTS_EVENT_SECS);
    let outage    = recovered || within(s.last_outage_at, EVENTS_EVENT_SECS);
    // An outage older than the event span is the steady state, not news:
    // its ongoing drops don't count as events.
    let steady_outage = in_outage && !outage;
    let severity: u8 = if outage { 3 }
        else if steady_outage { 0 }
        else if within(s.last_drop_events[1], EVENTS_EVENT_SECS) { 2 }
        else if within(s.last_drop_at, EVENTS_EVENT_SECS) { 1 }
        else { 0 };
    let ip = within(s.ip_changed_at, EVENTS_IP_EVENT_SECS);

    // Note what each group shows now; restart its clock when that changed.
    // Keys are 0 while a group is hidden, so reappearing counts as a change.
    // Recovery keeps the drop key at 3, so ✕✕✕ → ✕✕⤴ pops only the ⤴.
    // The trend is tracked even while hidden behind events, so it only counts
    // as changed when the trend itself moves; its first verdict after warming
    // up isn't a change at all (no clock: settled from the start).
    let keys = [severity, recovered as u8, ip as u8, trend_key(trend)];
    let mut clocks = [Clock::SETTLED; 4];
    {
        let mut marks = s.events_cell_marks.borrow_mut();
        for (g, key) in keys.iter().enumerate() {
            let (k, since) = &mut marks[g];
            if g == G_TREND {
                if *k != *key {
                    let first = *k == 0 || *k == trend_key(MtrTrend::WarmingUp);
                    *since = if first { None } else { Some(now) };
                    *k = *key;
                }
            } else if *k != *key || since.is_none() {
                *k = *key; *since = Some(now);
            }
            clocks[g] = since.map_or(Clock::SETTLED, |t| Clock::since(t, now));
        }
    }

    let mut parts = Vec::with_capacity(EVENTS_CELL_W);
    let drop = Part::Drop(clocks[G_DROPS]);
    if recovered {
        parts.extend([drop; 2]);
        parts.push(Part::Recovered(clocks[G_RECOVERED]));
    } else {
        parts.extend(std::iter::repeat_n(drop, severity as usize));
    }
    if ip && parts.len() < EVENTS_CELL_W { parts.push(Part::IpChanged(clocks[G_IP])); }
    if parts.is_empty() {
        // Steady unless the trend changed recently: an arrow is news, a
        // long-standing slope (or a trend still warming up) is not.
        let trend_news = clocks[G_TREND].age < EVENTS_EVENT_SECS;
        parts.push(match trend {
            MtrTrend::GentleUp | MtrTrend::SteepUp | MtrTrend::GentleDown | MtrTrend::SteepDown
                if trend_news && !steady_outage => Part::Trend { trend, clock: clocks[G_TREND] },
            _ => Part::Steady,
        });
    }
    EventsCell { parts, ascii }
}

impl Part {
    fn glyph(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (Part::Empty, false) => "\u{25f7}", // ◷ clock face: "please wait"
            (Part::Empty, true)  => ".",
            (Part::Steady, false) => "\u{00b7}", // ·
            (Part::Steady, true)  => ".",
            // Each event glyph lightens in shape as it ages (see Clock::shape).
            (Part::Drop(c), false) => ["\u{2715}", "\u{00d7}", "\u{02e3}"][c.shape()], // ✕ × ˣ
            (Part::Drop(c), true)  => ["X", "x", "x"][c.shape()],
            (Part::Recovered(c), false) => ["\u{2934}", "\u{2191}", "\u{02c4}"][c.shape()], // ⤴ ↑ ˄
            (Part::Recovered(_), true)  => "+",
            (Part::IpChanged(c), false) => ["\u{21c4}", "\u{2194}", "\u{2194}"][c.shape()], // ⇄ ↔ ↔
            (Part::IpChanged(_), true)  => "*",
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
        // Since the glyph last changed: events blink inverse once, then every
        // glyph pulses bold/plain on the shared beat, holds plain, and dims.
        let fade = |c: Clock, color: Color, flash: bool| {
            if flash && c.flashing() { Style::default().fg(Color::White).bg(color).add_modifier(Modifier::BOLD) }
            else if c.beats < EVENTS_PULSE_BEATS && c.beats.is_multiple_of(2) { Style::default().fg(color).add_modifier(Modifier::BOLD) }
            else if c.beats < EVENTS_LIGHT_BEAT { Style::default().fg(color) }
            else { Style::default().fg(color).add_modifier(Modifier::DIM) }
        };
        match self {
            Part::Empty | Part::Steady => dim,
            Part::Drop(c) => fade(c, theme.drop_color, true),
            Part::Recovered(c) => fade(c, theme.rtt_good, true),
            Part::IpChanged(c) => fade(c, theme.ip_change, true),
            Part::Trend { trend, clock } => match trend {
                MtrTrend::WarmingUp | MtrTrend::Flat => dim,
                MtrTrend::SteepUp | MtrTrend::GentleUp     => fade(clock, theme.rtt_warn, false),
                MtrTrend::SteepDown | MtrTrend::GentleDown => fade(clock, theme.rtt_good, false),
                MtrTrend::AllDrops => fade(clock, theme.drop_color, false),
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
    fn clock_only_until_the_first_result_and_through_calibration() {
        let s = TargetState::new("t".into());
        assert_eq!(events_cell(&s, Instant::now(), MtrTrend::Flat, false).text(), "\u{25f7}");
        let (mut s, now) = run("....");
        s.calibrating = Some((now, now + Duration::from_secs(5)));
        assert_eq!(events_cell(&s, now, MtrTrend::WarmingUp, false).text(), "\u{25f7}");
        // Past calibration, a trend still warming up is just steady.
        assert_eq!(events_cell(&s, now + Duration::from_secs(6), MtrTrend::WarmingUp, false).text(), "\u{00b7}");
    }

    #[test]
    fn quiet_and_flat_is_a_dot() {
        assert_eq!(text("..........", 0), ".");
    }

    #[test]
    fn trend_arrow_only_after_a_change() {
        let (s, now) = run("..........");
        // First verdict after warm-up is not a change.
        assert_eq!(events_cell(&s, now, MtrTrend::WarmingUp, true).text(), ".");
        assert_eq!(events_cell(&s, now, MtrTrend::GentleUp, true).text(), ".");
        // Flat -> rising is: arrow for the event span, then a dot again.
        assert_eq!(events_cell(&s, now, MtrTrend::Flat, true).text(), ".");
        assert_eq!(events_cell(&s, now, MtrTrend::GentleUp, true).text(), "v");
        let later = now + Duration::from_secs(EVENTS_EVENT_SECS as u64 + 1);
        assert_eq!(events_cell(&s, later, MtrTrend::GentleUp, true).text(), ".");
    }

    #[test]
    fn long_outage_settles_to_a_dot() {
        let down = ".....".to_string() + &"x".repeat(40);
        let (s, now) = run(&down);
        assert_eq!(events_cell(&s, now, MtrTrend::AllDrops, true).text(), ".");
        // ...and coming back is news again.
        let (s, now) = run(&(down + ".."));
        assert_eq!(events_cell(&s, now, MtrTrend::Flat, true).text(), "XX+");
    }

    #[test]
    fn one_drop_then_expires() {
        assert_eq!(text(".....x....", 0), "X");
        assert_eq!(text(".....x....", 30), ".");
    }

    #[test]
    fn repeated_drops_are_two() {
        assert_eq!(text("..x...x...", 0), "XX");
        assert_eq!(text("..xx...x..", 0), "XX"); // a short streak is one event
    }

    #[test]
    fn outage_is_three_while_down() {
        assert_eq!(text(".....xxxxx", 0), "XXX");
    }

    #[test]
    fn recovery_replaces_the_last_glyph() {
        assert_eq!(text("..xxxx..", 0), "XX+");
    }

    #[test]
    fn severity_holds_against_a_later_blip() {
        // Outage, recovery, then a lone drop: still reads as the outage story.
        assert_eq!(text("..xxx....x..", 0), "XX+");
    }

    #[test]
    fn down_again_drops_the_recovery_mark() {
        assert_eq!(text("..xxx...xxx", 0), "XXX");
    }

    #[test]
    fn ip_change_fits_beside_drops_and_lingers() {
        let (mut s, now) = run(".....x....");
        s.ip_changed_at = Some(now);
        assert_eq!(events_cell(&s, now, MtrTrend::Flat, true).text(), "X*");
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

    fn cell_at(s: &TargetState, now: Instant, ascii: bool) -> EventsCell {
        events_cell(s, now, MtrTrend::Flat, ascii)
    }

    #[test]
    fn blinks_once_then_pulses_and_lightens_on_the_beat() {
        let (s, now) = run(".....x....");
        let theme = Theme::default();
        let first = cell_at(&s, now, false);
        assert_eq!(first.text(), "\u{2715}");
        assert!(first.spans(&theme)[0].style.bg.is_some(), "the change blinks");
        assert!(flashing(&s, now));

        // Past the blink: no background, and the app can drop back to the slow tick.
        let after = now + Duration::from_secs_f64(EVENTS_FLASH_SECS + 0.05);
        assert!(cell_at(&s, after, false).spans(&theme)[0].style.bg.is_none());
        assert!(!flashing(&s, after));

        // The first frame at least `n` beat boundaries past the change.
        let at_beats = |n: u64| {
            let mut t = after;
            while Clock::since(now, t).beats < n { t += Duration::from_secs_f64(EVENTS_BEAT_SECS / 8.0); }
            cell_at(&s, t, false)
        };
        let bold = |c: &EventsCell| c.spans(&theme)[0].style.add_modifier.contains(Modifier::BOLD);
        let dimmed = |c: &EventsCell| c.spans(&theme)[0].style.add_modifier.contains(Modifier::DIM);
        // Pulse after the blink: (bold,) plain, bold, plain.
        assert!(!bold(&at_beats(1)));
        assert!(bold(&at_beats(2)));
        assert!(!bold(&at_beats(3)));
        // Holds the full glyph, plain, then lighter shapes.
        let hold = at_beats(EVENTS_PULSE_BEATS);
        assert_eq!(hold.text(), "\u{2715}");
        assert!(!bold(&hold) && !dimmed(&hold));
        assert_eq!(at_beats(EVENTS_MID_BEAT).text(), "\u{00d7}");
        let light = at_beats(EVENTS_LIGHT_BEAT);
        assert_eq!(light.text(), "\u{02e3}");
        assert!(dimmed(&light));
    }

    #[test]
    fn rows_step_on_the_same_beat() {
        // Two targets whose drops were first seen 0.7s apart cross each beat
        // boundary in the same frame.
        let (a, now) = run(".....x....");
        let (b, _) = run(".....x....");
        cell_at(&a, now, true);
        cell_at(&b, now + Duration::from_millis(700), true);
        let mut t = now + Duration::from_millis(700);
        let ma = a.events_cell_marks.borrow()[G_DROPS].1.unwrap();
        let mb = b.events_cell_marks.borrow()[G_DROPS].1.unwrap();
        for _ in 0..40 {
            let prev = t;
            t += Duration::from_millis(250);
            let stepped = |m| Clock::since(m, t).beats != Clock::since(m, prev).beats;
            assert_eq!(stepped(ma), stepped(mb), "rows must step in the same frame");
        }
    }

    #[test]
    fn recovery_on_an_old_outage_reads_light_light_fresh() {
        let (s, now) = run("..xxxx");
        cell_at(&s, now, false); // outage seen
        let mut s = s;
        let later = now + Duration::from_secs_f64(EVENTS_BEAT_SECS * (EVENTS_LIGHT_BEAT + 1) as f64);
        for i in 0..2 {
            s.record_sent_at(later, 100 + i);
            s.record_result_at(later, 100 + i, Ok(10.0), 3_600, false);
        }
        assert_eq!(cell_at(&s, later, false).text(), "\u{02e3}\u{02e3}\u{2934}");
    }
}
