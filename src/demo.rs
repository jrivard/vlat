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

//! Synthetic probe data - stands in for a real probe when there's no network
//! to measure, or none is wanted. Used by the native `--demo` flag
//! (`probe::spawn_demo_task`).

use crate::time::{SystemTime, UNIX_EPOCH};

/// Per-target synthetic latency behavior - stands in for a real probe.
#[derive(Clone, Copy)]
pub struct Profile {
    pub base:         f64, // baseline RTT, ms
    pub jitter:       f64, // +/- ms of random noise
    pub loss:         f64, // single-packet drop probability per probe, 0..1
    pub spike_chance: f64, // probability of a latency spike
    pub spike_mult:   f64, // spike multiplier on base RTT

    // Sustained outages - separate from single-packet `loss` - so the UP/DOWN
    // status badge (vlat::ui::widgets::build_status_badge_spans) has something
    // to actually demonstrate instead of only ever showing brief single-probe
    // blips.
    pub outage_chance: f64, // probability per probe, while up, of an outage starting
    pub outage_min:    u32, // shortest outage, in consecutive dropped probes
    pub outage_max:    u32, // longest outage, in consecutive dropped probes
}

/// Tiny xorshift64 PRNG - avoids pulling in `rand`/`getrandom` for synthetic
/// data that doesn't warrant it.
pub struct Rng(u64);

impl Rng {
    /// Seed from wall-clock time via the `crate::time` shim.
    pub fn seeded() -> Self {
        Self::seeded_with(0)
    }

    /// Seed from wall-clock time mixed with `salt`, so e.g. several targets
    /// spawned at nearly the same instant (coarse clock resolution) still
    /// diverge instead of moving in lockstep.
    pub fn seeded_with(salt: u64) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEAD_BEEF_1337_CAFE);
        Self((nanos ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform float in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.f64() * (hi - lo)
    }
}

/// One synthetic target: display label, fake (never dialed) address, short
/// protocol tag matching app::mode_label_str's form ("icmp"/"tcp"/"udp"/...),
/// and the behavior profile driving its simulated probes.
pub struct DemoTarget {
    pub label:    String,
    pub addr:     String,
    pub mode_tag: &'static str,
    pub profile:  Profile,
}

/// Curated roster spanning the latency/loss spectrum, tuned by eye against
/// every view - home router (near-instant, near-perfect) down to a flaky
/// branch relay (slow, lossy, frequent outages). Order matters: slicing the
/// first N keeps a sensible spread for any target count.
fn curated() -> [(&'static str, &'static str, &'static str, Profile); 5] {
    [
        ("home-router", "192.168.1.1", "icmp", Profile {
            base: 1.2, jitter: 0.4, loss: 0.0005, spike_chance: 0.005, spike_mult: 4.0,
            outage_chance: 0.0005, outage_min: 2, outage_max: 4,
        }),
        ("dns-resolver", "10.0.0.53", "udp", Profile {
            base: 3.0, jitter: 0.8, loss: 0.001, spike_chance: 0.01, spike_mult: 3.0,
            outage_chance: 0.001, outage_min: 2, outage_max: 5,
        }),
        ("app-server", "172.16.8.20", "https", Profile {
            base: 34.0, jitter: 6.0, loss: 0.01, spike_chance: 0.03, spike_mult: 2.5,
            outage_chance: 0.004, outage_min: 3, outage_max: 8,
        }),
        ("office-vpn", "10.0.4.1", "tcp", Profile {
            base: 118.0, jitter: 20.0, loss: 0.02, spike_chance: 0.05, spike_mult: 2.0,
            outage_chance: 0.006, outage_min: 4, outage_max: 10,
        }),
        ("branch-relay", "172.31.255.1", "udp", Profile {
            base: 260.0, jitter: 45.0, loss: 0.12, spike_chance: 0.1, spike_mult: 1.8,
            outage_chance: 0.01, outage_min: 5, outage_max: 12,
        }),
    ]
}

/// Bump an IPv4 literal's last octet by `bump` (clamped away from .0/.255)
/// so repeated cycles through the curated roster get visually distinct fake
/// addresses instead of literal duplicates. Falls back to the input
/// unchanged if it isn't a plain dotted-quad (shouldn't happen - `curated()`
/// only uses dotted-quads).
fn bump_addr(addr: &str, bump: u8) -> String {
    let mut octets: Vec<u8> = addr.split('.').filter_map(|p| p.parse().ok()).collect();
    if octets.len() != 4 {
        return addr.to_string();
    }
    octets[3] = 1 + (octets[3] as u32 + bump as u32 - 1).rem_euclid(254) as u8;
    format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])
}

/// Build `n` synthetic targets by cycling the curated roster (`n` can be
/// more or fewer than the roster's own length). Repeats past the first pass
/// get a numbered label suffix and a bumped fake address so they still read
/// as distinct targets.
pub fn demo_targets(n: usize) -> Vec<DemoTarget> {
    let roster = curated();
    let n = n.max(1);
    (0..n)
        .map(|i| {
            let (label, addr, mode_tag, profile) = roster[i % roster.len()];
            let cycle = i / roster.len();
            let label = if cycle == 0 { label.to_string() } else { format!("{label}-{}", cycle + 1) };
            let addr  = if cycle == 0 { addr.to_string() } else { bump_addr(addr, cycle as u8) };
            DemoTarget { label, addr, mode_tag, profile }
        })
        .collect()
}

/// Decide the outcome of one simulated probe: a sustained outage in
/// progress takes priority over single-packet loss (so status badges show
/// real downtime, not just isolated blips), then a fresh outage may start,
/// then ordinary per-packet loss; otherwise a latency is drawn, occasionally
/// spiked. `down_remaining` is this target's outage countdown, carried by
/// the caller across ticks.
pub fn decide(profile: &Profile, rng: &mut Rng, down_remaining: &mut u32) -> Result<f64, ()> {
    let dropped = if *down_remaining > 0 {
        *down_remaining -= 1;
        true
    } else if rng.f64() < profile.loss {
        true
    } else if rng.f64() < profile.outage_chance {
        let len = rng.range(profile.outage_min as f64, profile.outage_max as f64 + 1.0) as u32;
        *down_remaining = len.saturating_sub(1);
        true
    } else {
        false
    };

    if dropped {
        Err(())
    } else {
        let spike = rng.f64() < profile.spike_chance;
        let mult  = if spike { profile.spike_mult } else { 1.0 };
        let noise = rng.range(-profile.jitter, profile.jitter);
        Ok((profile.base * mult + noise).max(0.1))
    }
}
