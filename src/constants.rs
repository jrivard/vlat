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

// CLI defaults
pub const DEFAULT_TCP_PORT: u16       = 80;
pub const DEFAULT_UDP_PORT: u16       = 33434;
pub const DEFAULT_DNS_PORT: u16       = 53;
pub const DEFAULT_TLS_PORT: u16       = 443;
pub const DEFAULT_NTP_PORT: u16       = 123;
pub const DEFAULT_SSH_PORT: u16       = 22;
pub const DEFAULT_SMTP_PORT: u16      = 25;
pub const DEFAULT_SMTPS_PORT: u16     = 465;
pub const DEFAULT_QUIC_PORT: u16      = 443;
pub const DEFAULT_DNS_QUERY: &str     = "example.net";

// Probe timeout ceiling - applies to all probe types
pub const MAX_PROBE_TIMEOUT_SECS: f64 = 60.0;

// Maximum number of targets vlat will probe in one run, after expanding any
// IP range / CIDR target specs. Applies regardless of how the targets were
// specified (explicit list, range, or CIDR block).
pub const MAX_TARGETS: usize = 256;

// App timing
pub const UI_TICK_MS: u64             = 1000;
pub const FAST_TICK_MS: u64           = 200;
pub const WARNING_DISMISS_SECS: u64   = 5;
pub const HELP_DISMISS_SECS: u64      = 60;

// Sessions
pub const SESSION_MAX_UNNAMED: usize  = 10;      // unnamed sessions kept; named are exempt
pub const SESSION_SAVE_SECS: u64      = 60;      // periodic session-save interval

// --summary-json: how often the summary snapshot file is rewritten when no
// explicit --summary-json-interval is given.
pub const SUMMARY_JSON_DEFAULT_SECS: u64 = 5;

// Status text: how long without a failure before a target that has had losses
// reads plain "ok · no loss for T" again, instead of "ok now · N lost, ...".
pub const STATUS_ALL_CLEAR_SECS: u64 = 3_600;

// Status grading (state.rs `StatusGrade`, worded in ui/status_text.rs). Each
// grade has an ENTER threshold and a lower STAY threshold (hysteresis), so a
// value hovering near a boundary doesn't flip the word back and forth.
//
// Loss grades use window loss %, once the window holds enough resolved probes.
pub const GRADE_LOSS_MIN_SAMPLES: usize = 20;
pub const GRADE_LOSSY_ENTER_PCT: f64      = 5.0;
pub const GRADE_LOSSY_STAY_PCT: f64       = 3.0;
pub const GRADE_STRUGGLING_ENTER_PCT: f64 = 25.0;
pub const GRADE_STRUGGLING_STAY_PCT: f64  = 20.0;
// An outage is this many consecutive drops; 2+ outages in the window = flapping.
pub const GRADE_OUTAGE_MIN_DROPS: u32     = 2;
pub const GRADE_FLAPPING_OUTAGES: usize   = 2;
// Quality grades use (at most) the most recent replies in the window.
pub const GRADE_QUALITY_MIN_SAMPLES: usize = 10;
pub const GRADE_QUALITY_MAX_SAMPLES: usize = 600;
// slow: window median vs --warn-rtt (enter at it, stay above this fraction of it).
pub const GRADE_SLOW_STAY_FRAC: f64       = 0.9;
// degraded: window median vs the target's own slow-moving baseline, both as a
// ratio and an absolute gap (so 0.3ms -> 0.7ms on a LAN isn't "degraded").
pub const GRADE_BASELINE_TAU_SECS: f64    = 3_600.0;
pub const GRADE_DEGRADED_ENTER_RATIO: f64 = 2.0;
pub const GRADE_DEGRADED_STAY_RATIO: f64  = 1.5;
pub const GRADE_DEGRADED_ENTER_MS: f64    = 5.0;
pub const GRADE_DEGRADED_STAY_MS: f64     = 3.0;
// jittery: median probe-to-probe jitter vs median RTT.
pub const GRADE_JITTERY_ENTER_RATIO: f64  = 0.5;
pub const GRADE_JITTERY_STAY_RATIO: f64   = 0.35;
pub const GRADE_JITTERY_ENTER_MS: f64     = 2.0;
pub const GRADE_JITTERY_STAY_MS: f64      = 1.5;
// spiky: p99 vs median RTT - mostly fine, with occasional big outliers.
pub const GRADE_SPIKY_ENTER_RATIO: f64    = 4.0;
pub const GRADE_SPIKY_STAY_RATIO: f64     = 3.0;
pub const GRADE_SPIKY_ENTER_MS: f64       = 20.0;
pub const GRADE_SPIKY_STAY_MS: f64        = 15.0;
// Trends (improving / worsening): the last RECENT_SECS of the stats window vs
// the rest of it, each side needing MIN_SAMPLES resolved probes. Only clear
// changes count (both a ratio and an absolute gap), with hysteresis.
pub const GRADE_TREND_RECENT_SECS: f64     = 60.0;
pub const GRADE_TREND_MIN_SAMPLES: usize   = 10;
pub const GRADE_LOSS_TREND_ENTER_PP: f64   = 5.0;  // percentage points
pub const GRADE_LOSS_TREND_STAY_PP: f64    = 2.0;
pub const GRADE_LOSS_TREND_ENTER_RATIO: f64 = 1.5;
pub const GRADE_LATENCY_TREND_ENTER_RATIO: f64 = 1.25; // of the median
pub const GRADE_LATENCY_TREND_STAY_RATIO: f64  = 1.1;
pub const GRADE_LATENCY_TREND_ENTER_MS: f64    = 5.0;
pub const GRADE_LATENCY_TREND_STAY_MS: f64     = 3.0;

// Window / graph-width limits
pub const WINDOW_MIN_SECS: u64        = 10;
pub const WINDOW_MAX_SECS: u64        = 86_400; // 24 h

// Animation
pub const GRAPH_ANIM_SECS: f64        = 1.5;
pub const BAR_ANIM_SECS:   f64        = 6.0;
pub const SCALE_DECREASE_HOLD_SECS: f64 = 30.0;

// Latency coloring tiers (pct deviation from recent p95)
pub const TIER_FAST_PCT:  f64 = -0.75; // below this → fast (green)
pub const TIER_HIGH_PCT:  f64 =  0.75; // above this → high (red)

// UI layout
pub const SINGLE_HISTORY_ROWS: u16    = 0;   // history rows shown above stats in single view (adjust with Up/Down)
pub const DIALOG_ROWS: u16            = 12;
pub const SORT_ARROW_SECS:    u64     = 5;
pub const FREEZE_NOTICE_SECS: u64     = 5;
pub const SORT_NOTICE_SECS:   u64     = 5;
pub const THEME_NOTICE_SECS:  u64     = 5;
pub const VIEW_NOTICE_SECS:   u64     = 5;
pub const THEME_LABEL_SECS:   u64     = 3;
