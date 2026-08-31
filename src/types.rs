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

use crate::cli::PingMode;

pub struct ResolvedTarget {
    pub ip:               std::net::IpAddr,
    pub label:            String,
    pub label_is_custom:  bool,
    pub hostname:         Option<String>,
    pub mode:             PingMode,
    pub port:             u16,
    pub interval:         Option<u64>,
    pub timeout:          Option<f64>,
    pub resolve_interval: Option<u64>,
    pub http_path:        Option<String>,
    pub exec_cmd:         Option<String>,
}

/// One slot in the probe history timeline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Sample {
    Pending,      // probe sent, no result yet
    Drop,         // probe timed out
    Hit(f64),     // probe succeeded with this RTT (ms)
}

impl Sample {
    pub fn rtt(&self) -> Option<f64> {
        if let Sample::Hit(ms) = self { Some(*ms) } else { None }
    }
    pub fn is_drop(&self)    -> bool { matches!(self, Sample::Drop) }
    pub fn is_pending(&self) -> bool { matches!(self, Sample::Pending) }
}

/// Fired when a probe is sent; advances the timeline before the result arrives.
pub struct ProbeStarted {
    pub task_id: usize,
    pub seq:     usize,
}

pub struct ProbeResult {
    pub task_id:        usize,
    pub seq:            usize,
    pub outcome:        Result<f64, ()>,
    pub dup:            bool,
    pub bytes_sent:     u64,
    pub bytes_received: u64,
}

pub struct ResolveResult {
    pub indices:   Vec<usize>,
    pub new_ip:    Option<std::net::IpAddr>,
    pub new_label: String,
}

/// Result of the initial (startup) async DNS resolution for one or more slots
/// that share the same hostname.
pub enum InitResolveResult {
    Ok { slots: Vec<usize>, ip: std::net::IpAddr, hostname: Option<String>, label: String },
    Err { slots: Vec<usize>, msg: String },
}
