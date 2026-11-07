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

//! Bounded log-bucket histogram for lifetime percentiles and medians.
//!
//! Keeping every lifetime sample costs memory forever and a full sort per
//! query; this keeps one entry per occupied bucket (at most ~1,900, typically
//! a few hundred for real RTTs), O(log buckets) per insert and one pass per
//! query, however long the run. Buckets grow by [`GROWTH`] (1%) and each
//! reports the mean of the values in it, so a percentile is within 1% of the
//! true one - and exact whenever its bucket holds a single repeated value
//! (common: steady RTTs repeat). The very first and last ranks are the exact
//! min and max. Saved with the session, so restored lifetime percentiles
//! cover the whole session.

use serde::{Deserialize, Serialize};

/// Values below this (ms) share bucket 0.
pub const LOW_MS: f64 = 0.001;
/// Values above this (ms) share the last bucket.
pub const HIGH_MS: f64 = 120_000.0;
/// Ratio between consecutive bucket bounds.
pub const GROWTH: f64 = 1.01;

fn bucket_count() -> usize { 2 + ((HIGH_MS / LOW_MS).ln() / GROWTH.ln()).ceil() as usize }

fn bucket_of(v: f64) -> u16 {
    if v.is_nan() || v < LOW_MS { return 0; }
    (1 + ((v / LOW_MS).ln() / GROWTH.ln()) as usize).min(bucket_count() - 1) as u16
}

/// One occupied bucket: index, how many values, and their sum (for the mean).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Bucket { k: u16, n: u32, sum: f64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogHistogram {
    /// Occupied buckets, sorted by index.
    #[serde(default, rename = "b")]
    buckets: Vec<Bucket>,
    #[serde(default)]
    n: u64,
    #[serde(default)]
    min: f64,
    #[serde(default)]
    max: f64,
}

impl LogHistogram {
    pub fn new() -> Self { Self::default() }
    pub fn len(&self) -> u64 { self.n }
    pub fn is_empty(&self) -> bool { self.n == 0 }

    pub fn insert(&mut self, v: f64) {
        let k = bucket_of(v);
        match self.buckets.binary_search_by_key(&k, |b| b.k) {
            Ok(i)  => { let b = &mut self.buckets[i]; b.n = b.n.saturating_add(1); b.sum += v; }
            Err(i) => self.buckets.insert(i, Bucket { k, n: 1, sum: v }),
        }
        if self.n == 0 { self.min = v; self.max = v; } else { self.min = self.min.min(v); self.max = self.max.max(v); }
        self.n += 1;
    }

    /// The value at 0-based `rank` in sorted order (see module docs for accuracy).
    fn at_rank(&self, rank: u64) -> f64 {
        if rank == 0 { return self.min; }
        if rank + 1 >= self.n { return self.max; }
        let mut seen = 0u64;
        for b in &self.buckets {
            seen += b.n as u64;
            if seen > rank { return (b.sum / b.n as f64).clamp(self.min, self.max); }
        }
        self.max
    }

    /// Same rank rule as the exact window percentiles: floor(n * pct).
    pub fn percentile(&self, pct: f64) -> f64 {
        if self.n == 0 { return 0.0; }
        self.at_rank(((self.n as f64 * pct) as u64).min(self.n - 1))
    }

    pub fn median(&self) -> f64 {
        match self.n {
            0 => 0.0,
            n if n % 2 == 1 => self.at_rank(n / 2),
            n => (self.at_rank(n / 2 - 1) + self.at_rank(n / 2)) / 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(v: &[f64], pct: f64) -> f64 {
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[((s.len() as f64 * pct) as usize).min(s.len() - 1)]
    }

    #[test]
    fn percentiles_are_within_one_percent_and_exact_at_the_ends() {
        let vals: Vec<f64> = (0..10_000).map(|i| 0.2 + ((i * 7919) % 10_007) as f64 * 0.37).collect();
        let mut h = LogHistogram::new();
        for &v in &vals { h.insert(v); }
        for p in [0.01, 0.10, 0.50, 0.95, 0.99] {
            let (got, want) = (h.percentile(p), exact(&vals, p));
            assert!((got / want - 1.0).abs() <= 0.01, "p{p}: {got} vs {want}");
        }
        assert_eq!(h.percentile(0.0), vals.iter().cloned().fold(f64::MAX, f64::min));
        assert_eq!(h.percentile(1.0), vals.iter().cloned().fold(f64::MIN, f64::max));
    }

    #[test]
    fn steady_and_two_valued_series_come_out_exact() {
        let mut h = LogHistogram::new();
        for _ in 0..100 { h.insert(10.0); }
        assert_eq!((h.median(), h.percentile(0.99)), (10.0, 10.0));
        let mut h = LogHistogram::new();
        for i in 0..100 { h.insert(if i % 2 == 0 { 5.0 } else { 25.0 }); }
        assert_eq!((h.percentile(0.01), h.percentile(0.99), h.median()), (5.0, 25.0, 15.0));
    }

    #[test]
    fn zero_huge_and_empty_values_are_safe() {
        let mut h = LogHistogram::new();
        assert_eq!((h.median(), h.percentile(0.5)), (0.0, 0.0));
        h.insert(0.0);
        h.insert(1e9);
        assert_eq!((h.percentile(0.0), h.percentile(1.0)), (0.0, 1e9));
    }

    #[test]
    fn round_trips_through_its_sparse_session_form() {
        let mut h = LogHistogram::new();
        for i in 0..500 { h.insert(1.0 + i as f64 * 0.3); }
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.len() < 12_000, "sparse: {} bytes", json.len());
        let back: LogHistogram = serde_json::from_str(&json).unwrap();
        // serde_json may round a sum's last bit; the stats must not move.
        assert_eq!((back.len(), back.buckets.len()), (h.len(), h.buckets.len()));
        for p in [0.0, 0.01, 0.5, 0.95, 0.99, 1.0] {
            assert!((back.percentile(p) - h.percentile(p)).abs() < 1e-9, "p{p}");
        }
        assert!(h.buckets.len() < 400, "{} buckets", h.buckets.len());
        assert_eq!(serde_json::from_str::<LogHistogram>("{}").unwrap(), LogHistogram::new());
    }
}
