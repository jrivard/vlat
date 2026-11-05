//! The probe-status summary text ("ok · no loss · 31 sent over 30s"): the
//! single-target view's header line and the `status` column in every other view.
//!
//! # The line's format
//!
//! The line has up to three slots. Each slot always answers the same question,
//! whatever the target's state, so a reader never has to work out what a
//! position means:
//!
//! | slot     | question                      | values                                          |
//! |----------|-------------------------------|-------------------------------------------------|
//! | now      | is it answering right now? (and how well) | see below, e.g. `ok`, `flaky`, `down`, `ok, jittery` |
//! | record   | how much has it lost overall? | `no loss [for <T>]`, `N lost [(P%)], last <failure/success> <when>` |
//! | coverage | how much have we watched?     | `N sent over <elapsed>`                                   |
//!
//! - The verdict is the first matching row ("window" = the stats window,
//!   `--window`, 5m in lifetime mode; adjustable at runtime with `w`):
//!
//!   | verdict       | when                                                         |
//!   |---------------|--------------------------------------------------------------|
//!   | `waiting for reply` | nothing has replied or timed out yet                         |
//!   | `unreachable` | probes have timed out, and none has ever replied             |
//!   | `down`        | the latest resolved probe was lost (agrees with the badge)   |
//!   | `flapping`    | 2+ outages (2+ drops in a row each) in the window            |
//!   | `struggling`  | window loss >= 25% (stays until < 20%)                       |
//!   | `lossy`       | window loss >= 5% (stays until < 3%)                         |
//!   | `flaky`       | 2+ separate drop events in the window                        |
//!   | `recovering`  | 1 drop event (a single drop or one outage) in the window     |
//!   | `ok now`      | none in the window, but a failure within [`ALL_CLEAR_SECS`]  |
//!   | `ok`          | never failed, or not for [`ALL_CLEAR_SECS`]                  |
//!
//!   Loss grades need 20+ resolved probes in the window. Thresholds and their
//!   hysteresis live in `constants.rs` (`GRADE_*`); grading happens once per
//!   probe in `TargetState::regrade`, which remembers the previous grade.
//! - After `ok`, `ok now` or `recovering`, at most one quality word may follow,
//!   first match of: `slow` (median >= --warn-rtt), `degraded` (median >= 2x
//!   the target's own hour-scale baseline), `jittery` (median jitter >= half
//!   the median), `spiky` (p99 >= 4x median). E.g. `ok, jittery`. It's left
//!   off worse verdicts, where it would add nothing.
//! - A trend word (`improving` / `worsening`) is added only where direction
//!   changes what you'd do: the loss trend on `flapping`, `struggling` and
//!   `lossy`, and the latency trend after `degraded`. E.g. `lossy, improving`,
//!   `ok, degraded, worsening`. Never on `ok`/`ok now` alone (that's what
//!   quality words are for), `recovering` (already a direction) or `down`.
//!   Trends compare the last minute of the stats window with the rest of it,
//!   and need a clear change (ratio and absolute gap) with hysteresis.
//! - The coverage slot appears only on an `ok` that has never lost a probe.
//!   Anywhere else the earlier slots are the news.
//! - The record slot appears only once the target has replied at least once.
//!   Before that, "waiting for reply" and "unreachable" already say everything.
//! - The record slot ends with the most recent event *opposite* to the verdict:
//!   `last failure` while ok, `last success` while down. That's the one worth
//!   knowing (the latest event of the same kind is always just now).
//! - Every count is lifetime, and every duration has a label ("ago", "over"),
//!   so there are never two bare durations on one line.
//! - Percent loss appears only strictly between 1% and 99%. Below that it's
//!   noise next to the count, and above it "down" already says it.
//! - **Settling:** an age shorter than [`settle_secs`] reads "just now" rather
//!   than "3s ago", so a fresh event doesn't tick through a flickering duration
//!   on every probe. The verdict itself is never held back: it always agrees
//!   with the up/down badge.
//!
//! Deciding *what* to say ([`StatusSummary::from_state`]) is kept apart from
//! *how* it's worded ([`StatusSummary::parts`]). Every wording is pinned by the
//! snapshot test at the bottom of this file; see `snapshots/status_text.txt`.

use ratatui::{style::Style, text::Span};
use crate::constants::STATUS_ALL_CLEAR_SECS as ALL_CLEAR_SECS;
use crate::state::{LossGrade, TargetState};
pub use crate::state::Trend;
use crate::time::Instant;
use super::{fmt_count, fmt_loss, Theme};

/// Human-friendly duration for the probe/uptime summary text: more precision for
/// small values, progressively coarser as the value grows, so a long-running
/// target never reports something silly like "47m 12s". At each unit
/// tier, the smaller sub-unit is dropped once the larger unit's count reaches 4 -
/// by then the sub-unit is noise, not information.
fn format_elapsed(elapsed_secs: f64) -> String {
    // Whole seconds only - a ticking tenth just flickers. Truncated rather than
    // rounded, like a clock: "0s" until a full second has actually passed.
    let secs = elapsed_secs.max(0.0) as u64;
    if secs < 60 { return format!("{}s", secs); }

    let (mins, secs) = (secs / 60, secs % 60);
    if mins < 60 { return if mins < 4 { format!("{}m {}s", mins, secs) } else { format!("{}m", mins) }; }

    let (hours, mins) = (mins / 60, mins % 60);
    if hours < 24 { return if hours < 4 { format!("{}h {}m", hours, mins) } else { format!("{}h", hours) }; }

    let (days, hours) = (hours / 24, hours % 24);
    if days < 4 { format!("{}d {}h", days, hours) } else { format!("{}d", days) }
}

/// How old (seconds) an event must be before its age is shown. Younger ages are
/// held back so they don't flicker in on every probe. Scales with the probe
/// interval (two probes' worth), so a slow poller doesn't show an age for what
/// is, for it, a single probe. Never below 10s, so a fast poller doesn't show an
/// age for something that happened an eyeblink ago.
fn settle_secs(interval_ms: u64) -> f64 {
    (interval_ms as f64 / 1000.0 * 2.0).max(10.0)
}

/// Separator between the segments of the probe-status text. Both forms are 3
/// columns wide, so column widths don't depend on --ascii.
pub fn status_sep(ascii: bool) -> &'static str {
    if ascii { " - " } else { " \u{00b7} " }
}

/// The "now" slot: whether the target is answering right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Now {
    /// Probes are out, but none has replied or timed out yet.
    Waiting,
    /// Probes have timed out and none has ever replied.
    Unreachable,
    /// Replying, and has never lost a probe, or not for [`ALL_CLEAR_SECS`].
    Ok,
    /// Replying, but 2+ outages within the stats window.
    Flapping,
    /// Replying, but heavy loss in the stats window.
    Struggling,
    /// Replying, but sustained loss in the stats window.
    Lossy,
    /// Replying, but 2+ separate drop events within the stats window.
    Flaky,
    /// Replying, with 1 drop event within the stats window.
    Recovering,
    /// Replying, no failure within the stats window, but one within
    /// [`ALL_CLEAR_SECS`].
    OkNow,
    /// The most recent resolved probe was lost, having replied before.
    Down,
}

/// How well it answers, when it does - at most one is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quality { Slow, Degraded, Jittery, Spiky }

impl Quality {
    pub fn word(self) -> &'static str {
        match self {
            Quality::Slow => "slow",
            Quality::Degraded => "degraded",
            Quality::Jittery => "jittery",
            Quality::Spiky => "spiky",
        }
    }
}

/// The target's quality word, if any: the first of slow, degraded, jittery,
/// spiky that its current grade has on. Usable on its own (e.g. as a column).
/// Unlike the status line, it doesn't depend on the verdict - the status line
/// only shows it after `ok`, `ok now` or `recovering`. It reads the grade kept
/// by `TargetState::regrade`, so it's cheap to call every frame.
pub fn probe_quality(state: &TargetState) -> Option<Quality> {
    let q = state.grade.quality;
    [(q.slow, Quality::Slow), (q.degraded, Quality::Degraded), (q.jittery, Quality::Jittery), (q.spiky, Quality::Spiky)]
        .into_iter().find(|&(on, _)| on).map(|(_, w)| w)
}

impl Trend {
    pub fn word(self) -> &'static str {
        match self { Trend::Improving => "improving", Trend::Worsening => "worsening" }
    }
    /// Compact form for a narrow column (matches the mtr trend arrows).
    pub fn arrow(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (Trend::Improving, false) => "\u{2198}", (Trend::Worsening, false) => "\u{2197}",
            (Trend::Improving, true) => "v", (Trend::Worsening, true) => "^",
        }
    }
}

/// Which way the loss rate is clearly moving (last minute vs the rest of the
/// stats window), or `None` if steady / too little data. Usable on its own,
/// e.g. as a column; the status line shows it only on `flapping`,
/// `struggling` and `lossy`.
pub fn probe_loss_trend(state: &TargetState) -> Option<Trend> { state.grade.loss_trend }

/// Which way the median RTT is clearly moving, like [`probe_loss_trend`]. The
/// status line shows it only after the `degraded` quality word.
pub fn probe_latency_trend(state: &TargetState) -> Option<Trend> { state.grade.latency_trend }

/// Which kind of event the record slot's "last ..." refers to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event { Failure, Success }

/// How long ago an event happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum When {
    /// Younger than the settle time.
    JustNow,
    Ago(f64),
}

/// Everything the status line says, decided but not yet worded.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusSummary {
    pub now: Now,
    /// Only after `Ok`, `OkNow` or `Recovering`.
    pub quality: Option<Quality>,
    /// Only on `Flapping`/`Struggling`/`Lossy` (loss trend) or after the
    /// `Degraded` quality word (latency trend).
    pub trend: Option<Trend>,
    /// Lifetime drops.
    pub lost: u64,
    /// Lifetime loss, only when worth showing (strictly between 1% and 99%).
    pub loss_pct: Option<f64>,
    /// The most recent event opposite to the verdict: the last failure while
    /// ok (`None` if there has been none), the last success while down.
    pub last: Option<(Event, When)>,
    pub sent: u64,
    pub watched_secs: f64,
}

impl StatusSummary {
    pub fn from_state(state: &TargetState, now: Instant, interval_ms: u64) -> Self {
        let settle = settle_secs(interval_ms);
        let age = |t: Instant| now.saturating_duration_since(t).as_secs_f64();
        let when = |t: Instant| match age(t) { a if a >= settle => When::Ago(a), _ => When::JustNow };

        let verdict = match state.last_up {
            None if state.drops == 0 => Now::Waiting,
            None => Now::Unreachable,
            Some(_) if state.is_currently_down() => Now::Down,
            Some(_) => match state.last_drop_at {
                None => Now::Ok,
                Some(_) if state.win_outage_times.len() >= crate::constants::GRADE_FLAPPING_OUTAGES => Now::Flapping,
                Some(_) if state.grade.loss == LossGrade::Struggling => Now::Struggling,
                Some(_) if state.grade.loss == LossGrade::Lossy => Now::Lossy,
                Some(_) if state.win_drop_event_times.len() >= 2 => Now::Flaky,
                Some(_) if !state.win_drop_event_times.is_empty() => Now::Recovering,
                Some(t) if age(t) < ALL_CLEAR_SECS as f64 => Now::OkNow,
                Some(_) => Now::Ok,
            },
        };
        let quality = match verdict {
            Now::Ok | Now::OkNow | Now::Recovering => probe_quality(state),
            _ => None,
        };
        let trend = match (verdict, quality) {
            (Now::Flapping | Now::Struggling | Now::Lossy, _) => probe_loss_trend(state),
            (_, Some(Quality::Degraded)) => probe_latency_trend(state),
            _ => None,
        };
        let last = match verdict {
            Now::Waiting | Now::Unreachable => None,
            Now::Down => state.last_up.map(|t| (Event::Success, when(t))),
            _ => state.last_drop_at.map(|t| (Event::Failure, when(t))),
        };
        let pct = state.life_loss_pct();
        StatusSummary {
            now: verdict,
            quality,
            trend,
            lost: state.drops as u64,
            loss_pct: (pct > 1.0 && pct < 99.0).then_some(pct),
            last,
            sent: state.history.len() as u64, // includes probes still in flight
            watched_secs: state.first_sent_at.map(age).unwrap_or(0.0),
        }
    }

    /// The slots, worded: `[now, (record,) (coverage)]`.
    pub fn parts(&self) -> Vec<String> {
        let now = match self.now {
            Now::Waiting => "waiting for reply",
            Now::Unreachable => "unreachable",
            Now::Ok => "ok",
            Now::Flapping => "flapping",
            Now::Struggling => "struggling",
            Now::Lossy => "lossy",
            Now::Flaky => "flaky",
            Now::Recovering => "recovering",
            Now::OkNow => "ok now",
            Now::Down => "down",
        };
        // "lossy, worsening" / "ok, degraded, improving"
        let now = [Some(now), self.quality.map(Quality::word), self.trend.map(Trend::word)]
            .into_iter().flatten().collect::<Vec<_>>().join(", ");
        let mut parts = vec![now];

        if let (Now::Ok, Some((_, When::Ago(a)))) = (self.now, self.last) {
            // Had trouble once, but long enough ago that it's not news.
            parts.push(format!("no loss for {}", format_elapsed(a)));
        } else if !matches!(self.now, Now::Waiting | Now::Unreachable) {
            let mut record = if self.lost == 0 { "no loss".to_string() } else {
                let pct = self.loss_pct.map(|p| format!(" ({})", fmt_loss(p))).unwrap_or_default();
                format!("{} lost{}", fmt_count(self.lost), pct)
            };
            if let Some((event, when)) = self.last {
                let event = match event { Event::Failure => "failure", Event::Success => "success" };
                let when = match when {
                    When::JustNow => "just now".to_string(),
                    When::Ago(a) => format!("{} ago", format_elapsed(a)),
                };
                record = format!("{record}, last {event} {when}");
            }
            parts.push(record);
        }

        // Coverage only on a plain "ok": it's the reassuring "watched this long, no
        // trouble". Anywhere else the earlier slots are the news, and it's clutter.
        if self.now == Now::Ok && self.lost == 0 {
            parts.push(format!("{} sent over {}", fmt_count(self.sent), format_elapsed(self.watched_secs)));
        }
        parts
    }
}

/// Plain-text probe-status summary, segments joined with `status_sep`.
pub fn probe_status_text(state: &TargetState, now: Instant, interval_ms: u64, ascii: bool) -> String {
    StatusSummary::from_state(state, now, interval_ms).parts().join(status_sep(ascii))
}

/// Probe-status summary as spans: segments in `style`, separators in the theme's
/// dim `status_sep` color.
pub fn probe_status_spans(state: &TargetState, now: Instant, interval_ms: u64, ascii: bool, theme: &Theme, style: Style) -> Vec<Span<'static>> {
    let sep_style = Style::default().fg(theme.c(theme.status_sep));
    let mut spans = Vec::with_capacity(5);
    for (i, part) in StatusSummary::from_state(state, now, interval_ms).parts().into_iter().enumerate() {
        if i > 0 { spans.push(Span::styled(status_sep(ascii), sep_style)); }
        spans.push(Span::styled(part, style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use std::time::Duration;

    // --- format_elapsed ---

    #[test]
    fn format_elapsed_seconds_are_whole_and_truncated() {
        assert_eq!(format_elapsed(0.0), "0s");
        assert_eq!(format_elapsed(0.9), "0s"); // no tenths, and no "1s" before a full second
        assert_eq!(format_elapsed(3.4), "3s");
        assert_eq!(format_elapsed(45.6), "45s");
        assert_eq!(format_elapsed(59.9), "59s");
    }

    #[test]
    fn format_elapsed_minutes_under_four_keep_seconds() {
        assert_eq!(format_elapsed(60.0), "1m 0s");
        assert_eq!(format_elapsed(135.0), "2m 15s");
        assert_eq!(format_elapsed(239.0), "3m 59s");
    }

    #[test]
    fn format_elapsed_four_minutes_and_up_drops_seconds() {
        assert_eq!(format_elapsed(240.0), "4m");
        assert_eq!(format_elapsed(299.0), "4m");
        assert_eq!(format_elapsed(3599.0), "59m");
    }

    #[test]
    fn format_elapsed_hours_under_four_keep_minutes() {
        assert_eq!(format_elapsed(3600.0), "1h 0m");
        assert_eq!(format_elapsed(3660.0), "1h 1m");
        assert_eq!(format_elapsed(3600.0 * 3.5), "3h 30m");
    }

    #[test]
    fn format_elapsed_four_hours_and_up_drops_minutes() {
        assert_eq!(format_elapsed(3600.0 * 4.0), "4h");
        assert_eq!(format_elapsed(3600.0 * 23.0), "23h");
    }

    #[test]
    fn format_elapsed_days_under_four_keep_hours() {
        assert_eq!(format_elapsed(86400.0), "1d 0h");
        assert_eq!(format_elapsed(86400.0 + 3600.0 * 5.0), "1d 5h");
    }

    #[test]
    fn format_elapsed_four_days_and_up_drops_hours() {
        assert_eq!(format_elapsed(86400.0 * 4.0), "4d");
        assert_eq!(format_elapsed(86400.0 * 10.0), "10d");
    }

    // --- settle_secs ---

    #[test]
    fn settle_scales_with_interval_but_floors_at_ten_seconds() {
        assert_eq!(settle_secs(1_000), 10.0);   // 1s interval: 2s < 10s floor
        assert_eq!(settle_secs(3_000), 10.0);   // 3s interval: 6s < 10s floor
        assert_eq!(settle_secs(8_000), 16.0);   // 8s interval: 16s > floor
        assert_eq!(settle_secs(30_000), 60.0);  // 30s interval: 60s > floor
    }

    // --- scenarios ---

    /// A target timeline for the summary/snapshot tests.
    struct Scenario {
        name: &'static str,
        /// One char per probe, oldest first: 'x' lost, '?' still in flight, or a
        /// reply: '.' 10ms, 'm' 30ms, 's' 250ms, 'a'/'b' 5ms/25ms (for jitter).
        probes: String,
        /// Time since the first probe was sent.
        watched: f64,
        /// Time since the last reply / last drop (ignored if there was none).
        up_ago: f64,
        drop_ago: f64,
        /// Gap between successive drop events (streaks), going back from the last.
        drop_spacing: f64,
        warn_rtt: Option<f64>,
        /// The target's long-run normal RTT. Defaults to the window median: the
        /// builder records everything at one instant, so the real time-weighted
        /// baseline would never move off the first reply.
        baseline: Option<f64>,
        /// Place every probe one interval apart, ending now, instead of using
        /// `watched`/`up_ago`/`drop_ago`/`drop_spacing`. Needed for trends,
        /// which split the window by time.
        timed: bool,
        /// The stats window (`--window`; 300 is what lifetime mode uses).
        window_secs: f64,
        interval_ms: u64,
    }

    impl Scenario {
        /// Builds the state, then moves its timestamps so `now` sits `watched`
        /// after the first send - recording itself happens all at once.
        fn build(&self) -> (TargetState, Instant) {
            let mut s = TargetState::new("127.0.0.1".to_string());
            s.warn_rtt = self.warn_rtt;
            for (seq, c) in self.probes.chars().enumerate() {
                s.record_sent(seq);
                let rtt = match c { '.' => 10.0, 'm' => 30.0, 's' => 250.0, 'a' => 5.0, 'b' => 25.0, _ => 0.0 };
                match c {
                    'x' => s.record_result(seq, Err(()), 0, false),
                    '?' => {}
                    _ => s.record_result(seq, Ok(rtt), 0, false),
                }
            }
            if self.timed { return self.place_by_position(s); }
            let t0 = s.first_sent_at.unwrap();
            let now = t0 + Duration::from_secs_f64(self.watched);
            if s.last_up.is_some() { s.last_up = Some(now - Duration::from_secs_f64(self.up_ago)); }
            if s.last_drop_at.is_some() { s.last_drop_at = Some(now - Duration::from_secs_f64(self.drop_ago)); }
            // Drop streaks, most recent first, `drop_spacing` apart, pruned to the
            // window the way record_result does.
            let streaks: Vec<usize> = self.probes.split(|c| c != 'x').filter(|r| !r.is_empty()).map(str::len).rev().collect();
            let at = |a: f64| now - Duration::from_secs_f64(a);
            let in_window: Vec<(f64, usize)> = streaks.iter().enumerate()
                .map(|(i, &n)| (self.drop_ago + i as f64 * self.drop_spacing, n))
                .filter(|&(a, _)| a < self.window_secs)
                .collect();
            s.win_drop_times = in_window.iter().flat_map(|&(a, n)| std::iter::repeat_n(at(a), n)).collect();
            s.win_drops = s.win_drop_times.len() as u32;
            s.win_drop_event_times = in_window.iter().map(|&(a, _)| at(a)).collect();
            s.win_outage_times = in_window.iter()
                .filter(|&&(_, n)| n as u32 >= crate::constants::GRADE_OUTAGE_MIN_DROPS)
                .map(|&(a, _)| at(a)).collect();
            let mut rtts: Vec<f64> = s.window.iter().map(|&(_, v)| v).collect();
            rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if !rtts.is_empty() { s.baseline_rtt = self.baseline.unwrap_or(rtts[rtts.len() / 2]); }
            s.regrade_at(now);
            (s, now)
        }

        /// Timed mode: probe i of n was resolved (n-1-i) intervals before now.
        fn place_by_position(&self, mut s: TargetState) -> (TargetState, Instant) {
            let chars: Vec<char> = self.probes.chars().collect();
            let step = self.interval_ms as f64 / 1000.0;
            let n = chars.len();
            let now = s.first_sent_at.unwrap() + Duration::from_secs_f64(n as f64 * step);
            let age = |i: usize| (n - 1 - i) as f64 * step;
            let at = |i: usize| now - Duration::from_secs_f64(age(i));
            let in_win = |i: usize| age(i) < self.window_secs;
            s.first_sent_at = Some(at(0));
            s.last_up = (0..n).rev().find(|&i| !matches!(chars[i], 'x' | '?')).map(at);
            s.last_drop_at = (0..n).rev().find(|&i| chars[i] == 'x').map(at);
            let replies: Vec<usize> = (0..n).filter(|&i| !matches!(chars[i], 'x' | '?')).collect();
            s.window = replies.iter().zip(s.window.iter()).filter(|&(&i, _)| in_win(i)).map(|(&i, &(_, v))| (at(i), v)).collect();
            let drops: Vec<usize> = (0..n).filter(|&i| chars[i] == 'x' && in_win(i)).collect();
            s.win_drop_times = drops.iter().map(|&i| at(i)).collect();
            s.win_drops = drops.len() as u32;
            let streak_pos = |i: usize, k: usize| i >= k && (0..k).all(|j| chars[i - j - 1] == 'x') && (i == k || chars[i - k - 1] != 'x');
            s.win_drop_event_times = drops.iter().filter(|&&i| streak_pos(i, 0)).map(|&i| at(i)).collect();
            let k = crate::constants::GRADE_OUTAGE_MIN_DROPS as usize - 1;
            s.win_outage_times = drops.iter().filter(|&&i| streak_pos(i, k)).map(|&i| at(i)).collect();
            let mut rtts: Vec<f64> = s.window.iter().map(|&(_, v)| v).collect();
            rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if !rtts.is_empty() { s.baseline_rtt = self.baseline.unwrap_or(rtts[rtts.len() / 2]); }
            s.regrade_at(now);
            (s, now)
        }
        fn timed(self) -> Self { Scenario { timed: true, ..self } }
        fn spaced(self, drop_spacing: f64) -> Self { Scenario { drop_spacing, ..self } }
        fn window(self, window_secs: f64) -> Self { Scenario { window_secs, ..self } }
        fn interval(self, interval_ms: u64) -> Self { Scenario { interval_ms, ..self } }
        fn warn(self, ms: f64) -> Self { Scenario { warn_rtt: Some(ms), ..self } }
        fn baseline(self, ms: f64) -> Self { Scenario { baseline: Some(ms), ..self } }
        fn summary(&self) -> StatusSummary {
            let (s, now) = self.build();
            StatusSummary::from_state(&s, now, self.interval_ms)
        }
        fn text(&self) -> String {
            let (s, now) = self.build();
            probe_status_text(&s, now, self.interval_ms, false)
        }
    }

    fn sc(name: &'static str, probes: impl Into<String>, watched: f64, up_ago: f64, drop_ago: f64) -> Scenario {
        Scenario { name, probes: probes.into(), watched, up_ago, drop_ago, drop_spacing: 1.0, warn_rtt: None, baseline: None, timed: false, window_secs: 300.0, interval_ms: 1_000 }
    }

    /// `n` repeats of a probe char, e.g. `rep('.', 39)`.
    fn rep(c: char, n: usize) -> String { std::iter::repeat_n(c, n).collect() }

    /// Every distinct case the line can be in. Add a row here for each new case
    /// you want pinned; the snapshot shows them all side by side.
    fn scenarios() -> Vec<Scenario> {
        vec![
            sc("first probe in flight", "?", 0.4, 0.0, 0.0),
            sc("several in flight", "???", 2.5, 0.0, 0.0),
            sc("first probe timed out", "x", 1.2, 0.0, 1.2),
            sc("never replied", "xxxxx", 5.0, 0.0, 0.0),
            sc("first reply", ".", 0.5, 0.0, 0.0),
            sc("healthy", "..", 135.0, 1.0, 0.0),
            sc("healthy, long run", rep('.', 1_500), 86_400.0 * 2.0 + 3_600.0 * 5.0, 1.0, 0.0),
            sc("one drop, unsettled, back up", "x.", 2.0, 1.0, 1.0),
            sc("one drop, settled", "x.", 31.0, 1.0, 30.0),
            sc("two drops, last 45s ago (flaky)", ".x.x.", 120.0, 1.0, 45.0).spaced(30.0),
            sc("two drops 10m apart (1 in window)", ".x.x.", 900.0, 1.0, 45.0).spaced(600.0),
            sc("flaky, many drops", ".x.x.x.x.x.", 120.0, 1.0, 5.0).spaced(20.0),
            sc("loss 2.5%", "x".to_string() + &rep('.', 39), 40.0, 1.0, 39.0),
            sc("loss 0.5% (hidden)", "x".to_string() + &rep('.', 199), 200.0, 1.0, 199.0),
            sc("just went down (unsettled)", ".x", 2.0, 1.0, 0.0),
            sc("down 30s", ".xxx", 34.0, 30.0, 0.0),
            sc("down, mostly lost (98%)", ".".to_string() + &rep('x', 39), 40.0, 39.0, 0.0),
            sc("down, 99.5% lost (pct hidden)", ".".to_string() + &rep('x', 199), 200.0, 199.0, 0.0),
            sc("down again after recovering", ".x.xxx", 60.0, 20.0, 0.0),
            sc("slow poller, drop 40s ago (unsettled)", "x.", 60.0, 1.0, 40.0).interval(30_000),
            sc("slow poller, drop 90s ago", "x.", 120.0, 1.0, 90.0).interval(30_000),
            sc("slow poller, drop 20m ago", "x.", 1_500.0, 1.0, 1_200.0).interval(30_000),
            sc("recovering, 4m 59s since failure", "x.", 400.0, 1.0, 299.0),
            sc("ok now, 5m since failure", "x.", 400.0, 1.0, 300.0),
            sc("short --window 10s, drop 30s ago", "x.", 60.0, 1.0, 30.0).window(10.0),
            sc("ok now, 59m since failure", ".x.", 7_200.0, 1.0, 3_599.0),
            sc("all clear, 1h since failure", ".x.", 7_200.0, 1.0, 3_600.0),
            sc("all clear, days later", ".x.", 86_400.0 * 5.0, 1.0, 86_400.0 * 4.0),
            // loss grades (need 20+ resolved probes in the window)
            sc("loss 4.9% (just under lossy)", rep('.', 82) + &"..x.".repeat(5), 110.0, 1.0, 1.0).spaced(4.0),
            sc("loss 5.1% (lossy)", rep('.', 78) + &"..x.".repeat(5), 110.0, 1.0, 1.0).spaced(4.0),
            sc("lossy, fallen to 4% (hysteresis holds)", rep('.', 80) + &"...x".repeat(6) + &rep('.', 46), 160.0, 1.0, 47.0).spaced(4.0),
            sc("lossy, fallen to 2.9% (released)", rep('.', 80) + &"...x".repeat(6) + &rep('.', 103), 200.0, 1.0, 104.0).spaced(4.0),
            sc("struggling, 1 in 3 lost", ".x.".repeat(10), 30.0, 1.0, 1.0).spaced(3.0),
            // drop patterns
            sc("one outage, recovered", ".....xxx.....", 60.0, 1.0, 20.0),
            sc("flapping, two outages", "....xxx....xxx....", 120.0, 1.0, 20.0).spaced(40.0),
            // quality words
            sc("slow (median over --warn-rtt 20ms)", rep('m', 20), 20.0, 1.0, 0.0).warn(20.0),
            sc("degraded (3x its own baseline)", rep('m', 40), 60.0, 1.0, 0.0).baseline(10.0),
            sc("jittery", "ab".repeat(15), 30.0, 1.0, 0.0),
            sc("spiky", (rep('.', 24) + "s").repeat(4), 100.0, 1.0, 0.0),
            sc("recovering, jittery", "ab".repeat(15) + "x" + &"ab".repeat(3), 40.0, 1.0, 12.0),
            // trends (timed: one probe per second, ending now; the last 60s vs the rest)
            sc("lossy, worsening", rep('.', 240) + &".x.".repeat(20), 0.0, 0.0, 0.0).timed(),
            sc("lossy, improving", ".........x".repeat(20) + &rep('.', 100), 0.0, 0.0, 0.0).timed(),
            sc("lossy, steady (no trend word)", ".......x.......".repeat(20), 0.0, 0.0, 0.0).timed(),
            sc("struggling, worsening", "....x".repeat(48) + &"x.".repeat(30), 0.0, 0.0, 0.0).timed(),
            sc("degraded, worsening", rep('m', 240) + &rep('s', 60), 0.0, 0.0, 0.0).timed().baseline(10.0),
            sc("degraded, improving", rep('s', 240) + &rep('m', 60), 0.0, 0.0, 0.0).timed().baseline(10.0),
            sc("latency rising, but not degraded (no word)", rep('.', 240) + &rep('m', 60), 0.0, 0.0, 0.0).timed(),
            sc("loss rising, but only recovering (no word)", rep('.', 290) + "x" + &rep('.', 9), 0.0, 0.0, 0.0).timed(),
            sc("flaky hides quality", "ab".repeat(30) + "x" + &"ab".repeat(5) + "x" + &"ab".repeat(5), 90.0, 1.0, 10.0).spaced(10.0),
        ]
    }

    /// Snapshot of every scenario's text, in `snapshots/status_text.txt`. To
    /// change the wording: edit `parts`, run with `UPDATE_SNAPSHOTS=1`, then
    /// review the result with `git diff`.
    #[test]
    fn status_text_snapshot() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/snapshots/status_text.txt");
        let actual: String = scenarios().iter()
            .map(|s| format!("{:<46} | {}\n", s.name, s.text()))
            .collect();
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(expected == actual,
            "status text snapshot differs; rerun with UPDATE_SNAPSHOTS=1 and review `git diff {}`\n--- expected\n{expected}--- actual\n{actual}",
            path.display());
    }

    // --- decisions (independent of wording) ---

    fn find(name: &str) -> StatusSummary {
        scenarios().into_iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no scenario {name:?}")).summary()
    }

    #[test]
    fn verdict_tracks_the_latest_resolved_probe() {
        assert_eq!(find("several in flight").now, Now::Waiting);
        assert_eq!(find("first probe timed out").now, Now::Unreachable);
        assert_eq!(find("first reply").now, Now::Ok);
        assert_eq!(find("healthy").now, Now::Ok);
        assert_eq!(find("one drop, unsettled, back up").now, Now::Recovering);
        assert_eq!(find("two drops, last 45s ago (flaky)").now, Now::Flaky);
        assert_eq!(find("flaky, many drops").now, Now::Flaky);
        assert_eq!(find("two drops 10m apart (1 in window)").now, Now::Recovering, "only one is in the 5m window");
        assert_eq!(find("recovering, 4m 59s since failure").now, Now::Recovering);
        assert_eq!(find("ok now, 5m since failure").now, Now::OkNow, "out of the stats window");
        assert_eq!(find("short --window 10s, drop 30s ago").now, Now::OkNow, "window follows --window");
        assert_eq!(find("slow poller, drop 20m ago").now, Now::OkNow);
        assert_eq!(find("ok now, 59m since failure").now, Now::OkNow);
        assert_eq!(find("all clear, 1h since failure").now, Now::Ok, "an old enough failure isn't news");
        assert_eq!(find("loss 4.9% (just under lossy)").now, Now::Flaky);
        assert_eq!(find("loss 5.1% (lossy)").now, Now::Lossy);
        assert_eq!(find("lossy, fallen to 4% (hysteresis holds)").now, Now::Lossy);
        assert_eq!(find("lossy, fallen to 2.9% (released)").now, Now::Flaky);
        assert_eq!(find("struggling, 1 in 3 lost").now, Now::Struggling);
        assert_eq!(find("one outage, recovered").now, Now::Recovering, "an outage is one event, not flaky");
        assert_eq!(find("flapping, two outages").now, Now::Flapping);
        // Down shows at once, agreeing with the badge.
        assert_eq!(find("just went down (unsettled)").now, Now::Down);
        assert_eq!(find("down 30s").now, Now::Down);
    }

    #[test]
    fn quality_word_only_after_ok_ok_now_or_recovering() {
        assert_eq!(find("healthy").quality, None);
        assert_eq!(find("slow (median over --warn-rtt 20ms)").quality, Some(Quality::Slow));
        assert_eq!(find("degraded (3x its own baseline)").quality, Some(Quality::Degraded));
        assert_eq!(find("jittery").quality, Some(Quality::Jittery));
        assert_eq!(find("spiky").quality, Some(Quality::Spiky));
        assert_eq!(find("recovering, jittery").quality, Some(Quality::Jittery));
        assert_eq!(find("flaky hides quality").quality, None);
    }

    #[test]
    fn trend_word_only_on_lossy_struggling_flapping_or_degraded() {
        assert_eq!(find("lossy, worsening").trend, Some(Trend::Worsening));
        assert_eq!(find("lossy, improving").trend, Some(Trend::Improving));
        assert_eq!(find("lossy, steady (no trend word)").trend, None);
        assert_eq!(find("struggling, worsening").trend, Some(Trend::Worsening));
        assert_eq!(find("degraded, worsening").trend, Some(Trend::Worsening));
        assert_eq!(find("degraded, improving").trend, Some(Trend::Improving));
        for sc in scenarios() {
            let sum = sc.summary();
            if sum.trend.is_some() {
                assert!(matches!(sum.now, Now::Flapping | Now::Struggling | Now::Lossy) || sum.quality == Some(Quality::Degraded),
                    "{}: trend word on {:?}/{:?}", sc.name, sum.now, sum.quality);
            }
        }
    }

    #[test]
    fn trends_stand_alone_where_the_line_hides_them() {
        let (s, _) = scenarios().into_iter().find(|s| s.name == "latency rising, but not degraded (no word)").unwrap().build();
        assert_eq!(probe_latency_trend(&s), Some(Trend::Worsening));
        assert_eq!(Trend::Worsening.word(), "worsening");
    }

    #[test]
    fn probe_quality_stands_alone_and_ignores_the_verdict() {
        // Same answer as the status line wherever the line shows it...
        for sc in scenarios() {
            let (s, now) = sc.build();
            let line = StatusSummary::from_state(&s, now, sc.interval_ms).quality;
            if line.is_some() { assert_eq!(probe_quality(&s), line, "{}", sc.name); }
        }
        // ...and still available where the line hides it.
        let (s, _) = scenarios().into_iter().find(|s| s.name == "flaky hides quality").unwrap().build();
        assert_eq!(probe_quality(&s), Some(Quality::Jittery));
        assert_eq!(Quality::Jittery.word(), "jittery");
    }

    #[test]
    fn last_event_is_the_opposite_of_the_verdict_and_settles() {
        use Event::*;
        assert_eq!(find("several in flight").last, None);
        assert_eq!(find("never replied").last, None);
        assert_eq!(find("healthy").last, None, "ok and never failed: nothing to report");
        assert_eq!(find("one drop, unsettled, back up").last, Some((Failure, When::JustNow)));
        assert_eq!(find("one drop, settled").last, Some((Failure, When::Ago(30.0))));
        assert_eq!(find("slow poller, drop 40s ago (unsettled)").last, Some((Failure, When::JustNow)));
        assert_eq!(find("slow poller, drop 90s ago").last, Some((Failure, When::Ago(90.0))));
        assert_eq!(find("just went down (unsettled)").last, Some((Success, When::JustNow)));
        assert_eq!(find("down 30s").last, Some((Success, When::Ago(30.0))));
    }

    #[test]
    fn counts_are_lifetime() {
        let s = find("down again after recovering");
        assert_eq!((s.lost, s.sent), (4, 6), "lost counts the earlier, recovered drop too");
    }

    #[test]
    fn loss_pct_only_strictly_between_one_and_ninety_nine() {
        assert!(find("loss 0.5% (hidden)").loss_pct.is_none());
        assert!(find("loss 2.5%").loss_pct.is_some());
        assert!(find("down, mostly lost (98%)").loss_pct.is_some());
        assert!(find("down, 99.5% lost (pct hidden)").loss_pct.is_none());
    }

    // --- rendering details ---

    #[test]
    fn probe_status_text_uses_dot_or_dash_separators() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Ok(10.0), 0, false);
        let now = s.first_sent_at.unwrap() + Duration::from_secs(135);
        assert_eq!(probe_status_text(&s, now, 1_000, false), "ok \u{00b7} no loss \u{00b7} 1 sent over 2m 15s");
        assert_eq!(probe_status_text(&s, now, 1_000, true), "ok - no loss - 1 sent over 2m 15s");
    }

    #[test]
    fn probe_status_spans_dim_only_the_separators() {
        let mut s = TargetState::new("127.0.0.1".to_string());
        s.record_sent(0);
        s.record_result(0, Ok(10.0), 0, false);
        let theme = crate::cli::ThemeName::Default.to_theme();
        let base = Style::default().fg(Color::White);
        let spans = probe_status_spans(&s, Instant::now(), 1_000, false, &theme, base);
        assert_eq!(spans.len(), 5);
        let sep = Style::default().fg(theme.c(theme.status_sep));
        for (i, sp) in spans.iter().enumerate() {
            assert_eq!(sp.style, if i % 2 == 1 { sep } else { base }, "span {i}: {:?}", sp.content);
        }
    }
}
