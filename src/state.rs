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

use crate::hist::LogHistogram;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use crate::time::Instant;
use crate::types::Sample;

/// Per-target column cache for the step-line graph.
///
/// The graph resamples `span_cols` source samples into `out_cols` terminal columns.
/// When span_cols > out_cols (the common case) the compression ratio is fractional,
/// so naively recomputing every repaint assigns different source samples to the same
/// column on different ticks - causing shimmer.
///
/// Instead we cache the column assignments and advance them incrementally:
///  - Each new source sample shifts the window by `out_cols / take` columns (< 1).
///  - When the accumulated fraction crosses 1.0 we evict the leftmost column and
///    shift everything left by 1, computing a fresh value for the new rightmost slot.
///  - On every repaint we always refresh the rightmost column so the live edge is
///    always up-to-date; all other columns are frozen until they scroll off.
///  - A viewport resize or span change invalidates the cache and triggers a full rebuild.
#[derive(Clone, Debug)]
pub struct GraphColCache {
    pub cols:        Vec<Sample>,  // one sample per output column
    pub data_end:    usize,        // columns [0..data_end) hold real data; rest is Pending
    pub frac:        f64,          // fractional scroll accumulator
    pub push_count:  usize,        // graph_push_count at last update (not hist.len() - capped)
    pub out_cols:    usize,        // output width the cache was built for
    pub span_cols:   usize,        // span_cols the cache was built for
    pub drain_count: usize,        // total left-column evictions; used for stable drop-marker parity
}

impl GraphColCache {
    pub fn new() -> Self {
        Self { cols: Vec::new(), data_end: 0, frac: 0.0, push_count: 0, out_cols: 0, span_cols: 0, drain_count: 0 }
    }
}

/// Per-target column cache for the compact sparkline timeline.
///
/// Same incremental advancement logic as GraphColCache, but also tracks a locked
/// vertical scale (val_min/val_max).  The scale is only updated when columns actually
/// advance, so all historical columns have frozen heights between advances - only the
/// live-edge column can change on intermediate repaint ticks.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SparklineColCache {
    pub cols:         Vec<Sample>,
    pub data_end:     usize,
    pub frac:         f64,
    pub push_count:   usize,
    pub out_cols:     usize,
    pub span_cols:    usize,
    pub val_min:    f64,
    pub val_max:    f64,
}

impl SparklineColCache {
    pub fn new() -> Self {
        Self {
            cols: Vec::new(), data_end: 0, frac: 0.0,
            push_count: 0, out_cols: 0, span_cols: 0,
            val_min: 0.0, val_max: 1.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TargetState {
    pub label:          String,
    pub host:           String,  // original hostname/IP being probed (never changes after init)
    pub custom_label:   bool,
    pub exec_cmd:       String,
    pub window:         Vec<(Instant, f64)>,
    /// Every reply's RTT over the run, as a fixed-size histogram (lifetime
    /// percentiles; see `hist.rs`). Saved with the session.
    pub life_rtt_hist:  LogHistogram,
    pub history:        Vec<Sample>,
    pub pending_map:    HashMap<usize, usize>, // seq → history index
    pub early_results:  HashSet<usize>,        // seqs where result arrived before start
    /// Probe-to-probe jitter in the window, each tagged with the time of the
    /// *earlier* reply of its pair, so a jump spanning a reply that has left
    /// the window leaves with it.
    pub jitter_window:  Vec<(Instant, f64)>,
    pub latency_sum:    f64,
    pub jitter_sum:     f64,
    pub jitter_count:   u64,
    pub total_sent:     u64,
    pub drops:          u32,
    pub dups:           u32,
    pub lifetime_min:   f64,
    pub lifetime_max:   f64,
    pub latency_sq_sum: f64,
    pub win_drops:      u32,
    pub win_drop_times: Vec<Instant>,
    pub win_dups:       u32,
    pub win_dup_times:  Vec<Instant>,
    /// Start of each drop *event* in the stats window: a streak of consecutive
    /// drops counts once. Pruned like `win_drop_times`.
    pub win_drop_event_times: Vec<Instant>,
    /// Each outage (a streak reaching `GRADE_OUTAGE_MIN_DROPS` drops) in the
    /// stats window, timed when it qualified. Pruned like `win_drop_times`.
    pub win_outage_times: Vec<Instant>,
    pub last_rtt:          f64,
    pub current_jitter:    f64,
    pub srtt:              f64,   // RFC 6298 smoothed RTT
    pub rttvar:            f64,   // RFC 6298 RTT variance
    pub cur_drop_streak:   u32,   // consecutive drops right now
    pub max_drop_streak:   u32,   // longest drop streak ever seen
    pub current_pct:       f64,
    pub color_score:    f64,
    pub warning_streak: u32,
    pub last_scale:     f64,
    pub scale_anim:     Option<u8>,
    pub scale_anim_old: f64,   // scale we're transitioning FROM
    pub scale_anim_new: f64,   // scale we're transitioning TO (locked for animation duration)
    pub scale_anim_up:  bool,  // true = scaling up (sweep L→R), false = scaling down (sweep R→L)
    pub bar_anim_start: Option<Instant>, // wall-clock start of range-bar sweep animation
    pub graph_anim_start: Option<Instant>, // wall-clock start of dissolve animation
    pub waiting:        bool,
    pub calibrating:    Option<(Instant, Instant)>, // (start, until) - collecting samples, overlay shown
    pub last_was_drop:  bool,
    pub graph_interval_max: f64,              // peak RTT hit since last flush_to_graph; 0 = no hit yet
    pub graph_interval_drop: bool,            // any drop since last flush_to_graph
    pub circle_history:  Vec<u8>,
    pub graph_history:   Vec<Sample>,
    pub graph_push_count: usize,              // monotonic: incremented every flush_to_graph call
    pub graph_col_cache:      RefCell<GraphColCache>,
    pub sparkline_col_cache:  RefCell<SparklineColCache>,
    pub trail:           VecDeque<f32>,  // normalized RTT fractions (0.0–1.0 of shared_scale)
    pub bar_ema:         f64,   // EMA-smoothed RTT for range-bar cursor position (α=0.3)
    pub threshold_flash: u8,
    pub drop_flash:      u8,
    pub resolve_notice:  u8,
    pub ip_changes:      u32,
    pub resolving:       bool,
    pub resolve_error:   Option<String>,
    pub prev_ip:         Option<std::net::IpAddr>,
    pub current_ip:      Option<std::net::IpAddr>,

    /// Smoothed expected response window in ms: max(500, 1.5 × win_mtr).
    /// Visual only - does not affect probe behaviour.
    pub display_timeout_ms: f64,

    /// Wall-clock time of the last successful probe response (host was up).
    /// None until the first successful response arrives.
    pub last_up: Option<Instant>,

    /// Wall-clock time of the most recent dropped probe. Unlike `win_drop_times`,
    /// never pruned by the stats window - drives the "N lost <age> ago" verdict in
    /// the probe/uptime summary text, which should still work with `--window 0`
    /// (lifetime) or a short window long after the drop itself has aged out of it.
    pub last_drop_at: Option<Instant>,

    /// Wall-clock time the very first probe was sent. Drives the elapsed-wait
    /// display in the single-target view's compact pre-reply line.
    pub first_sent_at: Option<Instant>,

    /// `history` index of the very first successful reply. None until it arrives.
    /// Lets the single-target view's row-by-row reveal count samples *since first
    /// contact* rather than the raw resolved-sample count - a target that was down
    /// for a while before finally answering may already have dozens of drops
    /// sitting in `history`, and without this the view would jump straight to its
    /// full row count the instant that backlog counts as "revealed" data.
    pub first_success_idx: Option<usize>,

    /// --warn-rtt, for the `slow` grade. Set by the app before each result.
    pub warn_rtt: Option<f64>,
    /// Slow-moving "normal" RTT for the `degraded` grade: the window median,
    /// time-averaged with a `GRADE_BASELINE_TAU_SECS` time constant. 0 until
    /// the first reply.
    pub baseline_rtt: f64,
    pub baseline_at: Option<Instant>,
    /// When the baseline was first set (for its warm-up).
    pub baseline_since: Option<Instant>,
    /// Loss and quality grades for the status text, re-graded (with
    /// hysteresis) whenever a probe resolves or the window changes.
    pub grade: StatusGrade,
    /// Medians of the whole stats window (the jitpct column; see `WindowMedians`).
    pub medians: WindowMedians,
    /// The same over the grading span (last `GRADE_SPAN_SECS`) - what the
    /// quality grades and the degraded baseline judge.
    pub grade_medians: WindowMedians,
    grade_medians_at: Option<Instant>,
    /// What's in the grading span as of the last regrade (see `SpanCounts`).
    pub span: SpanCounts,
    /// The window's RTTs, sorted - one copy serving every window percentile
    /// column (p01..p99, median), refreshed with `medians`.
    win_sorted: Vec<f64>,
    medians_at: Option<Instant>,
    /// Every probe-to-probe jitter of the run (lifetime jitpct), like `life_rtt_hist`.
    pub life_jitter_hist: LogHistogram,
}

/// How much of each windowed collection falls in the grading span (the last
/// `GRADE_SPAN_SECS`). The status text and quality column read these rather
/// than the whole stats window, so they stay about recent behaviour even with
/// a long `--window`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanCounts {
    pub replies: usize,
    pub jitters: usize,
    pub drops: usize,
    pub drop_events: usize,
    pub outages: usize,
}

/// Where the grading span starts in each windowed collection.
#[derive(Clone, Copy)]
struct SpanStarts { w: usize, j: usize, d: usize, e: usize, o: usize }

/// True if cached medians over `n` replies, last computed at `at`, should be
/// recomputed: always for up to `GRADE_MEDIANS_FULL_N` replies, else at most
/// once per `GRADE_MEDIANS_REFRESH_SECS`.
fn medians_stale(n: usize, at: Option<Instant>, now: Instant) -> bool {
    use crate::constants::{GRADE_MEDIANS_FULL_N, GRADE_MEDIANS_MAX_REFRESH_SECS, GRADE_MEDIANS_REFRESH_SECS};
    if n <= GRADE_MEDIANS_FULL_N { return true; }
    let every = (GRADE_MEDIANS_REFRESH_SECS * n as f64 / GRADE_MEDIANS_FULL_N as f64)
        .min(GRADE_MEDIANS_MAX_REFRESH_SECS);
    !at.is_some_and(|t| now.saturating_duration_since(t).as_secs_f64() < every)
}

impl WindowMedians {
    /// From sorted RTTs and (unsorted) jitter values.
    fn of(rtts: &[f64], mut jit: Vec<f64>) -> Self {
        jit.sort_unstable_by(f64_cmp);
        let n = rtts.len();
        WindowMedians {
            n,
            med:  median_sorted(rtts),
            p99:  if n == 0 { 0.0 } else { rtts[((n as f64 * 0.99) as usize).min(n - 1)] },
            jmed: median_sorted(&jit),
        }
    }

    fn jitter_pct(&self) -> Option<f64> {
        if self.n < crate::constants::GRADE_QUALITY_MIN_SAMPLES || self.med <= 0.0 { return None; }
        Some(self.jmed / self.med * 100.0)
    }
}

/// Median-based figures over a set of replies: the whole stats window
/// (`medians`, for the jitpct column) or the grading span (`grade_medians`,
/// for the quality grades). Cached, and refreshed by `refresh_medians`, because
/// a long window is costly to sort on every probe and every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowMedians {
    /// Replies in the window when computed.
    pub n: usize,
    /// Median and 99th-percentile RTT.
    pub med: f64,
    pub p99: f64,
    /// Median probe-to-probe jitter.
    pub jmed: f64,
}

/// How much of the stats window is being lost.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum LossGrade { #[default] Normal, Lossy, Struggling }

/// How well a target answers when it does. Each flag has its own hysteresis;
/// the status text shows the first set one in the order listed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QualityFlags {
    pub slow: bool,
    pub degraded: bool,
    pub jittery: bool,
    pub spiky: bool,
}

/// Which way a measure is clearly moving: the last `GRADE_TREND_RECENT_SECS`
/// of the stats window against the rest of it. `None` (in an `Option<Trend>`)
/// means steady, or not enough data to say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trend { Improving, Worsening }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatusGrade {
    pub loss: LossGrade,
    pub quality: QualityFlags,
    /// Loss rate: recent vs the rest of the window.
    pub loss_trend: Option<Trend>,
    /// Median RTT: recent vs the rest of the window.
    pub latency_trend: Option<Trend>,
}

/// Next trend with hysteresis: keep the current direction while its STAY test
/// holds, otherwise start one whose ENTER test holds.
fn next_trend(prev: Option<Trend>, worse: (bool, bool), better: (bool, bool)) -> Option<Trend> {
    let ((worse_enter, worse_stay), (better_enter, better_stay)) = (worse, better);
    match prev {
        Some(Trend::Worsening) if worse_stay => Some(Trend::Worsening),
        Some(Trend::Improving) if better_stay => Some(Trend::Improving),
        _ if worse_enter => Some(Trend::Worsening),
        _ if better_enter => Some(Trend::Improving),
        _ => None,
    }
}

/// Hysteresis: once on, stay on while `stay` holds; otherwise turn on at `enter`.
fn hyst(was: bool, enter: bool, stay: bool) -> bool { if was { stay } else { enter } }

fn f64_cmp(a: &f64, b: &f64) -> std::cmp::Ordering { a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal) }

fn median_sorted(v: &[f64]) -> f64 {
    let n = v.len();
    if n == 0 { 0.0 } else if n.is_multiple_of(2) { (v[n / 2 - 1] + v[n / 2]) / 2.0 } else { v[n / 2] }
}

impl TargetState {
    pub fn new(label: String) -> Self {
        Self {
            host:           label.clone(),
            label,
            custom_label:   false,
            exec_cmd:       String::new(),
            window:         Vec::new(),
            life_rtt_hist:  LogHistogram::new(),
            history:        Vec::new(),
            pending_map:    HashMap::new(),
            early_results:  HashSet::new(),
            jitter_window:  Vec::new(),
            latency_sum:    0.0,
            jitter_sum:     0.0,
            jitter_count:   0,
            total_sent:     0,
            drops:          0,
            dups:           0,
            lifetime_min:   f64::MAX,
            lifetime_max:   f64::MIN,
            latency_sq_sum: 0.0,
            win_drops:      0,
            win_drop_times: Vec::new(),
            win_dups:       0,
            win_dup_times:  Vec::new(),
            last_rtt:          0.0,
            current_jitter:    0.0,
            srtt:              0.0,
            rttvar:            0.0,
            cur_drop_streak:   0,
            max_drop_streak:   0,
            current_pct:       0.0,
            color_score:    0.0,
            warning_streak: 0,
            last_scale:     0.0,
            scale_anim:     None,
            scale_anim_old: 0.0,
            scale_anim_new: 0.0,
            scale_anim_up:  true,
            bar_anim_start: None,
            graph_anim_start: None,
            waiting:        true,
            calibrating:    None,
            last_was_drop:  false,
            graph_interval_max: 0.0,
            graph_interval_drop: false,
            circle_history:  Vec::new(),
            graph_history:   Vec::new(),
            graph_push_count: 0,
            graph_col_cache:     RefCell::new(GraphColCache::new()),
            sparkline_col_cache: RefCell::new(SparklineColCache::new()),
            trail:           VecDeque::new(),
            bar_ema:         0.0,
            threshold_flash: 0,
            drop_flash:      0,
            resolve_notice:  0,
            ip_changes:      0,
            resolving:       false,
            resolve_error:   None,
            prev_ip:         None,
            current_ip:      None,
            display_timeout_ms: 1000.0,
            last_up:         None,
            last_drop_at:    None,
            first_sent_at:   None,
            first_success_idx: None,
            warn_rtt:        None,
            baseline_rtt:    0.0,
            baseline_at:     None,
            baseline_since:  None,
            grade:           StatusGrade::default(),
            medians:         WindowMedians::default(),
            grade_medians:   WindowMedians::default(),
            grade_medians_at: None,
            span:            SpanCounts::default(),
            win_sorted:      Vec::new(),
            medians_at:      None,
            life_jitter_hist: LogHistogram::new(),
            win_drop_event_times: Vec::new(),
            win_outage_times: Vec::new(),
        }
    }

    /// Called when a probe is sent. Advances the timeline immediately.
    pub fn record_sent(&mut self, seq: usize) { self.record_sent_at(Instant::now(), seq); }

    /// `record_sent` with an explicit clock (tests and simulations).
    pub fn record_sent_at(&mut self, now: Instant, seq: usize) {
        if self.first_sent_at.is_none() { self.first_sent_at = Some(now); }
        // If the result already arrived (race: ProbeResult before ProbeStarted), skip.
        if self.early_results.remove(&seq) { return; }
        self.pending_map.insert(seq, self.history.len());
        self.history.push(Sample::Pending);
        self.circle_history.push(0);
    }

    /// Called when a probe result arrives (hit or drop).
    pub fn record_result(&mut self, seq: usize, outcome: Result<f64, ()>, window_secs: u64, dup: bool) {
        self.record_result_at(Instant::now(), seq, outcome, window_secs, dup);
    }

    /// `record_result` with an explicit clock (tests and simulations).
    pub fn record_result_at(&mut self, now: Instant, seq: usize, outcome: Result<f64, ()>, window_secs: u64, dup: bool) {
        let idx = match self.pending_map.remove(&seq) {
            Some(idx) => idx,
            None => {
                // ProbeResult arrived before ProbeStarted - synthesize the history slot now.
                self.early_results.insert(seq);
                let i = self.history.len();
                self.history.push(Sample::Pending);
                self.circle_history.push(0);
                i
            }
        };
        self.waiting     = false;
        self.total_sent += 1;

        self.expire_window(now, window_secs);
        let prev_up = self.last_up;

        match outcome {
            Ok(rtt_ms) => {
                self.last_was_drop = false;
                self.last_up = Some(now);
                if self.first_success_idx.is_none() { self.first_success_idx = Some(idx); }

                // Compute p95 of the last 20 window entries BEFORE adding the current sample,
                // so the new probe is scored against prior behavior, not its own influence.
                let pre_push_len = self.window.len();
                let recent_p95   = self.recent_p95_n(20);

                self.window.push((now, rtt_ms));
                self.life_rtt_hist.insert(rtt_ms);

                // pct deviation of current probe relative to the pre-push p95.
                // Negative = better than usual, positive = worse than usual.
                let pct = if recent_p95 > 0.0 {
                    (rtt_ms - recent_p95) / recent_p95
                } else {
                    0.0
                };
                self.current_pct = pct;

                // warning_streak: consecutive probes above recent p95.
                // Guard: require at least 10 pre-push samples so p95 is meaningful.
                if pre_push_len >= 10 && pct > 0.0 {
                    self.warning_streak += 1;
                } else {
                    self.warning_streak = 0;
                }

                if self.last_rtt > 0.0 {
                    self.current_jitter = (rtt_ms - self.last_rtt).abs();
                    self.jitter_sum    += self.current_jitter;
                    self.jitter_count  += 1;
                    self.life_jitter_hist.insert(self.current_jitter);
                    // Tagged with the earlier reply's time (see `jitter_window`).
                    let pair_start = prev_up.unwrap_or(now);
                    if !self.is_expired(pair_start, now, window_secs) {
                        self.jitter_window.push((pair_start, self.current_jitter));
                    }
                }

                // color_score momentum: pushed by pct deviation, decays 12% per probe.
                // Dead band: no push unless probe is meaningfully above p95 (+25%) or
                // clearly below average (-50%).  Keeps stable targets at score ≈ 0.
                if self.window.len() >= 3 {
                    let push = if pct > 0.25 {
                        pct * 3.0
                    } else if pct < -0.50 {
                        pct * 2.0
                    } else {
                        0.0
                    };
                    self.color_score = (self.color_score + push).clamp(-3.0, 3.0);
                }
                self.color_score *= 0.88;

                self.latency_sum    += rtt_ms;
                self.latency_sq_sum += rtt_ms * rtt_ms;
                self.lifetime_min   = self.lifetime_min.min(rtt_ms);
                self.lifetime_max   = self.lifetime_max.max(rtt_ms);
                self.history[idx]   = Sample::Hit(rtt_ms);

                // Bake the circle tier at arrival time using the pre-push p95.
                // Enough history required; fall back to tier 1 (normal) during warmup.
                let tier = if pre_push_len >= 5 { pct_to_circle_tier(pct) } else { 1 };
                if idx < self.circle_history.len() {
                    self.circle_history[idx] = tier;
                }
                if dup {
                    self.dups += 1;
                    self.win_dups += 1;
                    self.win_dup_times.push(now);
                }

                self.last_scale = scale_bucket(self.win_max());
                self.last_rtt   = rtt_ms;
                self.graph_interval_max = self.graph_interval_max.max(rtt_ms);

                // RFC 6298 smoothed RTT (α=1/8) and variance (β=1/4)
                if self.srtt == 0.0 {
                    self.srtt   = rtt_ms;
                    self.rttvar = rtt_ms / 2.0;
                } else {
                    self.rttvar = 0.75 * self.rttvar + 0.25 * (self.srtt - rtt_ms).abs();
                    self.srtt   = 0.875 * self.srtt  + 0.125 * rtt_ms;
                }
                self.cur_drop_streak = 0;

                // EMA toward max(500ms, 1.5 × win_mtr). Falls back to win_avg when
                // mtr is undefined (100% loss window). Alpha=0.15: ~7 samples to track.
                let target_ms = {
                    let mtr = self.win_mtr()
                        .unwrap_or_else(|| self.win_avg().max(500.0 / 1.5));
                    (mtr * 1.5).max(500.0)
                };
                self.display_timeout_ms = self.display_timeout_ms * 0.85 + target_ms * 0.15;
            }
            Err(()) => {
                self.last_was_drop = true;
                self.graph_interval_drop = true;
                self.drop_flash  = 6;
                self.drops      += 1;
                self.win_drops  += 1;
                self.win_drop_times.push(now);
                self.last_drop_at = Some(now);
                self.history[idx] = Sample::Drop;
                if idx < self.circle_history.len() {
                    self.circle_history[idx] = 255; // drop sentinel - tier lookup not used for drops
                }
                self.cur_drop_streak += 1;
                if self.cur_drop_streak == 1 { self.win_drop_event_times.push(now); }
                if self.cur_drop_streak == crate::constants::GRADE_OUTAGE_MIN_DROPS { self.win_outage_times.push(now); }
                if self.cur_drop_streak > self.max_drop_streak {
                    self.max_drop_streak = self.cur_drop_streak;
                }
            }
        }
        self.refresh_medians(now);
        if outcome.is_ok() { self.update_baseline(now); }
        self.regrade_fresh(now);
    }

    /// Moves `baseline_rtt` toward the current window median, weighted by the
    /// time since the last update (so it means the same at any probe rate).
    fn update_baseline(&mut self, now: Instant) {
        if self.grade_medians.n == 0 { return; }
        let med = self.grade_medians.med;
        match (self.baseline_at, self.baseline_since) {
            (Some(t), Some(since)) => {
                // While young, the time constant is the baseline's own age - a
                // running average of what it has seen - so one unusually fast or
                // slow first reply can't anchor it for an hour.
                let dt = now.saturating_duration_since(t).as_secs_f64();
                let age = now.saturating_duration_since(since).as_secs_f64().max(dt).max(1e-9);
                let tau = crate::constants::GRADE_BASELINE_TAU_SECS.min(age);
                let alpha = 1.0 - (-dt / tau).exp();
                self.baseline_rtt += alpha * (med - self.baseline_rtt);
            }
            _ => { self.baseline_rtt = med; self.baseline_since = Some(now); }
        }
        self.baseline_at = Some(now);
    }

    /// Recomputes `medians` from the whole window - every update for windows
    /// up to `GRADE_MEDIANS_FULL_N` replies, at most once per
    /// `GRADE_MEDIANS_REFRESH_SECS` beyond that.
    fn refresh_medians(&mut self, now: Instant) {
        let window_fresh = medians_stale(self.window.len(), self.medians_at, now);
        if window_fresh {
            self.win_sorted.clear();
            self.win_sorted.extend(self.window.iter().map(|&(_, v)| v));
            self.win_sorted.sort_unstable_by(f64_cmp);
            self.medians = WindowMedians::of(&self.win_sorted, self.jitter_window.iter().map(|&(_, j)| j).collect());
            self.medians_at = Some(now);
        }
        let st = self.span_starts(now);
        let span_n = self.window.len() - st.w;
        if !medians_stale(span_n, self.grade_medians_at, now) { return; }
        self.grade_medians = if st.w == 0 && st.j == 0 && window_fresh {
            self.medians // the span covers the whole window (short window, lifetime mode)
        } else {
            let mut rtts: Vec<f64> = self.window[st.w..].iter().map(|&(_, v)| v).collect();
            rtts.sort_unstable_by(f64_cmp);
            WindowMedians::of(&rtts, self.jitter_window[st.j..].iter().map(|&(_, j)| j).collect())
        };
        self.grade_medians_at = Some(now);
    }

    /// Where the grading span (last `GRADE_SPAN_SECS` as of `now`) starts in
    /// each windowed collection. They're in time order, so a binary search.
    fn span_starts(&self, now: Instant) -> SpanStarts {
        let span = Duration::from_secs(crate::constants::GRADE_SPAN_SECS);
        let old = |t: Instant| now.saturating_duration_since(t) >= span;
        SpanStarts {
            w: self.window.partition_point(|&(t, _)| old(t)),
            j: self.jitter_window.partition_point(|&(t, _)| old(t)),
            d: self.win_drop_times.partition_point(|&t| old(t)),
            e: self.win_drop_event_times.partition_point(|&t| old(t)),
            o: self.win_outage_times.partition_point(|&t| old(t)),
        }
    }

    fn span_counts(&self, st: SpanStarts) -> SpanCounts {
        SpanCounts {
            replies:     self.window.len() - st.w,
            jitters:     self.jitter_window.len() - st.j,
            drops:       self.win_drop_times.len() - st.d,
            drop_events: self.win_drop_event_times.len() - st.e,
            outages:     self.win_outage_times.len() - st.o,
        }
    }

    /// `medians` over the whole run, from the lifetime histograms (within 1%).
    pub fn life_medians(&self) -> WindowMedians {
        let h = &self.life_rtt_hist;
        WindowMedians { n: h.len() as usize, med: h.median(), p99: h.percentile(0.99), jmed: self.life_jitter_hist.median() }
    }

    /// Relative jitter: median probe-to-probe jitter as a percentage of median
    /// RTT, over the stats window when `window`, else the whole run. Medians,
    /// so one outlier barely moves it - unlike cv, which is stddev / mean.
    /// `None` until there are enough replies to judge. The `jittery` grade
    /// applies the same test to the grading span (`grade_medians`).
    pub fn jitter_pct(&self, window: bool) -> Option<f64> {
        if window { self.medians.jitter_pct() } else { self.life_medians().jitter_pct() }
    }
    pub fn win_jitter_pct(&self) -> Option<f64> { self.jitter_pct(true) }
    pub fn life_jitter_pct(&self) -> Option<f64> { self.jitter_pct(false) }

    /// Whether the jitpct shown (window when `window`, else lifetime) is past
    /// the `jittery` entry thresholds (ratio and ms floor) - for colouring the
    /// column, whose span differs from the grade's (the last few minutes).
    pub fn jitter_over_threshold(&self, window: bool) -> bool {
        use crate::constants::{GRADE_JITTERY_ENTER_MS, GRADE_JITTERY_ENTER_RATIO};
        let m = if window { self.medians } else { self.life_medians() };
        m.jitter_pct().is_some() && m.jmed >= m.med * GRADE_JITTERY_ENTER_RATIO && m.jmed >= GRADE_JITTERY_ENTER_MS
    }

    /// Recomputes `grade` from the window, keeping each grade on until its
    /// (lower) STAY threshold is crossed. Thresholds are in `constants.rs`.
    pub fn regrade(&mut self) { self.regrade_at(Instant::now()); }

    /// `regrade` as of `now` (trends split the window by time).
    pub fn regrade_at(&mut self, now: Instant) {
        self.refresh_medians(now);
        self.regrade_fresh(now);
    }

    /// `regrade_at` with `medians` already refreshed for this update.
    fn regrade_fresh(&mut self, now: Instant) {
        use crate::constants::*;
        let prev = self.grade;

        let st = self.span_starts(now);
        self.span = self.span_counts(st);
        let resolved = self.span.replies + self.span.drops;
        let pct = if resolved == 0 { 0.0 } else { self.span.drops as f64 / resolved as f64 * 100.0 };
        let struggling = prev.loss == LossGrade::Struggling;
        let lossy_or_worse = prev.loss != LossGrade::Normal;
        let loss = if resolved < GRADE_LOSS_MIN_SAMPLES { LossGrade::Normal }
            else if hyst(struggling, pct >= GRADE_STRUGGLING_ENTER_PCT, pct >= GRADE_STRUGGLING_STAY_PCT) { LossGrade::Struggling }
            else if hyst(lossy_or_worse, pct >= GRADE_LOSSY_ENTER_PCT, pct >= GRADE_LOSSY_STAY_PCT) { LossGrade::Lossy }
            else { LossGrade::Normal };

        let WindowMedians { n, med, p99, jmed } = self.grade_medians;
        let quality = if n < GRADE_QUALITY_MIN_SAMPLES { QualityFlags::default() } else {
            // No baseline to compare to until it has warmed up.
            let warm = self.baseline_since.is_some_and(|t| now.saturating_duration_since(t).as_secs_f64() >= GRADE_BASELINE_WARMUP_SECS);
            let base = if warm { self.baseline_rtt } else { 0.0 };
            let q = prev.quality;
            QualityFlags {
                slow: self.warn_rtt.is_some_and(|w| hyst(q.slow, med >= w, med >= w * GRADE_SLOW_STAY_FRAC)),
                degraded: base > 0.0 && hyst(q.degraded,
                    med >= base * GRADE_DEGRADED_ENTER_RATIO && med - base >= GRADE_DEGRADED_ENTER_MS,
                    med >= base * GRADE_DEGRADED_STAY_RATIO && med - base >= GRADE_DEGRADED_STAY_MS),
                jittery: hyst(q.jittery,
                    jmed >= med * GRADE_JITTERY_ENTER_RATIO && jmed >= GRADE_JITTERY_ENTER_MS,
                    jmed >= med * GRADE_JITTERY_STAY_RATIO && jmed >= GRADE_JITTERY_STAY_MS),
                spiky: hyst(q.spiky,
                    p99 >= med * GRADE_SPIKY_ENTER_RATIO && p99 - med >= GRADE_SPIKY_ENTER_MS,
                    p99 >= med * GRADE_SPIKY_STAY_RATIO && p99 - med >= GRADE_SPIKY_STAY_MS),
            }
        };
        let (loss_trend, latency_trend) = self.trends(now, prev, st);
        self.grade = StatusGrade { loss, quality, loss_trend, latency_trend };
    }

    /// Loss-rate and median-RTT trends: the last `GRADE_TREND_RECENT_SECS` of
    /// the window against the rest of it.
    fn trends(&self, now: Instant, prev: StatusGrade, st: SpanStarts) -> (Option<Trend>, Option<Trend>) {
        use crate::constants::*;
        let recent = |t: Instant| now.saturating_duration_since(t).as_secs_f64() < GRADE_TREND_RECENT_SECS;
        let (mut rtt_new, mut rtt_old): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
        for &(t, v) in &self.window[st.w..] { if recent(t) { rtt_new.push(v) } else { rtt_old.push(v) } }
        let span_drops = &self.win_drop_times[st.d..];
        let drops_new = span_drops.iter().filter(|&&t| recent(t)).count();
        let drops_old = span_drops.len() - drops_new;
        let (n_new, n_old) = (rtt_new.len() + drops_new, rtt_old.len() + drops_old);
        if n_new < GRADE_TREND_MIN_SAMPLES || n_old < GRADE_TREND_MIN_SAMPLES { return (None, None); }

        let loss_new = drops_new as f64 / n_new as f64 * 100.0;
        let loss_old = drops_old as f64 / n_old as f64 * 100.0;
        let r = GRADE_LOSS_TREND_ENTER_RATIO;
        let loss_trend = next_trend(prev.loss_trend,
            (loss_new - loss_old >= GRADE_LOSS_TREND_ENTER_PP && loss_new >= loss_old * r,
             loss_new - loss_old >= GRADE_LOSS_TREND_STAY_PP),
            (loss_old - loss_new >= GRADE_LOSS_TREND_ENTER_PP && loss_old >= loss_new * r,
             loss_old - loss_new >= GRADE_LOSS_TREND_STAY_PP));

        let latency_trend = if rtt_new.len() < GRADE_TREND_MIN_SAMPLES || rtt_old.len() < GRADE_TREND_MIN_SAMPLES { None } else {
            let sort = |v: &mut Vec<f64>| v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sort(&mut rtt_new);
            sort(&mut rtt_old);
            let (m_new, m_old) = (median_sorted(&rtt_new), median_sorted(&rtt_old));
            let (er, sr) = (GRADE_LATENCY_TREND_ENTER_RATIO, GRADE_LATENCY_TREND_STAY_RATIO);
            let (ems, sms) = (GRADE_LATENCY_TREND_ENTER_MS, GRADE_LATENCY_TREND_STAY_MS);
            next_trend(prev.latency_trend,
                (m_new >= m_old * er && m_new - m_old >= ems, m_new >= m_old * sr && m_new - m_old >= sms),
                (m_old >= m_new * er && m_old - m_new >= ems, m_old >= m_new * sr && m_old - m_new >= sms))
        };
        (loss_trend, latency_trend)
    }

    /// True if the most recently resolved probe (skipping any still-pending ones)
    /// was a drop - i.e. the target is down right now, as opposed to merely having
    /// had some drops somewhere in its recent history.
    pub fn is_currently_down(&self) -> bool {
        self.history.iter().rev().find(|s| !s.is_pending()).map(|s| s.is_drop()).unwrap_or(false)
    }

    pub fn win_min(&self) -> f64 { self.window.iter().map(|&(_, v)| v).fold(f64::MAX, f64::min) }
    pub fn win_max(&self) -> f64 { self.window.iter().map(|&(_, v)| v).fold(f64::MIN, f64::max) }
    pub fn win_avg(&self) -> f64 {
        if self.window.is_empty() { return 0.0; }
        self.window.iter().map(|(_, v)| v).sum::<f64>() / self.window.len() as f64
    }
    pub fn win_jitter_avg(&self) -> f64 {
        if self.jitter_window.is_empty() { return 0.0; }
        self.jitter_window.iter().map(|&(_, j)| j).sum::<f64>() / self.jitter_window.len() as f64
    }
    /// From the cached sorted window (see `win_sorted`), so drawing a dozen
    /// percentile cells per frame doesn't re-sort the window each time.
    fn win_percentile(&self, pct: f64) -> f64 {
        let vals = &self.win_sorted;
        if vals.len() < 2 { return self.win_avg(); }
        let idx = ((vals.len() as f64 * pct) as usize).min(vals.len() - 1);
        vals[idx]
    }

    pub fn win_p95(&self) -> f64 { self.win_percentile(0.95) }

    /// 95th percentile of the last `n` window entries (current state, no new sample).
    /// Used to score an incoming probe against recent behaviour before it enters the window.
    fn recent_p95_n(&self, n: usize) -> f64 {
        let len = self.window.len();
        if len == 0 { return 0.0; }
        let start = len.saturating_sub(n);
        let mut vals: Vec<f64> = self.window[start..].iter().map(|&(_, v)| v).collect();
        if vals.len() < 2 { return vals[0]; }
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((vals.len() as f64 * 0.95) as usize).min(vals.len() - 1);
        vals[idx]
    }

    /// Replies over the whole run (results minus drops) - the count the
    /// lifetime RTT sums are over.
    pub fn life_replies(&self) -> u64 { self.total_sent.saturating_sub(self.drops as u64) }

    /// Lifetime mean RTT: summed RTT over *replies* (dividing by every result,
    /// drops included, would understate it by the loss rate).
    pub fn avg_latency(&self) -> f64 {
        let n = self.life_replies();
        if n > 0 { self.latency_sum / n as f64 } else { 0.0 }
    }
    pub fn avg_jitter(&self) -> f64 {
        if self.jitter_count > 0 { self.jitter_sum / self.jitter_count as f64 } else { 0.0 }
    }
    pub fn life_min(&self) -> f64 { if self.lifetime_min == f64::MAX { 0.0 } else { self.lifetime_min } }
    pub fn life_max(&self) -> f64 { if self.lifetime_max == f64::MIN { 0.0 } else { self.lifetime_max } }
    /// Lifetime (population) stddev from the running sums. Sum-of-squares
    /// loses some precision to cancellation, but RTTs are small, positive and
    /// spread out enough that it's far below display resolution (a 10ms-ish
    /// mean leaves ~10 of a double's ~16 digits even over 10^7 replies).
    pub fn life_stddev(&self) -> f64 {
        let n = self.life_replies();
        if n < 2 { return 0.0; }
        let avg = self.avg_latency();
        let mean_sq = self.latency_sq_sum / n as f64;
        (mean_sq - avg * avg).max(0.0).sqrt()
    }
    pub fn win_stddev(&self) -> f64 {
        let n = self.window.len();
        if n < 2 { return 0.0; }
        let avg = self.win_avg();
        let variance = self.window.iter().map(|(_, v)| (v - avg).powi(2)).sum::<f64>() / n as f64;
        variance.sqrt()
    }

    pub fn win_p99(&self) -> f64 { self.win_percentile(0.99) }
    pub fn win_p01(&self) -> f64 { self.win_percentile(0.01) }
    pub fn win_p10(&self) -> f64 { self.win_percentile(0.10) }
    pub fn win_p90(&self) -> f64 { self.win_percentile(0.90) }

    pub fn win_median(&self) -> f64 { median_sorted(&self.win_sorted) }

    /// Lifetime percentiles come from `life_rtt_hist`: within 1%, exact
    /// at the min and max, constant memory and cost however long the run.
    fn life_percentile(&self, pct: f64) -> f64 { self.life_rtt_hist.percentile(pct) }
    /// True once any reply has been seen this run (lifetime stats have data).
    pub fn has_life_rtts(&self) -> bool { !self.life_rtt_hist.is_empty() }
    pub fn life_p01(&self) -> f64 { self.life_percentile(0.01) }
    pub fn life_p10(&self) -> f64 { self.life_percentile(0.10) }
    pub fn life_p95(&self) -> f64 { self.life_percentile(0.95) }
    pub fn life_p99(&self) -> f64 { self.life_percentile(0.99) }
    pub fn life_median(&self) -> f64 { self.life_rtt_hist.median() }

    pub fn win_loss_pct(&self) -> f64 {
        let total = self.window.len() + self.win_drops as usize;
        if total == 0 { return 0.0; }
        self.win_drops as f64 / total as f64 * 100.0
    }

    pub fn win_cv(&self) -> f64 {
        let avg = self.win_avg();
        if avg == 0.0 { return 0.0; }
        self.win_stddev() / avg * 100.0
    }

    pub fn life_cv(&self) -> f64 {
        let avg = self.avg_latency();
        if avg == 0.0 { return 0.0; }
        self.life_stddev() / avg * 100.0
    }

    pub fn life_loss_pct(&self) -> f64 {
        if self.total_sent == 0 { return 0.0; }
        self.drops as f64 / self.total_sent as f64 * 100.0
    }

    /// Loss % over the stats window when `window`, else the whole run; `None`
    /// until there is something to judge (nothing resolved in the window /
    /// nothing sent yet), so "no data" stays distinct from "no loss".
    pub fn loss_pct(&self, window: bool) -> Option<f64> {
        if window {
            (self.window.len() + self.win_drops as usize > 0).then(|| self.win_loss_pct())
        } else {
            (self.total_sent > 0).then(|| self.life_loss_pct())
        }
    }

    /// Drop count over the stats window when `window`, else the whole run.
    pub fn drop_count(&self, window: bool) -> u64 {
        if window { self.win_drops as u64 } else { self.drops as u64 }
    }
    pub fn life_mtr(&self) -> Option<f64> {
        let avg = self.avg_latency();
        if avg == 0.0 { return None; }
        let loss = self.life_loss_pct() / 100.0;
        if loss >= 1.0 { return None; }
        Some(avg / (1.0 - loss))
    }

    /// Mean time to reliability (window-based): win_avg / (1 - loss_fraction).
    /// Blends RTT and packet loss into a single delivery-cost metric.
    /// Returns None when there is no data or 100% loss.
    pub fn win_mtr(&self) -> Option<f64> {
        let avg = self.win_avg();
        if avg <= 0.0 { return None; }
        let win_total = self.window.len() + self.win_drops as usize;
        let loss_frac = if win_total > 0 {
            self.win_drops as f64 / win_total as f64
        } else {
            self.life_loss_pct() / 100.0
        };
        if loss_frac >= 1.0 { return None; }
        Some(avg / (1.0 - loss_frac))
    }


    /// The value a numeric sort mode orders by, from the same stats the columns
    /// show: the stats window when `window`, else lifetime. `None` = nothing to rank yet; sorts
    /// treat it as worst. Non-numeric modes (none/name/seen/lost) are `None`.
    pub fn sort_stat(&self, mode: &crate::cli::SortMode, window: bool) -> Option<f64> {
        use crate::cli::SortMode as M;
        let has = if window { !self.window.is_empty() } else { self.has_life_rtts() };
        let rtt = |w: f64, l: f64| if !has { None } else if window { Some(w) } else { Some(l) };
        match mode {
            M::Loss   => Some(if window { self.win_loss_pct() } else { self.life_loss_pct() }),
            M::Mtr    => if window { self.win_mtr() } else { self.life_mtr() },
            M::Avg    => rtt(self.win_avg(), self.avg_latency()),
            M::Std    => rtt(self.win_stddev(), self.life_stddev()),
            M::Jitter => rtt(self.win_jitter_avg(), self.avg_jitter()),
            M::P01    => rtt(self.win_p01(), self.life_p01()),
            M::P10    => rtt(self.win_p10(), self.life_p10()),
            M::P50    => rtt(self.win_median(), self.life_median()),
            M::P95    => rtt(self.win_p95(), self.life_p95()),
            M::P99    => rtt(self.win_p99(), self.life_p99()),
            M::Cv     => rtt(self.win_cv(), self.life_cv()),
            M::Jitpct => self.jitter_pct(window),
            M::Srtt   => (self.srtt > 0.0).then_some(self.srtt),
            M::None | M::Name | M::Seen | M::Lost => None,
        }
    }

    /// True if a sample taken at `t` is outside the window as of `now`.
    /// `window_secs` 0 (lifetime mode) still keeps a short window for the
    /// status and quality grades (`GRADE_SPAN_SECS`).
    fn is_expired(&self, t: Instant, now: Instant, window_secs: u64) -> bool {
        let secs = if window_secs == 0 { crate::constants::GRADE_SPAN_SECS } else { window_secs };
        now.saturating_duration_since(t) >= Duration::from_secs(secs)
    }

    /// Drops everything older than the window from the windowed collections
    /// (replies, jitter, drops, dups, drop events, outages). Runs on every
    /// result - reply or drop - and from the UI tick, so a target that stops
    /// answering still ages out. Returns true if anything was removed.
    pub fn expire_window(&mut self, now: Instant, window_secs: u64) -> bool {
        let secs = if window_secs == 0 { crate::constants::GRADE_SPAN_SECS } else { window_secs };
        let old = |t: &Instant| now.saturating_duration_since(*t) >= Duration::from_secs(secs);
        // Samples are pushed in time order, so a fresh head means nothing to do.
        fn prune<T>(v: &mut Vec<T>, ts: impl Fn(&T) -> Instant, old: &impl Fn(&Instant) -> bool) -> bool {
            if !v.first().is_some_and(|x| old(&ts(x))) { return false; }
            v.retain(|x| !old(&ts(x)));
            true
        }
        let mut changed = false;
        changed |= prune(&mut self.window,               |&(t, _)| t, &old);
        changed |= prune(&mut self.jitter_window,        |&(t, _)| t, &old);
        changed |= prune(&mut self.win_drop_times,       |&t| t, &old);
        changed |= prune(&mut self.win_dup_times,        |&t| t, &old);
        changed |= prune(&mut self.win_drop_event_times, |&t| t, &old);
        changed |= prune(&mut self.win_outage_times,     |&t| t, &old);
        self.win_drops = self.win_drop_times.len() as u32;
        self.win_dups  = self.win_dup_times.len() as u32;
        changed
    }

    /// `expire_window`, re-grading if anything left the window. For the UI
    /// tick, so stats and grades keep moving while no results arrive.
    pub fn expire_window_and_regrade(&mut self, now: Instant, window_secs: u64) {
        let expired = self.expire_window(now, window_secs);
        // Samples also age out of the (shorter) grading span while still in the window.
        let span_moved = self.span_counts(self.span_starts(now)) != self.span;
        if expired || span_moved { self.regrade_at(now); }
    }

    /// Resize the rolling window to a new duration, preserving existing data that fits.
    /// Trims timestamps-based collections to the new window; trims graph_history to the new cap.
    /// Resets animation state but does NOT set calibrating — caller does that if needed.
    pub fn resize_window(&mut self, new_window_secs: u64, graph_max_entries: usize) {
        let now = Instant::now();
        self.expire_window(now, new_window_secs);
        self.medians_at = None; // a new span: recompute even a long window now
        self.regrade_at(now);

        if self.graph_history.len() > graph_max_entries {
            let excess = self.graph_history.len() - graph_max_entries;
            self.graph_history.drain(..excess);
        }

        *self.graph_col_cache.borrow_mut()     = GraphColCache::new();
        *self.sparkline_col_cache.borrow_mut() = SparklineColCache::new();

        self.scale_anim       = None;
        self.bar_anim_start   = None;
        self.graph_anim_start = None;
    }


    /// Snapshot the current probe state into graph_history. Called at graph_interval rate.
    /// max_entries caps the history length to avoid unbounded growth.
    ///
    /// Each entry reflects only events that occurred during the just-elapsed interval:
    ///   - a Hit (peak RTT) if any probe completed successfully
    ///   - a Drop if any probe dropped (peak takes precedence when both occurred)
    ///   - Pending if no probe completed in either direction
    ///
    /// Pending entries render as a dimmed live-edge in the graph and are skipped by
    /// stats consumers, so the graph cannot fabricate data when probes lag the tick.
    pub fn flush_to_graph(&mut self, max_entries: usize) {
        if self.waiting { return; }
        let sample = if self.graph_interval_max > 0.0 {
            let peak = self.graph_interval_max;
            self.graph_interval_max  = 0.0;
            self.graph_interval_drop = false;
            Sample::Hit(peak)
        } else if self.graph_interval_drop {
            self.graph_interval_drop = false;
            Sample::Drop
        } else {
            Sample::Pending
        };
        self.graph_push_count += 1;
        self.graph_history.push(sample);
        if self.graph_history.len() > max_entries {
            let excess = self.graph_history.len() - max_entries;
            self.graph_history.drain(..excess);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MtrTrend {
    WarmingUp,  // not enough history yet - ·
    SteepUp,    // >25% worse  - ↑
    GentleUp,   // 10–25% worse - ↗
    Flat,       // <10% change  - →
    GentleDown, // 10–25% better - ↘
    SteepDown,  // >25% better  - ↓
    AllDrops,   // all drops in window - ✕
}

impl TargetState {
    /// Compute the MTR trend by comparing the last ~60 seconds of graph history
    /// (recent) against a bounded baseline of the preceding ~5 minutes.
    ///
    /// AllDrops is detected early on a quick 10 s window regardless of history
    /// depth.  Trend arrows are suppressed until enough baseline has accumulated
    /// (at least as many samples as the recent window).
    ///
    /// Thresholds: <10% or <2ms abs → Flat; 10–25% → Gentle; >25% → Steep.
    pub fn mtr_trend(&self, graph_interval_ms: u64) -> MtrTrend {
        let total = self.graph_history.len();

        // AllDrops: quick check on last ~10 s - intentionally fast, not gated on depth.
        let quick_n = ((10_000u64 / graph_interval_ms.max(1)) as usize).max(4).min(total);
        if quick_n >= 4 {
            let tail = &self.graph_history[total - quick_n..];
            let real: Vec<&Sample> = tail.iter().filter(|s| !s.is_pending()).collect();
            if !real.is_empty() && real.iter().all(|s| s.is_drop()) {
                return MtrTrend::AllDrops;
            }
        }

        // Recent window: last 60 s.  Larger than the old 10 s so individual probe
        // noise doesn't flip the arrow on every tick.
        let n_recent = ((60_000u64 / graph_interval_ms.max(1)) as usize).max(8);
        if total < n_recent { return MtrTrend::WarmingUp; }

        let recent = &self.graph_history[total - n_recent..];

        // Baseline: the window immediately before `recent`, capped at 5× n_recent
        // (~5 min).  Bounding this prevents very old stable history from masking a
        // genuine recent change and gives the arrow a "rolling" feel.
        let n_baseline_cap = n_recent * 5;
        let baseline_end   = total - n_recent;
        let baseline_start = baseline_end.saturating_sub(n_baseline_cap);
        let baseline       = &self.graph_history[baseline_start..baseline_end];

        // Need at least as much baseline as recent before committing to a direction.
        if baseline.len() < n_recent { return MtrTrend::WarmingUp; }

        let mtr_of = |slice: &[Sample]| -> Option<f64> {
            let count = slice.iter().filter(|s| !s.is_pending()).count();
            if count == 0 { return None; }
            let hits: Vec<f64> = slice.iter().filter_map(|s| s.rtt()).collect();
            if hits.is_empty() { return None; }
            let avg = hits.iter().sum::<f64>() / hits.len() as f64;
            let loss = (count - hits.len()) as f64 / count as f64;
            if loss >= 1.0 { return None; }
            Some(avg / (1.0 - loss))
        };

        let (Some(base_mtr), Some(recent_mtr)) = (mtr_of(baseline), mtr_of(recent)) else {
            return MtrTrend::Flat;
        };

        let abs_diff = (recent_mtr - base_mtr).abs();
        if abs_diff < 2.0 { return MtrTrend::Flat; }
        let rel = abs_diff / base_mtr;
        if recent_mtr > base_mtr {
            if rel >= 0.25 { MtrTrend::SteepUp   } else if rel >= 0.10 { MtrTrend::GentleUp   } else { MtrTrend::Flat }
        } else {
            if rel >= 0.25 { MtrTrend::SteepDown } else if rel >= 0.10 { MtrTrend::GentleDown } else { MtrTrend::Flat }
        }
    }
}

/// Map a pct-from-recent-p95 value to a circle tier (0=fast, 1=normal, 2=high).
pub fn pct_to_circle_tier(pct: f64) -> u8 {
    use crate::constants::{TIER_FAST_PCT, TIER_HIGH_PCT};
    if pct < TIER_FAST_PCT { 0 }
    else if pct <= TIER_HIGH_PCT { 1 }
    else { 2 }
}

pub fn scale_bucket(max_ms: f64) -> f64 {
    if max_ms <= 10.0 || max_ms == f64::MAX { 10.0 }
    else if max_ms <= 50.0  { 50.0 }
    else if max_ms <= 100.0 { 100.0 }
    else { (max_ms / 100.0).ceil() * 100.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_pct_is_median_based_and_matches_the_jittery_grade() {
        let fill = |rtts: &[f64]| {
            let mut s = TargetState::new("127.0.0.1".to_string());
            for (seq, &r) in rtts.iter().enumerate() { s.record_sent(seq); s.record_result(seq, Ok(r), 0, false); }
            s
        };
        // Too few replies to judge.
        assert_eq!(fill(&[10.0; 5]).win_jitter_pct(), None);

        // Steady 10ms with one 1000ms outlier: cv blows up, relative jitter stays ~0.
        let mut steady = vec![10.0; 40];
        steady[20] = 1_000.0;
        let s = fill(&steady);
        assert!(s.win_cv() > 100.0, "cv {}", s.win_cv());
        assert!(s.win_jitter_pct().unwrap() < 1.0, "jit% {:?}", s.win_jitter_pct());
        assert!(!s.grade.quality.jittery);

        // Alternating 5/25ms: median RTT 15, every step 20ms -> 133%, and graded jittery.
        let alt: Vec<f64> = (0..40).map(|i| if i % 2 == 0 { 5.0 } else { 25.0 }).collect();
        let s = fill(&alt);
        assert!((s.win_jitter_pct().unwrap() - 133.3).abs() < 0.1, "{:?}", s.win_jitter_pct());
        assert!(s.grade.quality.jittery);
    }

    // Enough window to prevent time-based eviction within any unit test run.
    const BIG_WINDOW: u64 = 86_400;

    fn make_state(rtts: &[f64]) -> TargetState {
        let mut s = TargetState::new("test".to_string());
        for (i, &rtt) in rtts.iter().enumerate() {
            s.record_sent(i);
            s.record_result(i, Ok(rtt), BIG_WINDOW, false);
        }
        s
    }

    // --- scale_bucket ---

    #[test]
    fn scale_zero_gives_ten() { assert_eq!(scale_bucket(0.0), 10.0); }

    #[test]
    fn scale_negative_gives_ten() { assert_eq!(scale_bucket(-5.0), 10.0); }

    #[test]
    fn scale_f64_min_gives_ten() { assert_eq!(scale_bucket(f64::MIN), 10.0); }

    #[test]
    fn scale_f64_max_gives_ten() { assert_eq!(scale_bucket(f64::MAX), 10.0); }

    #[test]
    fn scale_within_ten() { assert_eq!(scale_bucket(5.0), 10.0); }

    #[test]
    fn scale_exactly_ten() { assert_eq!(scale_bucket(10.0), 10.0); }

    #[test]
    fn scale_just_over_ten() { assert_eq!(scale_bucket(10.1), 50.0); }

    #[test]
    fn scale_within_fifty() { assert_eq!(scale_bucket(30.0), 50.0); }

    #[test]
    fn scale_exactly_fifty() { assert_eq!(scale_bucket(50.0), 50.0); }

    #[test]
    fn scale_just_over_fifty() { assert_eq!(scale_bucket(50.1), 100.0); }

    #[test]
    fn scale_exactly_hundred() { assert_eq!(scale_bucket(100.0), 100.0); }

    #[test]
    fn scale_just_over_hundred() { assert_eq!(scale_bucket(101.0), 200.0); }

    #[test]
    fn scale_two_fifty() { assert_eq!(scale_bucket(250.0), 300.0); }

    #[test]
    fn scale_exactly_three_hundred() { assert_eq!(scale_bucket(300.0), 300.0); }

    // --- rolling window stats ---

    #[test]
    fn win_avg_empty_is_zero() {
        let s = TargetState::new("test".to_string());
        assert_eq!(s.win_avg(), 0.0);
    }

    #[test]
    fn win_stats_basic() {
        let s = make_state(&[10.0, 20.0, 30.0]);
        assert_eq!(s.win_min(), 10.0);
        assert_eq!(s.win_max(), 30.0);
        assert!((s.win_avg() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn win_min_single_value() {
        let s = make_state(&[42.0]);
        assert_eq!(s.win_min(), 42.0);
        assert_eq!(s.win_max(), 42.0);
    }

    #[test]
    fn win_p95_single_value_returns_avg() {
        let s = make_state(&[50.0]);
        // < 2 values → falls back to win_avg
        assert_eq!(s.win_p95(), s.win_avg());
    }

    #[test]
    fn win_p95_two_values() {
        let s = make_state(&[10.0, 100.0]);
        // idx = (2 * 0.95) as usize = 1 → sorted[1] = 100
        assert_eq!(s.win_p95(), 100.0);
    }

    #[test]
    fn win_p95_ten_values() {
        let s = make_state(&[10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0]);
        // idx = (10 * 0.95) as usize = 9 → sorted[9] = 100
        assert_eq!(s.win_p95(), 100.0);
    }

    // --- lifetime stats ---

    #[test]
    fn avg_latency_correct() {
        let s = make_state(&[10.0, 20.0, 30.0]);
        assert!((s.avg_latency() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn avg_latency_no_hits_is_zero() {
        let s = TargetState::new("test".to_string());
        assert_eq!(s.avg_latency(), 0.0);
    }

    #[test]
    fn life_min_max_correct() {
        let s = make_state(&[10.0, 50.0, 25.0]);
        assert_eq!(s.life_min(), 10.0);
        assert_eq!(s.life_max(), 50.0);
    }

    #[test]
    fn life_min_max_no_hits_returns_zero() {
        let s = TargetState::new("test".to_string());
        assert_eq!(s.life_min(), 0.0);
        assert_eq!(s.life_max(), 0.0);
    }

    #[test]
    fn life_loss_pct_no_drops() {
        let s = make_state(&[10.0, 20.0]);
        assert_eq!(s.life_loss_pct(), 0.0);
    }

    #[test]
    fn life_loss_pct_all_drops() {
        let mut s = TargetState::new("test".to_string());
        s.record_sent(0); s.record_result(0, Err(()), BIG_WINDOW, false);
        s.record_sent(1); s.record_result(1, Err(()), BIG_WINDOW, false);
        assert_eq!(s.life_loss_pct(), 100.0);
    }

    #[test]
    fn life_loss_pct_half_drops() {
        let mut s = TargetState::new("test".to_string());
        s.record_sent(0); s.record_result(0, Ok(50.0), BIG_WINDOW, false);
        s.record_sent(1); s.record_result(1, Err(()), BIG_WINDOW, false);
        s.record_sent(2); s.record_result(2, Ok(50.0), BIG_WINDOW, false);
        s.record_sent(3); s.record_result(3, Err(()), BIG_WINDOW, false);
        assert!((s.life_loss_pct() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn loss_pct_is_none_until_there_is_data() {
        let s = TargetState::new("x".to_string());
        assert_eq!(s.loss_pct(true), None);
        assert_eq!(s.loss_pct(false), None);
    }

    #[test]
    fn life_loss_pct_zero_sent_is_zero() {
        let s = TargetState::new("test".to_string());
        assert_eq!(s.life_loss_pct(), 0.0);
    }

    #[test]
    fn life_stddev_uniform_is_zero() {
        let s = make_state(&[50.0, 50.0, 50.0]);
        assert!((s.life_stddev() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn life_stddev_single_value_is_zero() {
        let s = make_state(&[50.0]);
        assert_eq!(s.life_stddev(), 0.0);
    }

    #[test]
    fn life_stddev_two_values() {
        // values: 50, 100 → avg=75, mean_sq=(2500+10000)/2=6250, stddev=sqrt(625)=25
        let s = make_state(&[50.0, 100.0]);
        assert!((s.life_stddev() - 25.0).abs() < 1e-6);
    }

    // --- record_sent / record_result state transitions ---

    #[test]
    fn record_sent_adds_pending_to_history() {
        let mut s = TargetState::new("test".to_string());
        assert!(s.history.is_empty());
        s.record_sent(1);
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history[0], Sample::Pending);
    }

    #[test]
    fn record_result_hit_updates_history_and_counters() {
        let mut s = TargetState::new("test".to_string());
        s.record_sent(1);
        s.record_result(1, Ok(42.0), BIG_WINDOW, false);
        assert_eq!(s.history[0], Sample::Hit(42.0));
        assert_eq!(s.total_sent, 1);
        assert_eq!(s.drops, 0);
        assert_eq!(s.win_drops, 0);
        assert_eq!(s.last_rtt, 42.0);
    }

    #[test]
    fn record_result_drop_updates_history_and_counters() {
        let mut s = TargetState::new("test".to_string());
        s.record_sent(1);
        s.record_result(1, Err(()), BIG_WINDOW, false);
        assert_eq!(s.history[0], Sample::Drop);
        assert_eq!(s.total_sent, 1);
        assert_eq!(s.drops, 1);
        assert_eq!(s.win_drops, 1);
        assert!(s.last_was_drop);
    }

    #[test]
    fn record_result_early_arrival_before_sent() {
        let mut s = TargetState::new("test".to_string());
        // Result arrives before record_sent is called
        s.record_result(99, Ok(10.0), BIG_WINDOW, false);
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history[0], Sample::Hit(10.0));
        assert_eq!(s.early_results.len(), 1);
        // Late record_sent should consume the early_results entry and add nothing
        s.record_sent(99);
        assert!(s.early_results.is_empty());
        assert_eq!(s.history.len(), 1); // no extra slot added
    }

    #[test]
    fn jitter_tracked_after_two_hits() {
        let mut s = TargetState::new("test".to_string());
        s.record_sent(0); s.record_result(0, Ok(50.0), BIG_WINDOW, false);
        s.record_sent(1); s.record_result(1, Ok(70.0), BIG_WINDOW, false);
        assert!((s.current_jitter - 20.0).abs() < 1e-9);
        assert_eq!(s.jitter_count, 1);
    }

    #[test]
    fn avg_jitter_after_three_hits() {
        let mut s = TargetState::new("test".to_string());
        // jitter after 2nd: |60-40|=20, after 3rd: |80-60|=20 → avg=20
        s.record_sent(0); s.record_result(0, Ok(40.0), BIG_WINDOW, false);
        s.record_sent(1); s.record_result(1, Ok(60.0), BIG_WINDOW, false);
        s.record_sent(2); s.record_result(2, Ok(80.0), BIG_WINDOW, false);
        assert!((s.avg_jitter() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn waiting_flag_cleared_on_first_result() {
        let mut s = TargetState::new("test".to_string());
        assert!(s.waiting);
        s.record_sent(0);
        s.record_result(0, Ok(10.0), BIG_WINDOW, false);
        assert!(!s.waiting);
    }

    // --- mtr_trend ---
    // At 1000 ms interval: n_recent = 60 samples (60 s).
    // Baseline capped at 5 × n_recent = 300 samples (5 min).
    // Minimum to show a non-Flat trend: baseline.len() >= n_recent → total >= 120.

    fn state_with_graph(samples: &[f64]) -> TargetState {
        let mut s = TargetState::new("test".to_string());
        for &rtt in samples {
            s.graph_history.push(Sample::Hit(rtt));
        }
        s
    }

    fn state_with_drops(n: usize) -> TargetState {
        let mut s = TargetState::new("test".to_string());
        for _ in 0..n {
            s.graph_history.push(Sample::Drop);
        }
        s
    }

    #[test]
    fn trend_flat_too_few_samples() {
        // Far below n_recent=60 → WarmingUp
        let s = state_with_graph(&[10.0, 10.0]);
        assert_eq!(s.mtr_trend(1000), MtrTrend::WarmingUp);
    }

    #[test]
    fn trend_flat_warming_up() {
        // total=90 < n_recent*2=120: baseline.len()=30 < n_recent=60 → WarmingUp
        // even with a dramatic step change from 10ms → 50ms
        let mut v = vec![10.0f64; 30];
        v.extend_from_slice(&[50.0f64; 60]);
        let s = state_with_graph(&v); // total=90
        assert_eq!(s.mtr_trend(1000), MtrTrend::WarmingUp);
    }

    #[test]
    fn trend_flat_stable_rtt() {
        // 120 samples all at 10ms → no change
        let s = state_with_graph(&[10.0; 120]);
        assert_eq!(s.mtr_trend(1000), MtrTrend::Flat);
    }

    #[test]
    fn trend_steep_up() {
        // Baseline 60×10ms, recent 60×50ms: rel = 40/10 = 400% → SteepUp
        let mut v = vec![10.0f64; 60];
        v.extend_from_slice(&[50.0f64; 60]);
        let s = state_with_graph(&v);
        assert_eq!(s.mtr_trend(1000), MtrTrend::SteepUp);
    }

    #[test]
    fn trend_gentle_up() {
        // Baseline 60×40ms, recent 60×46ms: rel = 6/40 = 15% → GentleUp
        let mut v = vec![40.0f64; 60];
        v.extend_from_slice(&[46.0f64; 60]);
        let s = state_with_graph(&v);
        assert_eq!(s.mtr_trend(1000), MtrTrend::GentleUp);
    }

    #[test]
    fn trend_steep_down() {
        // Baseline 60×50ms, recent 60×10ms: rel = 40/50 = 80% → SteepDown
        let mut v = vec![50.0f64; 60];
        v.extend_from_slice(&[10.0f64; 60]);
        let s = state_with_graph(&v);
        assert_eq!(s.mtr_trend(1000), MtrTrend::SteepDown);
    }

    #[test]
    fn trend_gentle_down() {
        // Baseline 60×46ms, recent 60×40ms: rel = 6/46 ≈ 13% → GentleDown
        let mut v = vec![46.0f64; 60];
        v.extend_from_slice(&[40.0f64; 60]);
        let s = state_with_graph(&v);
        assert_eq!(s.mtr_trend(1000), MtrTrend::GentleDown);
    }

    #[test]
    fn trend_all_drops() {
        // AllDrops detected on quick 10 s window, no history depth needed
        let s = state_with_drops(10);
        assert_eq!(s.mtr_trend(1000), MtrTrend::AllDrops);
    }

    #[test]
    fn trend_recent_better_than_baseline() {
        // Long degraded baseline (1000ms), then 60 s recovery (20ms) → SteepDown
        let mut s = TargetState::new("test".to_string());
        for _ in 0..200 { s.graph_history.push(Sample::Hit(1000.0)); }
        for _ in 0..60  { s.graph_history.push(Sample::Hit(20.0));   }
        assert_eq!(s.mtr_trend(1000), MtrTrend::SteepDown);
    }

    #[test]
    fn trend_stable_recent_matches_baseline() {
        // Both windows at the same level → Flat
        let mut s = TargetState::new("test".to_string());
        for _ in 0..200 { s.graph_history.push(Sample::Hit(20.0)); }
        for _ in 0..60  { s.graph_history.push(Sample::Hit(20.0)); }
        assert_eq!(s.mtr_trend(1000), MtrTrend::Flat);
    }

    #[test]
    fn trend_baseline_cap_ignores_ancient_history() {
        // Very old degraded history (2000 samples at 100ms), then a stable recent period.
        // The baseline cap (5×60=300 samples) should only look at the 300 samples before
        // recent, which are at 100ms - so recent 60ms vs baseline 100ms → SteepDown.
        // Without the cap this might accidentally compare against ancient 200ms data.
        let mut s = TargetState::new("test".to_string());
        for _ in 0..2000 { s.graph_history.push(Sample::Hit(200.0)); } // ancient, outside cap
        for _ in 0..300  { s.graph_history.push(Sample::Hit(100.0)); } // baseline window
        for _ in 0..60   { s.graph_history.push(Sample::Hit(60.0));  } // recent
        // baseline is capped to the 300 samples at 100ms (not the 2000 at 200ms)
        // rel = 40/100 = 40% → SteepDown
        assert_eq!(s.mtr_trend(1000), MtrTrend::SteepDown);
    }

    // --- flush_to_graph ---

    #[test]
    fn flush_records_peak_when_hits_arrived() {
        let mut s = TargetState::new("test".to_string());
        s.waiting = false;
        // Two hits arrived since last flush.
        s.record_sent(0); s.record_result(0, Ok(30.0), BIG_WINDOW, false);
        s.record_sent(1); s.record_result(1, Ok(200.0), BIG_WINDOW, false);
        s.flush_to_graph(100);
        assert_eq!(s.graph_history.last(), Some(&Sample::Hit(200.0)),
                   "flush must record the interval peak, not just the latest");
    }

    #[test]
    fn flush_records_drop_when_only_drops_arrived() {
        let mut s = TargetState::new("test".to_string());
        s.waiting = false;
        s.record_sent(0); s.record_result(0, Err(()), BIG_WINDOW, false);
        s.flush_to_graph(100);
        assert_eq!(s.graph_history.last(), Some(&Sample::Drop));
    }

    #[test]
    fn flush_records_pending_when_nothing_arrived() {
        let mut s = TargetState::new("test".to_string());
        s.waiting = false;
        // No record_result calls between init and flush - graph must not fabricate
        // a Hit from last_rtt.
        s.flush_to_graph(100);
        assert_eq!(s.graph_history.last(), Some(&Sample::Pending),
                   "intervals with no probe events must record Pending, not a duplicate");
    }

    #[test]
    fn flush_pending_does_not_fabricate_after_prior_hit() {
        // A hit lands in interval 1 - flush records Hit. No probe completes in
        // interval 2 - flush must record Pending (not repeat the Hit).
        let mut s = TargetState::new("test".to_string());
        s.waiting = false;
        s.record_sent(0); s.record_result(0, Ok(30.0), BIG_WINDOW, false);
        s.flush_to_graph(100);
        s.flush_to_graph(100);
        assert_eq!(s.graph_history.len(), 2);
        assert_eq!(s.graph_history[0], Sample::Hit(30.0));
        assert_eq!(s.graph_history[1], Sample::Pending,
                   "second flush has no events - must be Pending");
    }

    #[test]
    fn flush_drop_does_not_persist_across_intervals() {
        // A drop lands in interval 1 → Drop; nothing in interval 2 → Pending.
        let mut s = TargetState::new("test".to_string());
        s.waiting = false;
        s.record_sent(0); s.record_result(0, Err(()), BIG_WINDOW, false);
        s.flush_to_graph(100);
        s.flush_to_graph(100);
        assert_eq!(s.graph_history[0], Sample::Drop);
        assert_eq!(s.graph_history[1], Sample::Pending,
                   "drop flag must reset on flush - it should not echo into the next interval");
    }

    #[test]
    fn flush_hit_takes_precedence_over_drop_in_same_interval() {
        let mut s = TargetState::new("test".to_string());
        s.waiting = false;
        s.record_sent(0); s.record_result(0, Err(()), BIG_WINDOW, false);
        s.record_sent(1); s.record_result(1, Ok(50.0), BIG_WINDOW, false);
        s.flush_to_graph(100);
        assert_eq!(s.graph_history.last(), Some(&Sample::Hit(50.0)),
                   "any hit in the interval beats a drop - peak semantics");
    }
}

/// The stats window: what leaves it, when, and that every window stat (and
/// the sort keys) only see what's left.
#[cfg(test)]
mod window_tests {
    use super::*;
    use std::time::Duration;

    const WIN: u64 = 60;

    /// A target fed results at given offsets (seconds) from a fixed start.
    struct Feed { s: TargetState, t0: Instant, seq: usize, win: u64 }
    impl Feed {
        fn with_window(win: u64) -> Self { Feed { win, ..Feed::new() } }
        fn new() -> Self {
            // Start far enough in the past that resize_window (which uses the
            // real clock) sees these samples as old.
            let t0 = Instant::now().checked_sub(Duration::from_secs(1_000)).expect("monotonic clock too young");
            Feed { s: TargetState::new("t".into()), t0, seq: 0, win: WIN }
        }
        fn at(&self, secs: u64) -> Instant { self.t0 + Duration::from_secs(secs) }
        fn put(&mut self, secs: u64, r: Result<f64, ()>) {
            let now = self.at(secs);
            self.s.record_sent(self.seq);
            self.s.record_result_at(now, self.seq, r, self.win, false);
            self.seq += 1;
        }
        fn jittery(&mut self, from: u64, n: u64) { for i in from..from + n { self.put(i, Ok(if i % 2 == 0 { 5.0 } else { 25.0 })); } }
        fn steady(&mut self, from: u64, n: u64) { for i in from..from + n { self.put(i, Ok(10.0)); } }
    }

    #[test]
    fn a_down_target_ages_out_while_only_drops_arrive() {
        let mut f = Feed::new();
        f.steady(0, 30);
        for i in 100..300 { f.put(i, Err(())); }
        // Only the last 60s of drops remain; the replies are long gone.
        assert!(f.s.window.is_empty(), "stale replies kept: {}", f.s.window.len());
        assert!(f.s.jitter_window.is_empty());
        assert_eq!(f.s.win_drops, 60);
        assert_eq!(f.s.win_loss_pct(), 100.0);
        assert_eq!(f.s.win_jitter_pct(), None);
    }

    #[test]
    fn the_tick_ages_samples_out_when_nothing_arrives() {
        let mut f = Feed::new();
        f.jittery(0, 30);
        assert!(f.s.grade.quality.jittery);
        f.s.expire_window_and_regrade(f.at(29 + WIN - 1), WIN);
        assert_eq!(f.s.window.len(), 1, "only the last reply is still inside");
        f.s.expire_window_and_regrade(f.at(29 + WIN), WIN);
        assert!(f.s.window.is_empty() && f.s.jitter_window.is_empty());
        assert!(!f.s.grade.quality.jittery, "re-graded once its data left");
    }

    #[test]
    fn a_jump_across_a_gap_leaves_with_its_earlier_reply() {
        let mut f = Feed::new();
        f.put(0, Ok(25.0));
        f.steady(100, 30); // first one jumps 25 -> 10 across a reply that's expired
        assert_eq!(f.s.jitter_window.len(), 29, "N replies in the window -> N-1 jitters");
        assert_eq!(f.s.win_jitter_avg(), 0.0);
    }

    #[test]
    fn every_window_stat_only_sees_the_window() {
        // A minute of wild 5/25ms with a drop, then a minute of steady 10ms.
        let mut f = Feed::new();
        f.jittery(0, 30);
        f.put(30, Err(()));
        f.steady(100, 40);
        let s = &f.s;
        assert_eq!(s.window.len(), 40);
        for (name, v) in [
            ("avg", s.win_avg()), ("min", s.win_min()), ("max", s.win_max()), ("p01", s.win_p01()),
            ("p10", s.win_p10()), ("p50", s.win_median()), ("p95", s.win_p95()), ("p99", s.win_p99()),
            ("mtr", s.win_mtr().unwrap()),
        ] { assert_eq!(v, 10.0, "{name}"); }
        for (name, v) in [
            ("std", s.win_stddev()), ("cv", s.win_cv()), ("jitter", s.win_jitter_avg()),
            ("jitpct", s.win_jitter_pct().unwrap()), ("loss", s.win_loss_pct()),
        ] { assert_eq!(v, 0.0, "{name}"); }
        assert_eq!((s.win_drops, s.win_drop_event_times.len()), (0, 0));
        assert!(!s.grade.quality.jittery);
        // ...while lifetime still remembers all of it.
        assert!(s.life_cv() > 0.0 && s.life_max() == 25.0 && s.drops == 1);
    }

    #[test]
    fn resize_window_trims_everything_including_jitter() {
        let mut f = Feed::new();
        // Real-clock offsets: the last 10 results are within ~20s of now.
        f.jittery(0, 30);
        f.steady(980, 10);
        f.s.resize_window(30, 1_000);
        assert_eq!(f.s.window.len(), 10);
        assert_eq!(f.s.jitter_window.len(), 9);
        assert_eq!(f.s.win_jitter_avg(), 0.0);
    }

    #[test]
    fn quality_and_jitpct_see_the_whole_window_not_just_recent_replies() {
        // 2000 wild replies then 1000 steady ones, all inside a 1h window:
        // the wild majority still sets the medians.
        let mut s = TargetState::new("t".into());
        let t0 = Instant::now();
        for i in 0..3_000usize {
            let r = if i >= 2_000 { 10.0 } else if i % 2 == 0 { 5.0 } else { 25.0 };
            s.record_sent(i);
            s.record_result_at(t0 + Duration::from_millis(i as u64 * 100), i, Ok(r), 3_600, false);
        }
        s.regrade_at(t0 + Duration::from_secs(300 + 30)); // past any refresh spacing; all still in the 1h window
        assert_eq!(s.medians.n, 3_000);
        assert!(s.win_jitter_pct().unwrap() > 50.0, "{:?}", s.win_jitter_pct());
        assert!(s.grade.quality.jittery);
    }

    #[test]
    fn long_windows_refresh_medians_on_a_spacing_that_grows_with_size() {
        use crate::constants::{GRADE_MEDIANS_FULL_N as FULL, GRADE_MEDIANS_MAX_REFRESH_SECS as MAX};
        let mut s = TargetState::new("t".into());
        let t0 = Instant::now();
        let n = FULL * 2; // -> refreshed at most every ~2s
        let at = |i: usize| t0 + Duration::from_millis(i as u64);
        for i in 0..n { s.record_sent(i); s.record_result_at(at(i), i, Ok(10.0), 3_600, false); }
        let before = s.medians;
        for i in n..n + 50 { s.record_sent(i); s.record_result_at(at(i), i, Ok(500.0), 3_600, false); }
        assert_eq!(s.medians, before, "cached, even though the data moved");
        s.regrade_at(at(n) + Duration::from_secs(1));
        assert_eq!(s.medians, before, "~2s spacing at 2x the full-sort size");
        s.regrade_at(at(n) + Duration::from_secs(3));
        assert_eq!(s.medians.n, n + 50, "caught up");
        // Spacing is capped, however big it gets.
        assert!(super::medians_stale(10_000_000, Some(t0), t0 + Duration::from_secs_f64(MAX)));
        assert!(!super::medians_stale(10_000_000, Some(t0), t0 + Duration::from_secs_f64(MAX - 1.0)));
    }

    #[test]
    fn lifetime_jitpct_covers_the_whole_run_while_the_grade_stays_recent() {
        // Wild 5/25ms early (long expired from the 60s window), steady 10ms now.
        let mut f = Feed::new();
        f.jittery(0, 200);
        f.steady(300, 40);
        let s = &f.s;
        assert_eq!(s.win_jitter_pct(), Some(0.0), "window: only the steady minute");
        let life = s.life_jitter_pct().unwrap();
        assert!(life > 50.0, "lifetime remembers the wild part: {life}");
        assert!(s.jitter_over_threshold(false) && !s.jitter_over_threshold(true), "lifetime colouring uses the lifetime value");
        assert!(!s.grade.quality.jittery, "the grade judges recent behaviour only");
        use crate::cli::SortMode as M;
        assert_eq!(s.sort_stat(&M::Jitpct, false), Some(life));
        assert_eq!(s.sort_stat(&M::Jitpct, true), Some(0.0));
    }

    #[test]
    fn lifetime_mode_grades_over_the_short_lifetime_grade_window() {
        use crate::constants::GRADE_SPAN_SECS as W;
        let mut s = TargetState::new("t".into());
        let t0 = Instant::now();
        for i in 0..(W + 50) as usize {
            s.record_sent(i);
            s.record_result_at(t0 + Duration::from_secs(i as u64), i, Ok(10.0), 0, false);
        }
        assert_eq!(s.window.len() as u64, W, "window 0 keeps the last {W}s for the grades");
        assert_eq!(s.life_rtt_hist.len(), W + 50);
    }

    #[test]
    fn the_text_column_note_matches_the_lifetime_grade_window() {
        let mins = crate::constants::GRADE_SPAN_SECS / 60;
        assert!(crate::ui::dialogs::TEXT_COLUMNS_WINDOW_NOTE.contains(&format!("last {mins}m")));
    }

    #[test]
    fn lifetime_avg_std_cv_and_mtr_are_over_replies_not_results() {
        // 10ms and 30ms alternating, with every 5th probe lost (20% loss).
        let mut f = Feed::new();
        for i in 0..100u64 { f.put(i, if i % 5 == 4 { Err(()) } else if i % 2 == 0 { Ok(10.0) } else { Ok(30.0) }); }
        let s = &f.s;
        assert_eq!(s.life_replies(), 80);
        assert!((s.avg_latency() - 20.0).abs() < 1e-9, "{}", s.avg_latency());
        assert!((s.life_stddev() - 10.0).abs() < 1e-9, "{}", s.life_stddev());
        assert!((s.life_cv() - 50.0).abs() < 1e-9, "{}", s.life_cv());
        assert!((s.life_mtr().unwrap() - 25.0).abs() < 1e-9, "avg / (1 - 0.2)");
    }

    #[test]
    fn cached_window_percentiles_match_a_fresh_sort_after_expiry() {
        let mut f = Feed::new();
        for i in 0..150u64 { f.put(i, Ok(((i * 37) % 101) as f64 + 0.5)); }
        f.s.expire_window_and_regrade(f.at(170), WIN); // 20s more of the window gone
        let s = &f.s;
        let mut v: Vec<f64> = s.window.iter().map(|&(_, x)| x).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pick = |p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
        assert_eq!(v.len(), 39);
        for (p, got) in [(0.01, s.win_p01()), (0.10, s.win_p10()), (0.95, s.win_p95()), (0.99, s.win_p99())] {
            assert_eq!(got, pick(p), "p{p}");
        }
        assert_eq!(s.win_median(), v[v.len() / 2]);
    }

    #[test]
    fn with_a_long_window_the_grades_judge_only_the_last_few_minutes() {
        use crate::constants::GRADE_SPAN_SECS as SPAN;
        // 1h window: jittery and lossy for 10 minutes, then steady for the last 5.
        let mut f = Feed::with_window(3_600);
        for i in 0..600u64 { f.put(i, if i % 10 == 0 { Err(()) } else if i % 2 == 0 { Ok(5.0) } else { Ok(25.0) }); }
        f.steady(600, SPAN);
        let s = &f.s;
        // The window numbers still see all of it...
        assert!(s.win_drops > 0 && s.win_loss_pct() > 0.0);
        assert!(s.win_jitter_pct().unwrap() > 0.0 && s.win_p99() == 25.0);
        // ...the grades and the status text only the last GRADE_SPAN_SECS.
        // (the first steady reply's jitter pairs with one from before the span)
        assert_eq!(s.span, SpanCounts { replies: SPAN as usize, jitters: SPAN as usize - 1, ..Default::default() });
        assert_eq!(s.grade.loss, LossGrade::Normal);
        assert!(!s.grade.quality.jittery);
        assert_eq!(s.grade_medians.med, 10.0);
    }

    #[test]
    fn the_tick_regrades_when_samples_leave_the_span_but_not_the_window() {
        let mut f = Feed::with_window(3_600);
        f.steady(0, 100);
        for i in 100..110 { f.put(i, Err(())); } // a burst of drops
        f.steady(110, 20);
        assert!(f.s.grade.loss != LossGrade::Normal, "lossy right after the burst");
        f.s.expire_window_and_regrade(f.at(110 + crate::constants::GRADE_SPAN_SECS), 3_600);
        assert_eq!(f.s.win_drops, 10, "still in the 1h window");
        assert_eq!((f.s.span.drops, f.s.grade.loss), (0, LossGrade::Normal), "but out of the grading span");
    }

    #[test]
    fn sort_keys_follow_the_window_setting_like_the_columns() {
        use crate::cli::SortMode as M;
        let mut f = Feed::new();
        f.jittery(0, 30);
        f.steady(100, 40);
        let s = &f.s;
        assert_eq!(s.sort_stat(&M::Cv, true), Some(s.win_cv()));
        assert_eq!(s.sort_stat(&M::Cv, false), Some(s.life_cv()));
        assert_ne!(s.sort_stat(&M::Cv, true), s.sort_stat(&M::Cv, false));
        assert_eq!(s.sort_stat(&M::P99, true), Some(10.0));
        assert_eq!(s.sort_stat(&M::P99, false), Some(25.0));
        assert_eq!(s.sort_stat(&M::Jitter, false), Some(s.avg_jitter()));
        assert_eq!(s.sort_stat(&M::Jitpct, false), s.life_jitter_pct());
        assert_eq!(s.sort_stat(&M::Jitpct, true), s.win_jitter_pct());
        assert_eq!(TargetState::new("x".into()).sort_stat(&M::Avg, true), None, "nothing to rank yet");
    }
}
