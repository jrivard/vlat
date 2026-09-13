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
