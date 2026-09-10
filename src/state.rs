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
    pub lifetime_rtts:  Vec<f64>,
    pub history:        Vec<Sample>,
    pub pending_map:    HashMap<usize, usize>, // seq → history index
    pub early_results:  HashSet<usize>,        // seqs where result arrived before start
    pub jitter_window:  Vec<f64>,
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
}

impl TargetState {
    pub fn new(label: String) -> Self {
        Self {
            host:           label.clone(),
            label,
            custom_label:   false,
            exec_cmd:       String::new(),
            window:         Vec::new(),
            lifetime_rtts:  Vec::new(),
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
        }
    }

    /// Called when a probe is sent. Advances the timeline immediately.
    pub fn record_sent(&mut self, seq: usize) {
        // If the result already arrived (race: ProbeResult before ProbeStarted), skip.
        if self.early_results.remove(&seq) { return; }
        self.pending_map.insert(seq, self.history.len());
        self.history.push(Sample::Pending);
        self.circle_history.push(0);
    }

    /// Called when a probe result arrives (hit or drop).
    pub fn record_result(&mut self, seq: usize, outcome: Result<f64, ()>, window_secs: u64, dup: bool) {
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

        let effective_window = if window_secs == 0 { 300 } else { window_secs };

        match outcome {
            Ok(rtt_ms) => {
                self.last_was_drop = false;
                let now = Instant::now();
                self.last_up = Some(now);

                // Compute p95 of the last 20 window entries BEFORE adding the current sample,
                // so the new probe is scored against prior behavior, not its own influence.
                let pre_push_len = self.window.len();
                let recent_p95   = self.recent_p95_n(20);

                self.window.push((now, rtt_ms));
                self.lifetime_rtts.push(rtt_ms);
                self.window.retain(|(t, _)| now.duration_since(*t) < Duration::from_secs(effective_window));
                let before = self.win_drop_times.len();
                self.win_drop_times.retain(|t| now.duration_since(*t) < Duration::from_secs(effective_window));
                self.win_drops = self.win_drops.saturating_sub((before - self.win_drop_times.len()) as u32);
                let before_dup = self.win_dup_times.len();
                self.win_dup_times.retain(|t| now.duration_since(*t) < Duration::from_secs(effective_window));
                self.win_dups = self.win_dups.saturating_sub((before_dup - self.win_dup_times.len()) as u32);

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
                    self.jitter_window.push(self.current_jitter);
                }
                let window_len = self.window.len();
                if self.jitter_window.len() > window_len {
                    self.jitter_window.drain(..self.jitter_window.len() - window_len);
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
                self.win_drop_times.push(Instant::now());
                self.history[idx] = Sample::Drop;
                if idx < self.circle_history.len() {
                    self.circle_history[idx] = 255; // drop sentinel - tier lookup not used for drops
                }
                self.cur_drop_streak += 1;
                if self.cur_drop_streak > self.max_drop_streak {
                    self.max_drop_streak = self.cur_drop_streak;
                }
            }
        }
    }

    pub fn win_min(&self) -> f64 { self.window.iter().map(|&(_, v)| v).fold(f64::MAX, f64::min) }
    pub fn win_max(&self) -> f64 { self.window.iter().map(|&(_, v)| v).fold(f64::MIN, f64::max) }
    pub fn win_avg(&self) -> f64 {
        if self.window.is_empty() { return 0.0; }
        self.window.iter().map(|(_, v)| v).sum::<f64>() / self.window.len() as f64
    }
    pub fn win_jitter_avg(&self) -> f64 {
        if self.jitter_window.is_empty() { return 0.0; }
        self.jitter_window.iter().sum::<f64>() / self.jitter_window.len() as f64
    }
    fn win_percentile(&self, pct: f64) -> f64 {
        if self.window.len() < 2 { return self.win_avg(); }
        let mut vals: Vec<f64> = self.window.iter().map(|&(_, v)| v).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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

    pub fn avg_latency(&self) -> f64 {
        if self.total_sent > 0 { self.latency_sum / self.total_sent as f64 } else { 0.0 }
    }
    pub fn avg_jitter(&self) -> f64 {
        if self.jitter_count > 0 { self.jitter_sum / self.jitter_count as f64 } else { 0.0 }
    }
    pub fn life_min(&self) -> f64 { if self.lifetime_min == f64::MAX { 0.0 } else { self.lifetime_min } }
    pub fn life_max(&self) -> f64 { if self.lifetime_max == f64::MIN { 0.0 } else { self.lifetime_max } }
    pub fn life_stddev(&self) -> f64 {
        let n = self.total_sent.saturating_sub(self.drops as u64);
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

    pub fn win_median(&self) -> f64 {
        if self.window.is_empty() { return 0.0; }
        if self.window.len() == 1 { return self.window[0].1; }
        let mut vals: Vec<f64> = self.window.iter().map(|&(_, v)| v).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = vals.len() / 2;
        if vals.len().is_multiple_of(2) { (vals[mid - 1] + vals[mid]) / 2.0 } else { vals[mid] }
    }

    fn life_percentile(&self, pct: f64) -> f64 {
        if self.lifetime_rtts.len() < 2 { return self.avg_latency(); }
        let mut vals = self.lifetime_rtts.clone();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((vals.len() as f64 * pct) as usize).min(vals.len() - 1);
        vals[idx]
    }
    pub fn life_p01(&self) -> f64 { self.life_percentile(0.01) }
    pub fn life_p10(&self) -> f64 { self.life_percentile(0.10) }
    pub fn life_p95(&self) -> f64 { self.life_percentile(0.95) }
    pub fn life_p99(&self) -> f64 { self.life_percentile(0.99) }
    pub fn life_median(&self) -> f64 {
        if self.lifetime_rtts.is_empty() { return 0.0; }
        if self.lifetime_rtts.len() == 1 { return self.lifetime_rtts[0]; }
        let mut vals = self.lifetime_rtts.clone();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = vals.len() / 2;
        if vals.len().is_multiple_of(2) { (vals[mid - 1] + vals[mid]) / 2.0 } else { vals[mid] }
    }

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


    /// Resize the rolling window to a new duration, preserving existing data that fits.
    /// Trims timestamps-based collections to the new window; trims graph_history to the new cap.
    /// Resets animation state but does NOT set calibrating — caller does that if needed.
    pub fn resize_window(&mut self, new_window_secs: u64, graph_max_entries: usize) {
        let now = Instant::now();
        let effective = if new_window_secs == 0 { 300 } else { new_window_secs };
        let cutoff = Duration::from_secs(effective);

        self.window.retain(|(t, _)| now.duration_since(*t) < cutoff);

        self.win_drop_times.retain(|t| now.duration_since(*t) < cutoff);
        self.win_drops = self.win_drop_times.len() as u32;

        self.win_dup_times.retain(|t| now.duration_since(*t) < cutoff);
        self.win_dups = self.win_dup_times.len() as u32;

        let window_len = self.window.len();
        if self.jitter_window.len() > window_len {
            self.jitter_window.drain(..self.jitter_window.len() - window_len);
        }

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
