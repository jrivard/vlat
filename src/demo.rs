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
//!
//! Each target has a steady `Profile` (its normal character) plus *episodes*:
//! temporary changes that ramp up, hold, and ramp back down - a latency climb,
//! a jitter burst, a loss ramp, a run of outages, or a calm spell. Without
//! them every target would be stationary, and nothing that depends on change
//! (trends, `degraded`, `ok now`, hysteresis) could ever show.
//!
//! - One target (`--demo-count 1`) runs the fixed [`TOUR`], which walks
//!   through every status state in a known order.
//! - Several targets each draw random episodes from their own mix, scaled to
//!   their character and out of step with each other.

use crate::time::{SystemTime, UNIX_EPOCH};

/// Per-target synthetic latency behavior - stands in for a real probe.
#[derive(Clone, Copy, Debug)]
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
    /// Fixed seed, for reproducible runs (tests).
    pub fn from_seed(seed: u64) -> Self { Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1) }

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
    pub script:   Script,
}

/// A temporary change to a target's behavior, at full strength. Episodes
/// scale it by an intensity between 0 and 1 (see [`Episode`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    /// No change - a pause between episodes.
    Quiet,
    /// Loss and outages fade toward zero - an improving spell.
    Calm,
    /// Extra +/- ms of random noise.
    Jitter(f64),
    /// Multiplies the base RTT.
    Latency(f64),
    /// Extra per-probe drop probability.
    Loss(f64),
    /// Occasional large spikes.
    Spikes { chance: f64, mult: f64 },
    /// Runs of consecutive drops.
    Outages { chance: f64, min: u32, max: u32 },
}

/// An effect over time: ramps up over `ramp_secs`, holds for `hold_secs`,
/// then ramps down over `ramp_secs`. A ramp of 0 switches it on and off.
#[derive(Clone, Copy, Debug)]
pub struct Episode {
    pub effect:    Effect,
    pub ramp_secs: f64,
    pub hold_secs: f64,
}

impl Episode {
    const fn new(effect: Effect, ramp_secs: f64, hold_secs: f64) -> Self { Self { effect, ramp_secs, hold_secs } }
    pub fn len_secs(&self) -> f64 { self.ramp_secs * 2.0 + self.hold_secs }

    /// Strength (0..=1) `t` seconds into the episode.
    pub fn intensity(&self, t: f64) -> f64 {
        let (r, h) = (self.ramp_secs, self.hold_secs);
        if t < 0.0 || t >= self.len_secs() { 0.0 }
        else if r <= 0.0 || (t >= r && t < r + h) { 1.0 }
        else if t < r { t / r }
        else { (self.len_secs() - t) / r }
    }
}

/// `profile` with `effect` applied at strength `e` (0..=1).
pub fn apply(profile: Profile, effect: Effect, e: f64) -> Profile {
    let mut p = profile;
    match effect {
        Effect::Quiet => {}
        Effect::Calm => { p.loss *= 1.0 - e; p.outage_chance *= 1.0 - e; }
        Effect::Jitter(ms) => p.jitter += ms * e,
        Effect::Latency(mult) => p.base *= 1.0 + (mult - 1.0) * e,
        Effect::Loss(extra) => p.loss += extra * e,
        Effect::Spikes { chance, mult } => {
            if e > 0.0 { p.spike_chance = p.spike_chance.max(chance * e); p.spike_mult = p.spike_mult.max(mult); }
        }
        Effect::Outages { chance, min, max } => {
            if e > 0.0 { p.outage_chance += chance * e; p.outage_min = min; p.outage_max = max; }
        }
    }
    p
}

/// How a target's episodes are chosen.
#[derive(Clone, Copy, Debug)]
pub enum Script {
    /// The fixed [`TOUR`], looped.
    Tour,
    /// Random episodes drawn from `mix`, with a quiet gap of `gap_secs`
    /// (min, max) between them.
    Random { mix: &'static [Effect], gap_secs: (f64, f64) },
}

/// The single-target tour: each step is designed to reach particular status
/// states, in this order, with the demo's default 2m stats window
/// (`DEMO_WINDOW_SECS`). Takes about 17 minutes, then loops. The test
/// `tour_reaches_every_status_state` checks that it still does.
pub const TOUR: &[Episode] = &[
    // ok · no loss · N sent over T
    Episode::new(Effect::Quiet, 0.0, 60.0),
    // ok, jittery
    Episode::new(Effect::Jitter(20.0), 5.0, 100.0),
    Episode::new(Effect::Quiet, 0.0, 30.0),
    // ok, degraded, worsening -> ok, degraded -> ok, degraded, improving
    Episode::new(Effect::Latency(3.0), 45.0, 75.0),
    Episode::new(Effect::Quiet, 0.0, 30.0),
    // recovering -> flaky (scattered single drops)
    Episode::new(Effect::Loss(0.03), 0.0, 90.0),
    // lossy, worsening -> struggling -> lossy, improving
    Episode::new(Effect::Loss(0.45), 45.0, 75.0),
    // ... -> recovering -> ok now, once the window clears
    Episode::new(Effect::Quiet, 0.0, 130.0),
    // flapping, with down between outages
    Episode::new(Effect::Outages { chance: 0.03, min: 4, max: 8 }, 0.0, 120.0),
    // ... -> ok now
    Episode::new(Effect::Quiet, 0.0, 130.0),
];

/// The stats window `--demo` uses when `--window` isn't given: short enough
/// that trends, `ok now` and the tour's states show within minutes.
pub const DEMO_WINDOW_SECS: u64 = 120;

/// One target's running simulation: its profile and script, plus where it
/// is in its current episode.
pub struct DemoModel {
    profile:        Profile,
    script:         Script,
    rng:            Rng,
    interval_secs:  f64,
    down_remaining: u32,
    episode:        Option<Episode>,
    /// Seconds into the current episode (or gap).
    t:              f64,
    /// Random script: seconds of quiet left before the next episode.
    gap_left:       f64,
    tour_idx:       usize,
}

impl DemoModel {
    pub fn new(profile: Profile, script: Script, interval_ms: u64, mut rng: Rng) -> Self {
        // Start part-way into a gap so several targets don't all start an
        // episode at the same moment.
        let gap_left = match script {
            Script::Random { gap_secs: (lo, hi), .. } => rng.range(lo, hi) * rng.range(0.1, 0.6),
            Script::Tour => 0.0,
        };
        let episode = matches!(script, Script::Tour).then(|| TOUR[0]);
        Self { profile, script, rng, interval_secs: interval_ms as f64 / 1000.0, down_remaining: 0, episode, t: 0.0, gap_left, tour_idx: 0 }
    }

    /// The effect active right now, if any (for tests and debugging).
    pub fn current_effect(&self) -> Option<Effect> { self.episode.map(|e| e.effect) }

    /// Outcome of the next probe, then advance one interval.
    pub fn next_outcome(&mut self) -> Result<f64, ()> {
        let profile = match self.episode {
            Some(ep) => apply(self.profile, ep.effect, ep.intensity(self.t)),
            None => self.profile,
        };
        let outcome = decide(&profile, &mut self.rng, &mut self.down_remaining);
        self.advance();
        outcome
    }

    fn advance(&mut self) {
        let dt = self.interval_secs;
        match self.script {
            Script::Tour => {
                self.t += dt;
                if self.t >= TOUR[self.tour_idx].len_secs() {
                    self.tour_idx = (self.tour_idx + 1) % TOUR.len();
                    self.episode = Some(TOUR[self.tour_idx]);
                    self.t = 0.0;
                }
            }
            Script::Random { mix, gap_secs: (lo, hi) } => match self.episode {
                Some(ep) => {
                    self.t += dt;
                    if self.t >= ep.len_secs() {
                        self.episode = None;
                        self.gap_left = self.rng.range(lo, hi);
                    }
                }
                None => {
                    self.gap_left -= dt;
                    if self.gap_left <= 0.0 && !mix.is_empty() {
                        let effect = mix[(self.rng.f64() * mix.len() as f64) as usize % mix.len()];
                        let ramp = self.rng.range(20.0, 60.0);
                        let hold = self.rng.range(40.0, 120.0);
                        self.episode = Some(Episode::new(effect, ramp, hold));
                        self.t = 0.0;
                    }
                }
            },
        }
    }
}

/// Curated roster spanning the latency/loss spectrum, tuned by eye against
/// every view - home router (near-instant, near-perfect) down to a flaky
/// branch relay (slow, lossy, frequent outages). Order matters: slicing the
/// first N keeps a sensible spread for any target count.
fn curated() -> [(&'static str, &'static str, &'static str, Profile, Script); 5] {
    [
        ("home-router", "192.168.1.1", "icmp", Profile {
            base: 1.2, jitter: 0.4, loss: 0.0005, spike_chance: 0.005, spike_mult: 4.0,
            outage_chance: 0.0005, outage_min: 2, outage_max: 4,
        }, Script::Random { gap_secs: (240.0, 600.0), mix: &[
            Effect::Jitter(5.0), Effect::Spikes { chance: 0.05, mult: 25.0 }, Effect::Loss(0.02),
        ] }),
        ("dns-resolver", "10.0.0.53", "udp", Profile {
            base: 3.0, jitter: 0.8, loss: 0.001, spike_chance: 0.01, spike_mult: 3.0,
            outage_chance: 0.001, outage_min: 2, outage_max: 5,
        }, Script::Random { gap_secs: (180.0, 480.0), mix: &[
            Effect::Jitter(8.0), Effect::Latency(3.0), Effect::Loss(0.04), Effect::Spikes { chance: 0.04, mult: 12.0 },
        ] }),
        ("app-server", "172.16.8.20", "https", Profile {
            base: 34.0, jitter: 6.0, loss: 0.01, spike_chance: 0.03, spike_mult: 2.5,
            outage_chance: 0.004, outage_min: 3, outage_max: 8,
        }, Script::Random { gap_secs: (120.0, 360.0), mix: &[
            Effect::Latency(2.5), Effect::Loss(0.10), Effect::Outages { chance: 0.03, min: 3, max: 8 }, Effect::Jitter(30.0),
        ] }),
        ("office-vpn", "10.0.4.1", "tcp", Profile {
            base: 118.0, jitter: 20.0, loss: 0.02, spike_chance: 0.05, spike_mult: 2.0,
            outage_chance: 0.006, outage_min: 4, outage_max: 10,
        }, Script::Random { gap_secs: (120.0, 300.0), mix: &[
            Effect::Latency(2.5), Effect::Loss(0.12), Effect::Outages { chance: 0.03, min: 4, max: 10 }, Effect::Spikes { chance: 0.06, mult: 4.0 },
        ] }),
        ("branch-relay", "172.31.255.1", "udp", Profile {
            base: 260.0, jitter: 45.0, loss: 0.12, spike_chance: 0.1, spike_mult: 1.8,
            outage_chance: 0.01, outage_min: 5, outage_max: 12,
        }, Script::Random { gap_secs: (90.0, 240.0), mix: &[
            Effect::Calm, Effect::Calm, Effect::Loss(0.25), Effect::Outages { chance: 0.04, min: 5, max: 12 },
        ] }),
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
/// as distinct targets. A single target is instead the tour target: a
/// steady web API running the fixed [`TOUR`].
pub fn demo_targets(n: usize) -> Vec<DemoTarget> {
    if n <= 1 {
        return vec![DemoTarget {
            label: "web-api".to_string(), addr: "172.16.8.40".to_string(), mode_tag: "https",
            profile: Profile {
                base: 20.0, jitter: 1.5, loss: 0.0, spike_chance: 0.0, spike_mult: 1.0,
                outage_chance: 0.0, outage_min: 0, outage_max: 0,
            },
            script: Script::Tour,
        }];
    }
    let roster = curated();
    (0..n)
        .map(|i| {
            let (label, addr, mode_tag, profile, script) = roster[i % roster.len()];
            let cycle = i / roster.len();
            let label = if cycle == 0 { label.to_string() } else { format!("{label}-{}", cycle + 1) };
            let addr  = if cycle == 0 { addr.to_string() } else { bump_addr(addr, cycle as u8) };
            DemoTarget { label, addr, mode_tag, profile, script }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TargetState;
    use crate::ui::status_text::StatusSummary;
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// Runs `secs` of one demo target through a real `TargetState` on a
    /// simulated 1s clock with the demo window, returning the verdict slot
    /// ("lossy, worsening", ...) seen at each second.
    fn simulate(target: &DemoTarget, seed: u64, secs: usize) -> Vec<String> {
        let mut model = DemoModel::new(target.profile, target.script, 1_000, Rng::from_seed(seed));
        let mut s = TargetState::new(target.label.clone());
        let t0 = crate::time::Instant::now();
        (0..secs).map(|i| {
            let now = t0 + Duration::from_secs(i as u64);
            s.record_sent_at(now, i);
            s.record_result_at(now, i, model.next_outcome(), DEMO_WINDOW_SECS, false);
            StatusSummary::from_state(&s, now, 1_000).parts()[0].clone()
        }).collect()
    }

    #[test]
    fn tour_reaches_every_status_state() {
        let tour_secs: f64 = TOUR.iter().map(Episode::len_secs).sum();
        let target = &demo_targets(1)[0];
        let wanted = [
            "ok", "ok, jittery", "ok, degraded, worsening", "ok, degraded, improving",
            "recovering", "flaky", "lossy, worsening", "struggling", "lossy, improving",
            "flapping", "down", "ok now",
        ];
        for seed in 1..=5 {
            let seen: BTreeSet<String> = simulate(target, seed, tour_secs as usize + 30).into_iter().collect();
            // A trend word may be attached ("flapping, improving").
            let shown = |w: &str| [w.to_string(), format!("{w}, improving"), format!("{w}, worsening")].iter().any(|v| seen.contains(v));
            let missing: Vec<&str> = wanted.iter().copied().filter(|w| !shown(w)).collect();
            assert!(missing.is_empty(), "seed {seed}: tour never showed {missing:?}; saw {seen:?}");
        }
    }

    #[test]
    fn tour_shows_states_in_order() {
        // First appearance of each milestone, in tour order.
        let order = ["ok, jittery", "ok, degraded, worsening", "flaky", "struggling", "flapping", "ok now"];
        let tour_secs: f64 = TOUR.iter().map(Episode::len_secs).sum();
        let seen = simulate(&demo_targets(1)[0], 7, tour_secs as usize);
        let firsts: Vec<usize> = order.iter()
            .map(|w| seen.iter().position(|v| v == w || v.starts_with(&format!("{w}, "))).unwrap_or_else(|| panic!("never saw {w:?}")))
            .collect();
        assert!(firsts.windows(2).all(|p| p[0] < p[1]), "out of order: {:?}", order.iter().zip(&firsts).collect::<Vec<_>>());
    }

    #[test]
    fn roster_targets_change_over_time_and_keep_their_character() {
        // Over an hour, each roster target should show more than one state, and
        // the home router should still be the healthiest.
        let targets = demo_targets(5);
        let mut kinds = Vec::new();
        for t in &targets {
            let seen: BTreeSet<String> = simulate(t, 3, 3_600).into_iter().collect();
            assert!(seen.len() > 1, "{} never changed state: {seen:?}", t.label);
            kinds.push(seen);
        }
        let bad = |set: &BTreeSet<String>| set.iter().filter(|v| v.starts_with("struggling") || v.starts_with("flapping")).count();
        assert!(bad(&kinds[0]) <= bad(&kinds[4]), "home-router should be no worse than branch-relay");
    }

    #[test]
    fn episode_intensity_ramps_holds_and_ramps_down() {
        let e = Episode::new(Effect::Loss(0.1), 10.0, 20.0);
        assert_eq!(e.len_secs(), 40.0);
        assert_eq!(e.intensity(0.0), 0.0);
        assert_eq!(e.intensity(5.0), 0.5);
        assert_eq!(e.intensity(15.0), 1.0);
        assert_eq!(e.intensity(35.0), 0.5);
        assert_eq!(e.intensity(40.0), 0.0);
        let step = Episode::new(Effect::Quiet, 0.0, 5.0);
        assert_eq!(step.intensity(0.0), 1.0);
    }
}
